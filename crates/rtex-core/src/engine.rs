//! The persistent paragraph server: one `lualatex` process kept alive after `\begin{document}`.
//! Requests go down its stdin as JSON lines; responses come back as length-prefixed JSON frames
//! on a FIFO (stdout is not clean: the banner is printed even in batch mode — see ENGINE_NOTES).

use crate::texlive::TexLive;
use anyhow::{anyhow, bail, Context, Result};
use rtex_dl::DisplayList;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read, Write};
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
    Result(CompileResult),
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
    stdin: std::process::ChildStdin,
    resp: BufReader<File>,
    pub generation: u64,
    pub work_dir: PathBuf,
    pub banner: String,
    pub startup: Duration,
    /// Per-request watchdog (default 5 s; the first compile of a session loads fonts and may take longer).
    pub timeout: Duration,
    /// How long to busy-poll for a reply before blocking (default 3 ms; zero disables).
    pub spin: Duration,
    next_req: i64,
    frame_buf: Vec<u8>,
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
        std::fs::create_dir_all(work_dir)?;
        let work_dir = &work_dir.canonicalize()?;
        let own_aux = work_dir.join(format!("rtex-serve-g{generation}.aux"));
        let _ = std::fs::remove_file(&own_aux);
        if let Some(a) = aux {
            if a.exists() {
                std::fs::copy(a, &own_aux)
                    .with_context(|| format!("copying {} for the server", a.display()))?;
            }
            // the bibliography data of that pass (biblatex reads \jobname.bbl at
            // \begin{document}; bibtex's .bbl is \input by \bibliography, in the body)
            for ext in ["bbl"] {
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
                preamble_file.display()
            ),
        )?;
        let fifo = work_dir.join(format!("resp-{}.fifo", std::process::id()));
        let _ = std::fs::remove_file(&fifo);
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).context("mkfifo")?;
        let mut cmd: Command = tl.lualatex_cmd(cwd);
        cmd.arg("-interaction=batchmode")
            .arg(format!("--output-directory={}", work_dir.display()))
            .arg(driver.as_os_str())
            .env("RTEX_RESP", &fifo)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let t0 = Instant::now();
        let mut child = cmd.spawn().context("spawning lualatex server")?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        // Opening the FIFO for reading blocks until the server opens it for writing. Poll the
        // child so a crash during the preamble does not hang us forever.
        let resp = open_fifo_with_timeout(&fifo, &mut child, Duration::from_secs(120))?;
        let mut s = FastServer {
            child,
            stdin,
            resp: BufReader::new(resp),
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
            frame_buf: Vec::with_capacity(1 << 16),
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

    fn send(&mut self, v: &serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Wait until a frame header is readable or `timeout` elapses (watchdog). On timeout the
    /// server is killed; the caller restarts it with a new generation.
    fn wait_readable(&mut self, timeout: Duration) -> Result<()> {
        use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
        use std::os::fd::AsFd;
        if self.resp.buffer().len() >= 4 {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        // Bounded busy-poll: the reply to a compile is expected within a few ms, and waking a
        // blocked thread costs tens of µs on most systems (more in VMs).
        if !self.spin.is_zero() {
            let spin_until = Instant::now() + self.spin;
            while Instant::now() < spin_until {
                let mut fds = [PollFd::new(self.resp.get_ref().as_fd(), PollFlags::POLLIN)];
                if poll(&mut fds, PollTimeout::ZERO)? > 0 {
                    return Ok(());
                }
                std::hint::spin_loop();
            }
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill();
                bail!(
                    "engine watchdog: no response within {:?}; server killed",
                    timeout
                );
            }
            let ms = remaining.as_millis().min(u16::MAX as u128) as u16;
            let mut fds = [PollFd::new(self.resp.get_ref().as_fd(), PollFlags::POLLIN)];
            let n = poll(&mut fds, PollTimeout::from(ms))?;
            if n > 0 {
                return Ok(());
            }
        }
    }

    pub fn recv(&mut self) -> Result<Response> {
        self.wait_readable(self.timeout)?;
        self.recv_timed()
    }

    fn recv_timed(&mut self) -> Result<Response> {
        let t0 = Instant::now();
        let mut hdr = [0u8; 4];
        self.resp
            .read_exact(&mut hdr)
            .context("server closed the response channel")?;
        let n = u32::from_le_bytes(hdr) as usize;
        let mut buf = std::mem::take(&mut self.frame_buf);
        buf.resize(n, 0);
        let res = self.resp.read_exact(&mut buf);
        let r = self.parse_frame(&buf, t0, res);
        self.frame_buf = buf;
        r
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
                Ok(Response::Result(cr))
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
    pub fn encode_compile(req: i64, ctx: i64, source: &str) -> Vec<u8> {
        let mut v = Vec::with_capacity(source.len() + 32);
        v.extend_from_slice(format!("C {req} {ctx} {}\n", source.len()).as_bytes());
        v.extend_from_slice(source.as_bytes());
        v
    }

    /// Next request id (shared counter for direct writers, see `Session`).
    pub fn next_req_id(&mut self) -> i64 {
        let r = self.next_req;
        self.next_req += 1;
        r
    }

    /// A second handle on the server's stdin so another thread can submit compile frames
    /// directly (the owner keeps reading results in order).
    pub fn stdin_clone(&self) -> Result<File> {
        use std::os::fd::{AsFd, OwnedFd};
        let fd: OwnedFd = self.stdin.as_fd().try_clone_to_owned()?;
        Ok(File::from(fd))
    }

    /// Submit a compile without waiting; the result is read with `recv`.
    pub fn send_compile(&mut self, req: i64, ctx: i64, source: &str) -> Result<()> {
        let frame = Self::encode_compile(req, ctx, source);
        self.stdin.write_all(&frame)?;
        Ok(())
    }

    /// Compile one paragraph; blocking. Returns the result and host-side timing.
    pub fn compile(&mut self, ctx: i64, source: &str) -> Result<(CompileResult, RoundTrip)> {
        let req = self.next_req_id();
        let t0 = Instant::now();
        self.send_compile(req, ctx, source)?;
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
                Ok((cr, rt))
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

fn open_fifo_with_timeout(fifo: &Path, child: &mut Child, timeout: Duration) -> Result<File> {
    use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
    use std::os::fd::AsFd;
    use std::os::unix::fs::OpenOptionsExt;
    // Open the read end once, non-blocking, so the open itself never blocks and the server's
    // own open(2) for writing succeeds as soon as it gets there. Never close and reopen:
    // a write into a FIFO without a reader would be lost (EPIPE on the server side).
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fifo)?;
    let t0 = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("lualatex server exited during startup with {status}");
        }
        if t0.elapsed() > timeout {
            bail!("timed out waiting for the server to open the response FIFO");
        }
        // Linux does not report POLLHUP for a FIFO that never had a writer, so poll blocks
        // until the first bytes ("ready" frame) arrive.
        let mut fds = [PollFd::new(f.as_fd(), PollFlags::POLLIN)];
        let n = poll(&mut fds, PollTimeout::from(50u16))?;
        if n > 0 {
            if let Some(ev) = fds[0].revents() {
                if ev.contains(PollFlags::POLLIN) {
                    break;
                }
                if ev.contains(PollFlags::POLLHUP) {
                    bail!("server closed the response FIFO during startup");
                }
            }
        }
    }
    // Back to blocking mode for normal framed reads.
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&f);
    let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL)?;
    let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
    oflags.remove(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(oflags))?;
    Ok(f)
}
