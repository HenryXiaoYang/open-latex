//! One background pass: snapshot the buffers, run the instrumented full compile (with
//! biber/bibtex when the document asks for it), and return the capture.

use crate::capture::CaptureResult;
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
    run_pass_with(tl, snapshot_dir, main, out_dir, max_passes, bib, instrumented, "")
}

/// `run_pass` with extra unit environments for the capture (see `run_capture_with`).
#[allow(clippy::too_many_arguments)]
pub fn run_pass_with(tl: &TexLive, snapshot_dir: &Path, main: &str, out_dir: &Path, max_passes: u32, bib: BibTool, instrumented: bool, unit_envs: &str) -> Result<PassOutcome> {
    std::fs::create_dir_all(out_dir)?;
    let jobname = Path::new(main).file_stem().and_then(|s| s.to_str()).unwrap_or("main").to_string();
    let mut sig_before = aux_signature(out_dir, &jobname);
    let mut bib_ran = false;
    let mut last: Option<CaptureResult> = None;
    let mut passes = 0;
    let mut stable = false;
    while passes < max_passes {
        passes += 1;
        let cap = crate::capture::run_capture_with(tl, snapshot_dir, main, out_dir, instrumented, unit_envs)?;
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

/// `\newlabel{name}{value}` and `\bibcite{key}{value}` definitions from an `.aux` file (and the
/// files it `\@input`s), as (control-sequence name, macro body) pairs the fast server can define
/// with `token.set_macro`: `r@name` → value, `b@key` → value.
pub fn read_aux_labels(aux: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    read_aux_into(aux, &mut out, &mut seen, 0);
    out
}

fn read_aux_into(aux: &Path, out: &mut Vec<(String, String)>, seen: &mut std::collections::HashSet<PathBuf>, depth: u32) {
    if depth > 8 || !seen.insert(aux.to_path_buf()) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(aux) else { return };
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        let (prefix, kind) = if rest.starts_with("\\newlabel") {
            ("\\newlabel", "r@")
        } else if rest.starts_with("\\bibcite") {
            ("\\bibcite", "b@")
        } else if rest.starts_with("\\@input") {
            ("\\@input", "")
        } else {
            i += 1;
            continue;
        };
        let mut j = i + prefix.len();
        let Some((name, after)) = brace_group(&text, j) else { i += 1; continue };
        j = after;
        if kind.is_empty() {
            let child = aux.parent().unwrap_or(Path::new(".")).join(name.trim());
            read_aux_into(&child, out, seen, depth + 1);
            i = j;
            continue;
        }
        let Some((value, after)) = brace_group(&text, j) else { i += 1; continue };
        out.push((format!("{kind}{name}"), value));
        i = after;
    }
}

/// The contents of the brace group starting at (or after whitespace from) `from`; returns the
/// inner text and the index after the closing brace.
fn brace_group(text: &str, from: usize) -> Option<(String, usize)> {
    let b = text.as_bytes();
    let mut i = from;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\n' || b[i] == b'\r' || b[i] == b'\t') {
        i += 1;
    }
    if i >= b.len() || b[i] != b'{' {
        return None;
    }
    let start = i + 1;
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((text[start..i].to_string(), i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod aux_tests {
    use super::*;
    #[test]
    fn labels_and_bibcites() {
        let dir = std::env::temp_dir().join(format!("rtex-aux-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.aux"), "\\relax\n\\newlabel{eq:a}{{1.2}{5}}\n\\bibcite{knuth}{1}\n\\@input{ch.aux}\n\\newlabel{fig:x}{{1}{2}{Caption}{figure.1}{}}\n").unwrap();
        std::fs::write(dir.join("ch.aux"), "\\newlabel{sec:b}{{3}{7}}\n").unwrap();
        let v = read_aux_labels(&dir.join("main.aux"));
        assert_eq!(v, vec![
            ("r@eq:a".to_string(), "{1.2}{5}".to_string()),
            ("b@knuth".to_string(), "1".to_string()),
            ("r@sec:b".to_string(), "{3}{7}".to_string()),
            ("r@fig:x".to_string(), "{1}{2}{Caption}{figure.1}{}".to_string()),
        ]);
    }
}
