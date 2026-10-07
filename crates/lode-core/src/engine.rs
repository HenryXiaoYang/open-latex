//! The persistent paragraph server: one `lualatex` process kept alive after `\begin{document}`.
//! Requests go down its stdin as JSON lines; responses come back as length-prefixed JSON frames
//! on a FIFO (stdout is not clean: the banner is printed even in batch mode — see ENGINE_NOTES).

use crate::texlive::TexLive;
use anyhow::{anyhow, bail, Context, Result};
use lode_dl::DisplayList;
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
    #[serde(default)]
    pub t_tex_us: i64,
    #[serde(default)]
    pub t_traverse_us: i64,
    #[serde(default)]
    pub t_pack_us: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op")]
pub enum Response {
    #[serde(rename = "ready")]
    Ready { banner: String, #[serde(default)] font_nextid: i64, #[serde(default)] fingerprint: String },
    #[serde(rename = "ok")]
    Ok { id: i64 },
    #[serde(rename = "result")]
    Result(CompileResult),
    #[serde(rename = "fatal")]
    Fatal { #[serde(default)] req: Option<i64>, reason: String, #[serde(default)] errors: Vec<EngineError> },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "stats")]
    Stats { requests: i64, font_nextid: i64, node_mem: String, grouplevel: i64, nest: i64, luastate: f64 },
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
    next_req: i64,
}

impl FastServer {
    /// Write the driver file and spawn the server. `preamble` is everything before
    /// `\begin{document}` of the project's main file; `cwd` is the project directory.
    pub fn spawn(tl: &TexLive, cwd: &Path, work_dir: &Path, preamble: &str, generation: u64) -> Result<FastServer> {
        std::fs::create_dir_all(work_dir)?;
        let work_dir = &work_dir.canonicalize()?;
        let driver = work_dir.join("lode-serve.tex");
        let preamble_file = work_dir.join("lode-preamble.tex");
        std::fs::write(&preamble_file, preamble)?;
        std::fs::write(
            &driver,
            format!(
                "\\input{{{}}}\n\\newbox\\lodebox\n\\begin{{document}}\n\\directlua{{lode_serve = dofile(kpse.find_file(\"lode-serve.lua\", \"lua\") or \"lode-serve.lua\") lode_serve.run(\\number\\lodebox)}}\n\\end{{document}}\n",
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
            .env("LODE_RESP", &fifo)
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
        self.work_dir.join("lode-serve.log")
    }

    fn send(&mut self, v: &serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()?;
        Ok(())
    }

    pub fn recv(&mut self) -> Result<Response> {
        let mut hdr = [0u8; 4];
        self.resp.read_exact(&mut hdr).context("server closed the response channel")?;
        let n = u32::from_le_bytes(hdr) as usize;
        let mut buf = vec![0u8; n];
        self.resp.read_exact(&mut buf)?;
        Ok(serde_json::from_slice(&buf).with_context(|| format!("bad response: {}", String::from_utf8_lossy(&buf[..buf.len().min(300)])))?)
    }

    pub fn set_context(&mut self, id: i64, ctx: &serde_json::Value) -> Result<()> {
        self.send(&serde_json::json!({"op": "context", "id": id, "ctx": ctx}))?;
        match self.recv()? {
            Response::Ok { .. } => Ok(()),
            other => bail!("set_context: {other:?}"),
        }
    }

    /// Compile one paragraph; blocking. Returns the result and host-side timing.
    pub fn compile(&mut self, ctx: i64, source: &str) -> Result<(CompileResult, RoundTrip)> {
        let req = self.next_req;
        self.next_req += 1;
        let t0 = Instant::now();
        self.send(&serde_json::json!({"op": "compile", "req": req, "ctx": ctx, "source": source}))?;
        let r = self.recv()?;
        let total = t0.elapsed();
        match r {
            Response::Result(cr) => {
                let rt = RoundTrip {
                    total,
                    t_tex: Duration::from_micros(cr.t_tex_us as u64),
                    t_traverse: Duration::from_micros(cr.t_traverse_us as u64),
                    t_pack: Duration::from_micros(cr.t_pack_us as u64),
                };
                Ok((cr, rt))
            }
            Response::Fatal { reason, errors, .. } => bail!("engine fatal: {reason} {errors:?}"),
            other => bail!("compile: unexpected {other:?}"),
        }
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
    let f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(fifo)?;
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
