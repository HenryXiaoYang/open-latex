//! One background pass: snapshot the buffers, run the instrumented full compile (with
//! biber/bibtex when the document asks for it), and return the capture.

use crate::capture::{run_capture, CaptureResult};
use crate::texlive::TexLive;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BibTool {
    Auto,
    Biber,
    BibTeX,
    None,
}

#[derive(Debug, Clone)]
pub struct PassOutcome {
    pub capture: CaptureResult,
    pub passes: u32,
    pub bib_ran: bool,
    pub aux_stable: bool,
}

impl Clone for CaptureResult {
    fn clone(&self) -> Self {
        CaptureResult { out_dir: self.out_dir.clone(), jobname: self.jobname.clone(), json: self.json.clone(), pdf: self.pdf.clone(), log: self.log.clone(), wall: self.wall, exit_ok: self.exit_ok }
    }
}

/// Write `files` (relative path → text) under `dir`, copying every other file from `project`
/// (images, .bib, .cls …) so relative inputs resolve.
pub fn write_snapshot(project: &Path, files: &BTreeMap<String, String>, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    copy_tree(project, dir, 0)?;
    for (rel, text) in files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, text)?;
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path, depth: usize) -> Result<()> {
    if depth > 8 {
        return Ok(());
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if name_s.starts_with('.') || name_s == "build" || name_s == "target" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &dst.join(&name), depth + 1)?;
        } else {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "pdf" | "aux" | "log" | "synctex.gz" | "fls" | "fdb_latexmk" | "out" | "toc" | "lof" | "lot" | "bbl" | "bcf" | "blg" | "run.xml") {
                continue;
            }
            let target = dst.join(&name);
            if !target.exists() || std::fs::metadata(&path)?.modified()? > std::fs::metadata(&target)?.modified()? {
                std::fs::copy(&path, &target)?;
            }
        }
    }
    Ok(())
}

fn aux_signature(out_dir: &Path, jobname: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    for ext in ["aux", "toc", "lof", "lot", "out", "bcf", "bbl", "idx"] {
        if let Ok(b) = std::fs::read(out_dir.join(format!("{jobname}.{ext}"))) {
            ext.hash(&mut h);
            b.hash(&mut h);
        }
    }
    h.finish()
}

fn log_requests_rerun(log: &Path) -> bool {
    std::fs::read_to_string(log).map(|s| s.contains("Rerun to get") || s.contains("rerun LaTeX") || s.contains("Please (re)run Biber") || s.contains("Please rerun LaTeX")).unwrap_or(false)
}

/// Run instrumented passes in `out_dir` until the aux family is stable or `max_passes` is hit.
pub fn run_pass(tl: &TexLive, snapshot_dir: &Path, main: &str, out_dir: &Path, max_passes: u32, bib: BibTool, instrumented: bool) -> Result<PassOutcome> {
    std::fs::create_dir_all(out_dir)?;
    let jobname = Path::new(main).file_stem().and_then(|s| s.to_str()).unwrap_or("main").to_string();
    let mut sig_before = aux_signature(out_dir, &jobname);
    let mut bib_ran = false;
    let mut last: Option<CaptureResult> = None;
    let mut passes = 0;
    let mut stable = false;
    while passes < max_passes {
        passes += 1;
        let cap = run_capture(tl, snapshot_dir, main, out_dir, instrumented)?;
        let bcf = out_dir.join(format!("{jobname}.bcf"));
        let aux = out_dir.join(format!("{jobname}.aux"));
        let wants_bib = match bib {
            BibTool::None => false,
            BibTool::Biber => bcf.exists(),
            BibTool::BibTeX => std::fs::read_to_string(&aux).map(|s| s.contains("\\citation") || s.contains("\\bibdata")).unwrap_or(false),
            BibTool::Auto => bcf.exists() || std::fs::read_to_string(&aux).map(|s| s.contains("\\bibdata")).unwrap_or(false),
        };
        if wants_bib && !bib_ran {
            let tool = if bcf.exists() { "biber" } else { "bibtex" };
            let mut cmd = Command::new(tool);
            cmd.current_dir(out_dir).arg(&jobname);
            if let Some(d) = &tl.bin_dir {
                cmd.env("PATH", format!("{}:{}", d.display(), std::env::var("PATH").unwrap_or_default()));
            }
            // bibtex needs BIBINPUTS to find .bib files in the snapshot
            cmd.env("BIBINPUTS", format!("{}:", snapshot_dir.display()));
            let out = cmd.output().with_context(|| format!("running {tool}"))?;
            if !out.status.success() {
                log::warn!("{tool} failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            bib_ran = true;
        }
        let sig_after = aux_signature(out_dir, &jobname);
        let rerun = log_requests_rerun(&cap.log) || sig_after != sig_before || (wants_bib && bib_ran && passes == 1);
        sig_before = sig_after;
        last = Some(cap);
        if !rerun {
            stable = true;
            break;
        }
    }
    Ok(PassOutcome { capture: last.unwrap(), passes, bib_ran, aux_stable: stable })
}

pub fn snapshot_dir(build: &Path) -> PathBuf {
    build.join("src")
}
