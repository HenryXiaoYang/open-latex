//! The persistent paragraph server: one `lualatex` process kept alive after `\begin{document}`.
//! Requests go to it as JSON lines; responses come back as length-prefixed JSON frames on a
//! channel of their own (stdout is not clean: the banner is printed even in batch mode — see
//! docs/engine-protocol.md). The channel is a FIFO on Unix and a pair of named pipes on Windows
//! (`transport`).

use crate::texlive::TexLive;
use crate::transport::{Frame, Transport};
use anyhow::{bail, Context, Result};
use rtex_dl::DisplayList;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EngineError {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub line: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompileResult {
    pub req: i64,
    #[serde(default)]
    pub ctx: Option<i64>,
    pub status: String,
    #[serde(default)]
    pub errors: Vec<EngineError>,
    #[serde(default)]
    pub dl: Option<DisplayList>,
    /// The display list as received (binary encoding revision 1), kept so hosts need no re-encoding.
    #[serde(skip)]
    pub dl_binary: Option<Vec<u8>>,
    #[serde(default)]
    pub dl_bytes: i64,
    #[serde(default)]
    pub t_tex_us: i64,
    #[serde(default)]
    pub t_traverse_us: i64,
    #[serde(default)]
    pub t_pack_us: i64,
    #[serde(default)]
    pub stages_us: serde_json::Value,
    /// Image resources used by the display list (index → file), from the server's graphicx hook.
    #[serde(default)]
    pub images: std::collections::BTreeMap<String, rtex_dl::ImageInfo>,
    /// Counters the compile advanced: name → value at the end (see rtex-serve.lua finish()).
    #[serde(default)]
    pub counters: std::collections::BTreeMap<String, i64>,
    /// Control sequences the source mentions whose meaning the compile changed (a definition
    /// or \let that leaked out of the unit's box).
    #[serde(default)]
    pub leaks: Vec<String>,
    /// Picture environments the compile began, and how many of them came from the picture
    /// cache (present when the request carried cache entries).
    #[serde(default)]
    pub pics_seen: Option<i64>,
    #[serde(default)]
    pub pics_used: Option<i64>,
    /// A Lua error in the server while handling the request (with its traceback): the result
    /// is an error the unit's text did not cause, sent instead of nothing so the host does not
    /// wait out the watchdog.
    #[serde(default)]
    pub internal: Option<String>,
    /// Host-side stage times (µs): send, wait, read, parse.
    #[serde(skip)]
    pub host_us: [u64; 4],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op")]
pub enum Response {
    #[serde(rename = "ready")]
    Ready {
        banner: String,
        #[serde(default)]
        font_nextid: i64,
        #[serde(default)]
        fingerprint: String,
    },
    #[serde(rename = "ok")]
    Ok { id: i64 },
    #[serde(rename = "result")]
    Result(Box<CompileResult>),
    #[serde(rename = "fatal")]
    Fatal {
        #[serde(default)]
        req: Option<i64>,
        reason: String,
        #[serde(default)]
        errors: Vec<EngineError>,
        #[serde(default)]
        before: String,
        #[serde(default)]
        after: String,
    },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "stats")]
    Stats {
        requests: i64,
        font_nextid: i64,
        node_mem: String,
        grouplevel: i64,
        nest: i64,
        luastate: f64,
    },
    #[serde(rename = "profile")]
    Profile {
        #[serde(default)]
        us: serde_json::Value,
        #[serde(default)]
        input_ptr: i64,
    },
    #[serde(rename = "bye")]
    Bye,
    #[serde(rename = "error")]
    Error { message: String },
}

/// Timing of one round trip as seen from the host.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct RoundTrip {
    pub total: Duration,
    pub t_tex: Duration,
    pub t_traverse: Duration,
    pub t_pack: Duration,
}

pub struct FastServer {
    child: Child,
    /// The request channel (the server's stdin on Unix, a named pipe on Windows).
    stdin: File,
    /// Frames from the response channel, in order, drained by a dedicated reader thread with
    /// blocking reads (see `transport`).
    frames: crossbeam_channel::Receiver<Frame>,
    /// A frame `wait_readable` took off the channel, not yet parsed.
    pending: Option<Frame>,
    pub generation: u64,
    pub work_dir: PathBuf,
    pub banner: String,
    pub startup: Duration,
    /// Per-request watchdog (default 5 s; the first compile of a session loads fonts and may take longer).
    pub timeout: Duration,
    /// How long to busy-poll for a reply before blocking (default 3 ms; zero disables).
    pub spin: Duration,
    next_req: i64,
}

impl FastServer {
    /// Write the driver file and spawn the server. `preamble` is everything before
    /// `\begin{document}` of the project's main file; `cwd` is the project directory.
    /// `aux`: the latest background pass's `.aux`, read by `\begin{document}` like in a real
    /// run so packages that decide their mode from it (natbib's author-year detection, hyperref)
    /// start in the document's state; labels are refreshed later through `set_labels`.
    pub fn spawn(
        tl: &TexLive,
        cwd: &Path,
        work_dir: &Path,
        preamble: &str,
        generation: u64,
        aux: Option<&Path>,
    ) -> Result<FastServer> {
        Self::spawn_with(tl, cwd, work_dir, preamble, generation, aux, false)
    }

    /// `spawn` with the server's own trace on (`rtex-serve-g<generation>.trace` in `work_dir`:
    /// one line-flushed entry per request stage, so a hang shows the last stage reached even
    /// when the TeX log's tail is lost with the killed process).
    pub fn spawn_with(
        tl: &TexLive,
        cwd: &Path,
        work_dir: &Path,
        preamble: &str,
        generation: u64,
        aux: Option<&Path>,
        trace: bool,
    ) -> Result<FastServer> {
        std::fs::create_dir_all(work_dir)?;
        let work_dir = &crate::paths::canonical(work_dir)?;
        let own_aux = work_dir.join(format!("rtex-serve-g{generation}.aux"));
        let _ = std::fs::remove_file(&own_aux);
        if let Some(a) = aux {
            if a.exists() {
                std::fs::copy(a, &own_aux)
                    .with_context(|| format!("copying {} for the server", a.display()))?;
            }
            // the bibliography data of that pass (biblatex reads \jobname.bbl at
            // \begin{document}; bibtex's .bbl is \input by \bibliography, in the body)
            {
                let ext = "bbl";
                let src = a.with_extension(ext);
                let dst = work_dir.join(format!("rtex-serve-g{generation}.{ext}"));
                let _ = std::fs::remove_file(&dst);
                if src.exists() {
                    std::fs::copy(&src, &dst)
                        .with_context(|| format!("copying {} for the server", src.display()))?;
                }
            }
        }
        // one driver/log per generation so a crashed server's log survives the restart
        let driver = work_dir.join(format!("rtex-serve-g{generation}.tex"));
        let preamble_file = work_dir.join("rtex-preamble.tex");
        std::fs::write(&preamble_file, preamble)?;
        std::fs::write(
            &driver,
            format!(
                concat!(
                    "\\input{{{}}}\n\\newbox\\rtexbox\\newcount\\rtexcontinue\\rtexcontinue=1 \\edef\\rtexcountnum{{\\number\\allocationnumber}}\\newcatcodetable\\rtexcct{{\\makeatletter\\savecatcodetable\\rtexcct}}\n\\begin{{document}}\n",
                    // server-only shortcuts (tex/latex/rtex-serve-patches.tex): marks, math font memo, graphics probes
                    "\\input{{rtex-serve-patches}}\n",
                    "\\directlua{{rtex_serve = dofile(kpse.find_file(\"rtex-serve.lua\", \"lua\") or \"rtex-serve.lua\") rtex_serve.init(\\number\\rtexbox, \\rtexcountnum, \\number\\rtexcct)}}\n",
                    // image resource index -> file mapping for IMAGE display-list items
                    "\\makeatletter\\IfPackageLoadedTF{{graphicx}}{{\\AddToHook{{cmd/Gin@setfile/after}}{{\\directlua{{rtex_serve.image(\\number\\lastsavedimageresourceindex,\"\\luaescapestring{{\\Gin@base\\Gin@ext}}\",\"\\luaescapestring{{\\Gin@page}}\",\\number\\lastsavedimageresourcepages)}}}}}}{{}}\\makeatother\n",
                    "\\loop\\rtexstep\\ifnum\\rtexcontinue>0 \\repeat\n\\end{{document}}\n"
                ),
                crate::paths::tex(&preamble_file)
            ),
        )?;
        let transport = Transport::create(work_dir)?;
        let mut cmd: Command = tl.lualatex_cmd(cwd);
        cmd.arg("-interaction=batchmode")
            .arg(format!(
                "--output-directory={}",
                crate::paths::tex(work_dir)
            ))
            .arg(crate::paths::tex(&driver))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        transport.configure(&mut cmd);
        if trace {
            cmd.env(
                "RTEX_TRACE",
                work_dir.join(format!("rtex-serve-g{generation}.trace")),
            );
        }
        let t0 = Instant::now();
        let mut child = cmd.spawn().context("spawning lualatex server")?;
        // Connecting waits until the server has opened the response channel and written its
        // first frame. It polls the child so a crash during the preamble does not hang us.
        let conn = match transport.connect(&mut child, Duration::from_secs(120)) {
            Ok(c) => c,
            Err(e) => {
                // the TeX log says why (a missing file, an error in the preamble, a Lua error
                // opening the channel); the exit status alone does not
                let log = work_dir.join(format!("rtex-serve-g{generation}.log"));
                return Err(match tex_log_excerpt(&log) {
                    Some(x) => anyhow::anyhow!("{e:#}; TeX log: {x}"),
                    None => e,
                });
            }
        };
        let mut s = FastServer {
            child,
            stdin: conn.requests,
            frames: conn.frames,
            pending: conn.first,
            generation,
            work_dir: work_dir.to_path_buf(),
            banner: String::new(),
            startup: Duration::ZERO,
            timeout: Duration::from_secs(5),
            spin: std::env::var("RTEX_SPIN_US")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_micros)
                .unwrap_or(Duration::from_millis(3)),
            next_req: 1,
        };
        match s.recv()? {
            Response::Ready { banner, .. } => {
                s.banner = banner;
                s.startup = t0.elapsed();
            }
            other => bail!("unexpected first response: {other:?}"),
        }
        Ok(s)
    }

    pub fn log_path(&self) -> PathBuf {
        self.work_dir
            .join(format!("rtex-serve-g{}.log", self.generation))
    }

    /// Files that describe this server (driver, preamble copy, TeX log, trace when on), for a
    /// debug bundle.
    pub fn debug_files(&self) -> Vec<PathBuf> {
        let g = self.generation;
        vec![
            self.work_dir.join(format!("rtex-serve-g{g}.tex")),
            self.work_dir.join("rtex-preamble.tex"),
            self.log_path(),
            self.work_dir.join(format!("rtex-serve-g{g}.trace")),
        ]
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn send(&mut self, v: &serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Wait until a frame has arrived or `timeout` elapses (watchdog). On timeout the server
    /// is killed; the caller restarts it with a new generation.
    fn wait_readable(&mut self, timeout: Duration) -> Result<()> {
        use crossbeam_channel::{RecvTimeoutError, TryRecvError};
        if self.pending.is_some() {
            return Ok(());
        }
        let ended = || {
            Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the response reader ended",
            ))
        };
        let deadline = Instant::now() + timeout;
        // Bounded busy-poll of the channel: the reply to a compile is expected within a few
        // ms, and waking a blocked thread costs tens of µs on most systems (more in VMs).
        if !self.spin.is_zero() {
            let spin_until = Instant::now() + self.spin;
            while Instant::now() < spin_until {
                match self.frames.try_recv() {
                    Ok(f) => {
                        self.pending = Some(f);
                        return Ok(());
                    }
                    // yield, not a pure spin: the reader thread that delivers the frame
                    // needs a core, and a busy host must not keep it waiting
                    Err(TryRecvError::Empty) => std::thread::yield_now(),
                    Err(TryRecvError::Disconnected) => {
                        self.pending = Some(ended());
                        return Ok(());
                    }
                }
            }
        }
        match self
            .frames
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(f) => {
                self.pending = Some(f);
                Ok(())
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.pending = Some(ended());
                Ok(())
            }
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                bail!(
                    "engine watchdog: no response within {:?}; server killed",
                    timeout
                );
            }
        }
    }

    pub fn recv(&mut self) -> Result<Response> {
        self.wait_readable(self.timeout)?;
        self.recv_timed()
    }

    fn recv_timed(&mut self) -> Result<Response> {
        let t0 = Instant::now();
        let frame = match self.pending.take() {
            Some(f) => f,
            None => self.frames.recv().unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the response reader ended",
                ))
            }),
        };
        let buf = frame.context("server closed the response channel")?;
        self.parse_frame(&buf, t0, Ok(()))
    }

    fn parse_frame(
        &mut self,
        buf: &[u8],
        t0: Instant,
        read: std::io::Result<()>,
    ) -> Result<Response> {
        read?;
        let t_read = t0.elapsed();
        if buf.is_empty() {
            bail!("empty frame");
        }
        match buf[0] {
            0 => Ok(serde_json::from_slice(&buf[1..]).with_context(|| {
                format!(
                    "bad response: {}",
                    String::from_utf8_lossy(&buf[1..buf.len().min(300)])
                )
            })?),
            1 => {
                if buf.len() < 5 {
                    bail!("short result frame");
                }
                let jl = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
                let json = &buf[5..5 + jl];
                let bin = &buf[5 + jl..];
                let mut cr: CompileResult = serde_json::from_slice(json).with_context(|| {
                    format!(
                        "bad result header: {}",
                        String::from_utf8_lossy(&json[..json.len().min(300)])
                    )
                })?;
                if !bin.is_empty() {
                    cr.dl = Some(
                        DisplayList::from_binary(bin).context("decoding binary display list")?,
                    );
                    cr.dl_binary = Some(bin.to_vec());
                }
                cr.host_us[2] = t_read.as_micros() as u64;
                cr.host_us[3] = (t0.elapsed() - t_read).as_micros() as u64;
                Ok(Response::Result(Box::new(cr)))
            }
            k => bail!("unknown frame kind {k}"),
        }
    }

    pub fn set_context(&mut self, id: i64, ctx: &serde_json::Value) -> Result<()> {
        self.send(&serde_json::json!({"op": "context", "id": id, "ctx": ctx}))?;
        match self.recv()? {
            Response::Ok { .. } => Ok(()),
            other => bail!("set_context: {other:?}"),
        }
    }

    /// Encode a compile request frame: `C <req> <ctx> <len>\n` followed by the raw source.
    /// `pics`: picture cache entries for the source's picture environments, in order, as a
    /// JSON array (empty string: none). They follow the source in the frame.
    pub fn encode_compile(req: i64, ctx: i64, source: &str, pics: &str) -> Vec<u8> {
        let mut v = Vec::with_capacity(source.len() + pics.len() + 40);
        if pics.is_empty() {
            v.extend_from_slice(format!("C {req} {ctx} {}\n", source.len()).as_bytes());
        } else {
            v.extend_from_slice(
                format!("C {req} {ctx} {} {}\n", source.len(), pics.len()).as_bytes(),
            );
        }
        v.extend_from_slice(source.as_bytes());
        v.extend_from_slice(pics.as_bytes());
        v
    }

    /// Next request id (shared counter for direct writers, see `Session`).
    pub fn next_req_id(&mut self) -> i64 {
        let r = self.next_req;
        self.next_req += 1;
        r
    }

    /// A second handle on the request channel so another thread can submit compile frames
    /// directly (the owner keeps reading results in order).
    pub fn stdin_clone(&self) -> Result<File> {
        Ok(self.stdin.try_clone()?)
    }

    /// Submit a compile without waiting; the result is read with `recv`.
    pub fn send_compile(&mut self, req: i64, ctx: i64, source: &str, pics: &str) -> Result<()> {
        let frame = Self::encode_compile(req, ctx, source, pics);
        self.stdin.write_all(&frame)?;
        Ok(())
    }

    /// Compile one paragraph; blocking. Returns the result and host-side timing.
    pub fn compile(&mut self, ctx: i64, source: &str) -> Result<(CompileResult, RoundTrip)> {
        self.compile_with_pics(ctx, source, "")
    }

    /// `compile` with picture cache entries (see `encode_compile`).
    pub fn compile_with_pics(
        &mut self,
        ctx: i64,
        source: &str,
        pics: &str,
    ) -> Result<(CompileResult, RoundTrip)> {
        let req = self.next_req_id();
        let t0 = Instant::now();
        self.send_compile(req, ctx, source, pics)?;
        let t_sent = t0.elapsed();
        self.wait_readable(self.timeout)?;
        let t_ready = t0.elapsed();
        let r = self.recv_timed()?;
        let total = t0.elapsed();
        match r {
            Response::Result(mut cr) => {
                cr.host_us = [
                    t_sent.as_micros() as u64,
                    (t_ready - t_sent).as_micros() as u64,
                    cr.host_us[2],
                    cr.host_us[3],
                ];
                let rt = RoundTrip {
                    total,
                    t_tex: Duration::from_micros(cr.t_tex_us as u64),
                    t_traverse: Duration::from_micros(cr.t_traverse_us as u64),
                    t_pack: Duration::from_micros(cr.t_pack_us as u64),
                };
                Ok((*cr, rt))
            }
            Response::Fatal {
                reason,
                errors,
                before,
                after,
                ..
            } => bail!("engine fatal: {reason} {errors:?} before=[{before}] after=[{after}]"),
            other => bail!("compile: unexpected {other:?}"),
        }
    }

    /// Define `\r@name`/`\b@key` macros from the last pass's aux so `\ref`/`\cite` resolve.
    pub fn set_labels(&mut self, labels: &[(String, String)]) -> Result<()> {
        self.send(&serde_json::json!({"op": "labels", "labels": labels}))?;
        match self.recv()? {
            Response::Ok { .. } => Ok(()),
            other => bail!("set_labels: {other:?}"),
        }
    }

    /// In-engine micro-profile of the replay machinery (diagnostic; uses tex.runtoks).
    pub fn profile(&mut self, ctx: i64, source: &str, n: usize) -> Result<Response> {
        self.send(
            &serde_json::json!({"op": "profile", "req": 0, "ctx": ctx, "source": source, "n": n}),
        )?;
        self.recv()
    }

    pub fn stats(&mut self) -> Result<Response> {
        self.send(&serde_json::json!({"op": "stats"}))?;
        self.recv()
    }

    pub fn ping(&mut self) -> Result<Duration> {
        let t0 = Instant::now();
        self.send(&serde_json::json!({"op": "ping"}))?;
        match self.recv()? {
            Response::Pong => Ok(t0.elapsed()),
            other => bail!("ping: {other:?}"),
        }
    }

    pub fn shutdown(&mut self) -> Result<()> {
        let _ = self.send(&serde_json::json!({"op": "shutdown"}));
        let _ = self.recv();
        // The Lua loop returns, TeX runs \end{document} and exits.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
        Ok(())
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for FastServer {
    fn drop(&mut self) {
        if self.is_alive() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The first TeX or Lua error in a log (its message and the lines up to the `l.<n>` line), else
/// its last lines; at most 400 characters on one line. None when there is no log.
fn tex_log_excerpt(log: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log).ok()?;
    // MiKTeX ignores max_print_line and wraps at 79 characters: join wrapped lines first
    let mut joined: Vec<String> = Vec::new();
    let mut wrapped = false;
    for l in text.lines() {
        match joined.last_mut() {
            Some(prev) if wrapped => prev.push_str(l),
            _ => joined.push(l.to_string()),
        }
        wrapped = l.chars().count() == 79;
    }
    let lines: Vec<&str> = joined.iter().map(|l| l.trim_end()).collect();
    // a TeX error starts with "!"; LuaTeX reports a Lua error as "<file>:<line>: message"
    // followed by "stack traceback:", with no "!"
    let first = lines.iter().enumerate().position(|(i, l)| {
        l.starts_with('!')
            || lines
                .get(i + 1)
                .is_some_and(|n| n.starts_with("stack traceback:"))
    });
    let picked: Vec<&str> = match first {
        Some(i) => {
            let end = lines[i..]
                .iter()
                .position(|l| l.starts_with("l."))
                .map(|j| i + j + 1)
                .unwrap_or(lines.len())
                .min(i + 6);
            lines[i..end].to_vec()
        }
        None => {
            let tail: Vec<&str> = lines
                .iter()
                .rev()
                .filter(|l| !l.is_empty())
                .take(3)
                .copied()
                .collect();
            tail.into_iter().rev().collect()
        }
    };
    let mut out = picked.join(" / ");
    if out.chars().count() > 400 {
        out = out.chars().take(400).collect::<String>() + "…";
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_errors_quote_the_first_tex_error() {
        let log = std::env::temp_dir().join(format!("rtex-excerpt-{}.log", std::process::id()));
        std::fs::write(
            &log,
            "This is LuaHBTeX\n(./rtex-serve-g1.tex\n! LaTeX Error: File `foo.sty' not found.\n\nType X to quit.\nl.3 \\usepackage{foo}\nmore\n",
        )
        .unwrap();
        let x = tex_log_excerpt(&log).unwrap();
        let _ = std::fs::remove_file(&log);
        assert_eq!(
            x,
            "! LaTeX Error: File `foo.sty' not found. /  / Type X to quit. / l.3 \\usepackage{foo}"
        );
        // a Lua error comes before the TeX errors it causes
        std::fs::write(
            &log,
            {
                // wrapped at 79 characters, as MiKTeX writes it
                let msg = format!(
                    "(./{}.tex)D:/rtex-serve.lua:712: cannot open response channel PIPE",
                    "y".repeat(40)
                );
                let (head, rest) = msg.split_at(79);
                format!("{head}\n{rest}\nstack traceback:\n\t[C]: in function 'assert'\nl.5 ...\n\n! Undefined control sequence.\nl.7 \\loop\n")
            },
        )
        .unwrap();
        let x = tex_log_excerpt(&log).unwrap();
        let _ = std::fs::remove_file(&log);
        assert!(x.contains("cannot open response channel PIPE"), "{x}");
    }
}
