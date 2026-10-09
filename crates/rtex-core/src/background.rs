//! One background pass: snapshot the buffers, run the instrumented full compile (with
//! biber/bibtex when the document asks for it), and return the capture.

use crate::capture::{capture_command, collect_capture, CaptureResult};
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
        CaptureResult {
            out_dir: self.out_dir.clone(),
            jobname: self.jobname.clone(),
            json: self.json.clone(),
            pdf: self.pdf.clone(),
            log: self.log.clone(),
            wall: self.wall,
            exit_ok: self.exit_ok,
        }
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
    // the snapshot's own tree, in canonical form (a host may keep its build directory inside
    // the project under any name, given relative or absolute)
    let dst_canon = dst.canonicalize().unwrap_or_else(|_| dst.to_path_buf());
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if name_s.starts_with('.') || name_s == "build" || name_s == "target" {
            continue;
        }
        let path = entry.path();
        // never descend into the build tree the snapshot itself lives in (a host may keep it
        // inside the project under any name), and skip FIFOs, sockets and devices
        let path_canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if dst_canon.starts_with(&path_canon) {
            continue;
        }
        if path.is_dir() {
            let sub = dst.join(&name);
            std::fs::create_dir_all(&sub)?;
            copy_tree(&path, &sub, depth + 1)?;
        } else if !path.is_file() {
            continue;
        } else {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            // a PDF is a compile output only next to the .tex it was built from (main.pdf);
            // every other PDF is an input (\includegraphics, \includepdf)
            if ext == "pdf" && path.with_extension("tex").is_file() {
                continue;
            }
            if matches!(
                ext,
                "aux"
                    | "log"
                    | "synctex.gz"
                    | "fls"
                    | "fdb_latexmk"
                    | "out"
                    | "toc"
                    | "lof"
                    | "lot"
                    | "bbl"
                    | "bcf"
                    | "blg"
                    | "run.xml"
            ) {
                continue;
            }
            let target = dst.join(&name);
            if !target.exists()
                || std::fs::metadata(&path)?.modified()? > std::fs::metadata(&target)?.modified()?
            {
                std::fs::copy(&path, &target)?;
            }
        }
    }
    Ok(())
}

/// Create under `out` every directory of `src` (skipping hidden, build and target), so
/// `\include{chapters/x}` can open `chapters/x.aux` in the output directory.
pub fn mirror_dirs(src: &Path, out: &Path, depth: usize) -> Result<()> {
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
        if entry.path().is_dir() {
            let sub = out.join(&name);
            std::fs::create_dir_all(&sub)?;
            mirror_dirs(&entry.path(), &sub, depth + 1)?;
        }
    }
    Ok(())
}

/// `bytes` with the pass directory's path taken out, plain and hex-encoded (bookmark writes
/// `srcfile={<hex of the .ind path>}` into the `.aux`): passes alternate between directories,
/// and the same document must give the same signature in either.
fn without_dir(bytes: &[u8], dir_forms: &[Vec<u8>]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for form in dir_forms {
        if form.is_empty() {
            continue;
        }
        let mut next = Vec::with_capacity(out.len());
        let mut i = 0;
        while i < out.len() {
            if out[i..].starts_with(form) {
                next.extend_from_slice(b"<dir>");
                i += form.len();
            } else {
                next.push(out[i]);
                i += 1;
            }
        }
        out = next;
    }
    out
}

fn dir_forms(out_dir: &Path) -> Vec<Vec<u8>> {
    let mut paths = vec![out_dir.to_string_lossy().into_owned()];
    if let Ok(c) = out_dir.canonicalize() {
        paths.push(c.to_string_lossy().into_owned());
    }
    let mut forms = Vec::new();
    for p in paths {
        let hex: String = p.bytes().map(|b| format!("{b:02X}")).collect();
        forms.push(hex.to_ascii_lowercase().into_bytes());
        forms.push(hex.into_bytes());
        forms.push(p.into_bytes());
    }
    // longest first, so a path is replaced before a shorter form inside it
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    forms.dedup();
    forms
}

fn aux_signature(out_dir: &Path, jobname: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    let forms = dir_forms(out_dir);
    // every index (imakeidx's `name=` indexes too), as makeindex left it
    let mut inds: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "ind"))
                .collect()
        })
        .unwrap_or_default();
    inds.sort();
    for p in &inds {
        if let Ok(b) = std::fs::read(p) {
            p.file_name().hash(&mut h);
            without_dir(&b, &forms).hash(&mut h);
        }
    }
    for ext in ["aux", "toc", "lof", "lot", "out", "bcf", "bbl", "idx"] {
        if let Ok(b) = std::fs::read(out_dir.join(format!("{jobname}.{ext}"))) {
            let b = without_dir(&b, &forms);
            ext.hash(&mut h);
            b.hash(&mut h);
            if ext == "aux" {
                // partial aux files of \include'd chapters (\@input{chapters/x.aux})
                let text = String::from_utf8_lossy(&b);
                for part in text.split("\\@input{").skip(1) {
                    let Some(name) = part.split('}').next() else {
                        continue;
                    };
                    if let Ok(pb) = std::fs::read(out_dir.join(name)) {
                        name.hash(&mut h);
                        without_dir(&pb, &forms).hash(&mut h);
                    }
                }
            }
        }
    }
    h.finish()
}

fn log_requests_rerun(log: &Path) -> bool {
    std::fs::read_to_string(log)
        .map(|s| {
            s.contains("Rerun to get")
                || s.contains("rerun LaTeX")
                || s.contains("Please (re)run Biber")
                || s.contains("Please rerun LaTeX")
        })
        .unwrap_or(false)
}

/// Run instrumented passes in `out_dir` until the aux family is stable or `max_passes` is hit.
pub fn run_pass(
    tl: &TexLive,
    snapshot_dir: &Path,
    main: &str,
    out_dir: &Path,
    max_passes: u32,
    bib: BibTool,
    instrumented: bool,
) -> Result<PassOutcome> {
    run_pass_with(
        tl,
        snapshot_dir,
        main,
        out_dir,
        max_passes,
        bib,
        instrumented,
        "",
    )
}

/// `run_pass` with extra unit environments for the capture (see `run_capture_with`).
#[allow(clippy::too_many_arguments)]
pub fn run_pass_with(
    tl: &TexLive,
    snapshot_dir: &Path,
    main: &str,
    out_dir: &Path,
    max_passes: u32,
    bib: BibTool,
    instrumented: bool,
    unit_envs: &str,
) -> Result<PassOutcome> {
    let mut runner = |_pass: u32| {
        crate::capture::run_capture_with(tl, snapshot_dir, main, out_dir, instrumented, unit_envs)
    };
    run_pass_with_runner(
        PassPlan {
            tl,
            snapshot_dir,
            main,
            aux_dir: out_dir,
            max_passes,
            bib,
        },
        &mut runner,
        &mut |_, _| {},
    )
}

/// Copy the aux family a pass leaves for the next one (`.aux .toc .lof .lot .out .bbl .bcf
/// .ind .gls .nav .snm`, and the partial `.aux` files of `\include`d chapters in
/// subdirectories) from one pass directory into another. Pass outputs (PDF, log, capture
/// JSON) and the picture cache are not copied, nor are the files TeX only writes, which a
/// standby engine may already hold open from its preamble (`.idx` from `\makeindex`, `.glo`
/// from `\makeglossaries`): a copy would land in a file being written.
pub fn copy_aux_family(from: &Path, to: &Path) -> Result<()> {
    fn go(root: &Path, dir: &Path, to: &Path) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if name != "pic-cache" && !name.starts_with("pass-") {
                    go(root, &path, to)?;
                }
                continue;
            }
            let keep = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    matches!(
                        e,
                        "aux"
                            | "toc"
                            | "lof"
                            | "lot"
                            | "out"
                            | "bbl"
                            | "bcf"
                            | "blg"
                            | "ind"
                            | "gls"
                            | "nav"
                            | "snm"
                            | "vrb"
                            | "xml"
                    )
                })
                .unwrap_or(false);
            if keep {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let dst = to.join(rel);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&path, dst)?;
            }
        }
        Ok(())
    }
    if from == to || !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    go(from, from, to)
}

/// Where and how a sequence of passes runs.
#[derive(Clone, Copy)]
pub struct PassPlan<'a> {
    pub tl: &'a TexLive,
    pub snapshot_dir: &'a Path,
    pub main: &'a str,
    /// The aux family the first pass starts from (each pass may run in its own directory).
    pub aux_dir: &'a Path,
    pub max_passes: u32,
    pub bib: BibTool,
}

/// `run_pass_with` with the single pass supplied by `runner` (a fresh lualatex, or a standby
/// `WarmEngine` that already holds the preamble). Each pass may run in its own directory
/// (`CaptureResult::out_dir`); `aux_dir` holds the aux family the first pass starts from.
pub fn run_pass_with_runner(
    plan: PassPlan,
    runner: &mut dyn FnMut(u32) -> Result<CaptureResult>,
    on_pass: &mut dyn FnMut(&CaptureResult, u32),
) -> Result<PassOutcome> {
    let PassPlan {
        tl,
        snapshot_dir,
        main,
        aux_dir,
        max_passes,
        bib,
    } = plan;
    std::fs::create_dir_all(aux_dir)?;
    let jobname = Path::new(main)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string();
    let mut sig_before = aux_signature(aux_dir, &jobname);
    let mut bib_ran = false;
    let mut indexed = BTreeMap::new();
    let mut last: Option<CaptureResult> = None;
    let mut passes = 0;
    let mut stable = false;
    while passes < max_passes {
        passes += 1;
        let cap = runner(passes)?;
        let out_dir = cap.out_dir.clone();
        let out_dir = out_dir.as_path();
        let bcf = out_dir.join(format!("{jobname}.bcf"));
        let aux = out_dir.join(format!("{jobname}.aux"));
        let wants_bib = match bib {
            BibTool::None => false,
            BibTool::Biber => bcf.exists(),
            BibTool::BibTeX => std::fs::read_to_string(&aux)
                .map(|s| s.contains("\\citation") || s.contains("\\bibdata"))
                .unwrap_or(false),
            BibTool::Auto => {
                bcf.exists()
                    || std::fs::read_to_string(&aux)
                        .map(|s| s.contains("\\bibdata"))
                        .unwrap_or(false)
            }
        };
        if wants_bib && !bib_ran {
            let tool = if bcf.exists() { "biber" } else { "bibtex" };
            let mut cmd = Command::new(tool);
            cmd.current_dir(out_dir).arg(&jobname);
            if let Some(d) = &tl.bin_dir {
                cmd.env(
                    "PATH",
                    format!(
                        "{}:{}",
                        d.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                );
            }
            // bibtex needs BIBINPUTS to find .bib files in the snapshot
            cmd.env("BIBINPUTS", format!("{}:", snapshot_dir.display()));
            let out = cmd.output().with_context(|| format!("running {tool}"))?;
            if !out.status.success() {
                log::warn!("{tool} failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            bib_ran = true;
        }
        run_makeindex(tl, snapshot_dir, out_dir, &mut indexed);
        let sig_after = aux_signature(out_dir, &jobname);
        // a bibliography run that changed nothing (same .bbl) needs no extra pass: the .bbl is
        // part of the signature
        let rerun = log_requests_rerun(&cap.log) || sig_after != sig_before;
        sig_before = sig_after;
        if rerun && passes < max_passes {
            // another pass follows: the caller may show this one meanwhile
            on_pass(&cap, passes);
        }
        last = Some(cap);
        if !rerun {
            stable = true;
            break;
        }
    }
    Ok(PassOutcome {
        capture: last.unwrap(),
        passes,
        bib_ran,
        aux_stable: stable,
    })
}

/// Run makeindex on every `.idx` a pass wrote (`\makeindex`, imakeidx's named indexes) whose
/// entries differ from the ones last indexed in `indexed` (name → hash of the `.idx`), or whose
/// `.ind` is missing. The `.ind` files are part of the aux signature, so a changed index
/// brings another pass. makeindex is run here rather than by the document (imakeidx's own
/// call needs shell escape and cannot find an `.idx` in the output directory).
fn run_makeindex(
    tl: &TexLive,
    snapshot_dir: &Path,
    out_dir: &Path,
    indexed: &mut BTreeMap<String, u64>,
) {
    use std::hash::{Hash, Hasher};
    let Ok(dir) = std::fs::read_dir(out_dir) else {
        return;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "idx") {
            continue;
        }
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        let hash = h.finish();
        if indexed.get(&name) == Some(&hash) && path.with_extension("ind").exists() {
            continue;
        }
        let mut cmd = Command::new("makeindex");
        cmd.current_dir(out_dir).arg("-q").arg(&name);
        if let Some(d) = &tl.bin_dir {
            cmd.env(
                "PATH",
                format!(
                    "{}:{}",
                    d.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        }
        // index styles (.ist) the project ships
        cmd.env("INDEXSTYLE", format!("{}:", snapshot_dir.display()));
        match cmd.output() {
            Ok(out) if !out.status.success() => {
                log::warn!(
                    "makeindex {name} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                )
            }
            Err(e) => log::warn!("running makeindex: {e}"),
            _ => {}
        }
        indexed.insert(name, hash);
    }
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

fn read_aux_into(
    aux: &Path,
    out: &mut Vec<(String, String)>,
    seen: &mut std::collections::HashSet<PathBuf>,
    depth: u32,
) {
    if depth > 8 || !seen.insert(aux.to_path_buf()) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(aux) else {
        return;
    };
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
        let Some((name, after)) = brace_group(&text, j) else {
            i += 1;
            continue;
        };
        j = after;
        if kind.is_empty() {
            let child = aux.parent().unwrap_or(Path::new(".")).join(name.trim());
            read_aux_into(&child, out, seen, depth + 1);
            i = j;
            continue;
        }
        let Some((value, after)) = brace_group(&text, j) else {
            i += 1;
            continue;
        };
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
        assert_eq!(
            v,
            vec![
                ("r@eq:a".to_string(), "{1.2}{5}".to_string()),
                ("b@knuth".to_string(), "1".to_string()),
                ("r@sec:b".to_string(), "{3}{7}".to_string()),
                (
                    "r@fig:x".to_string(),
                    "{1}{2}{Caption}{figure.1}{}".to_string()
                ),
            ]
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Standby background engine: a lualatex that has loaded the preamble and waits for the body.
// Engine start, format and preamble (packages, fonts) are ~75 % of a pass over a short
// document (docs/BENCHMARKS.md), so a pass that only typesets the body is 3–4× faster.
// ---------------------------------------------------------------------------------------------

/// Snapshot layout for a standby pass: the project copied as for `write_snapshot`, the preamble
/// (text before `\begin{document}`) in `rtex-preamble.tex`, and `main` replaced by the body
/// preceded by as many empty lines as the preamble had, so every line number and `status.filename`
/// the capture records are those of the original file. Returns the preamble's hash.
pub fn write_body_snapshot(
    project: &Path,
    files: &BTreeMap<String, String>,
    main: &str,
    dir: &Path,
) -> Result<u64> {
    let main_text = files
        .get(main)
        .ok_or_else(|| anyhow::anyhow!("main file {main} not in snapshot"))?;
    let (pre, body) = crate::split_preamble(main_text)
        .ok_or_else(|| anyhow::anyhow!("no \\begin{{document}} in {main}"))?;
    let mut padded = String::with_capacity(main_text.len());
    for _ in 0..pre.matches('\n').count() {
        padded.push('\n');
    }
    padded.push_str(body);
    let mut files2 = files.clone();
    files2.insert(main.to_string(), padded);
    files2.insert("rtex-preamble.tex".to_string(), pre.to_string());
    write_snapshot(project, &files2, dir)?;
    Ok(crate::document::hash_str(&crate::document::expand_inputs(
        pre, files,
    )))
}

pub struct WarmEngine {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    pub preamble_hash: u64,
    pub src_dir: PathBuf,
    out_dir: PathBuf,
    jobname: String,
    instrumented: bool,
    pub spawned: std::time::Instant,
}

/// What a standby engine runs: the project and its sources, where the body snapshot goes and
/// where the pass writes.
#[derive(Clone, Copy)]
pub struct StandbySpec<'a> {
    pub tl: &'a TexLive,
    pub project: &'a Path,
    pub files: &'a BTreeMap<String, String>,
    pub main: &'a str,
    pub src_dir: &'a Path,
    pub out_dir: &'a Path,
    pub instrumented: bool,
    pub unit_envs: &'a str,
}

impl WarmEngine {
    /// Start a standby: writes the body snapshot into `src_dir` and runs lualatex up to the end
    /// of the preamble, where it blocks on stdin (tex/rtex-bg.lua).
    pub fn spawn(spec: StandbySpec) -> Result<WarmEngine> {
        let StandbySpec {
            tl,
            project,
            files,
            main,
            src_dir,
            out_dir,
            instrumented,
            unit_envs,
        } = spec;
        let preamble_hash = write_body_snapshot(project, files, main, src_dir)?;
        let (mut cmd, jobname, out_dir) =
            capture_command(tl, src_dir, main, out_dir, instrumented, unit_envs)?;
        let pkg = if instrumented {
            "\\RequirePackage{rtex-capture}"
        } else {
            ""
        };
        cmd.arg(format!("{pkg}\\input{{rtex-preamble.tex}}\\directlua{{dofile(kpse.find_file(\"rtex-bg.lua\",\"lua\") or \"rtex-bg.lua\")}}\\input{{{main}}}"));
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().context("spawning standby lualatex")?;
        let stdin = child.stdin.take();
        Ok(WarmEngine {
            child,
            stdin,
            preamble_hash,
            src_dir: src_dir.to_path_buf(),
            out_dir,
            jobname,
            instrumented,
            spawned: std::time::Instant::now(),
        })
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Where this engine writes its pass (PDF, log, capture JSON, aux family).
    pub fn out_dir(&self) -> &Path {
        &self.out_dir
    }

    /// Typeset the body: refresh the snapshot texts (same preamble), release the engine and
    /// collect the pass like a fresh run.
    pub fn run(
        mut self,
        project: &Path,
        files: &BTreeMap<String, String>,
        main: &str,
    ) -> Result<CaptureResult> {
        let t0 = std::time::Instant::now();
        let h = write_body_snapshot(project, files, main, &self.src_dir)?;
        if h != self.preamble_hash {
            anyhow::bail!("standby preamble differs from the snapshot");
        }
        {
            use std::io::Write;
            let mut stdin = self
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("standby stdin already closed"))?;
            stdin.write_all(b"GO\n").context("releasing standby")?;
            stdin.flush().ok();
            drop(stdin);
        }
        // drain stdout before waiting (a full pipe would block the engine), then reap
        let mut stdout = Vec::new();
        if let Some(mut so) = self.child.stdout.take() {
            use std::io::Read;
            let _ = so.read_to_end(&mut stdout);
        }
        let status = self.child.wait().context("waiting for standby pass")?;
        collect_capture(
            &self.out_dir,
            &self.jobname,
            self.instrumented,
            status.success(),
            status.code(),
            &stdout,
            t0.elapsed(),
        )
    }

    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for WarmEngine {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_copies_nested_folders() {
        let dir = std::env::temp_dir().join(format!("rtex-snap-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(project.join("a/b")).unwrap();
        std::fs::write(project.join("main.tex"), "x").unwrap();
        std::fs::write(project.join("a/b/file.txt"), "nested").unwrap();
        std::fs::write(project.join("a/top.bib"), "bib").unwrap();
        let mut files = BTreeMap::new();
        files.insert("main.tex".to_string(), "edited".to_string());
        write_snapshot(&project, &files, &dir.join("snap")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("snap/a/b/file.txt")).unwrap(),
            "nested"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("snap/a/top.bib")).unwrap(),
            "bib"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("snap/main.tex")).unwrap(),
            "edited"
        );
        // a second snapshot (files already present) is fine too
        write_snapshot(&project, &files, &dir.join("snap")).unwrap();
    }

    #[test]
    fn signature_ignores_the_pass_directory() {
        let dir = std::env::temp_dir().join(format!("rtex-sig-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("pass-0"), dir.join("pass-1"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
            let p = d.canonicalize().unwrap().join("main.ind");
            let hex: String = p
                .to_string_lossy()
                .bytes()
                .map(|x| format!("{x:02X}"))
                .collect();
            std::fs::write(
                d.join("main.aux"),
                format!(
                    "\\BKM@entry{{srcfile={{{hex}}}}}\n\\@input{{{}}}\n",
                    p.display()
                ),
            )
            .unwrap();
            std::fs::write(d.join("main.ind"), "\\begin{theindex}\\end{theindex}").unwrap();
        }
        assert_eq!(aux_signature(&a, "main"), aux_signature(&b, "main"));
        std::fs::write(
            b.join("main.ind"),
            "\\begin{theindex}\\item x\\end{theindex}",
        )
        .unwrap();
        assert_ne!(aux_signature(&a, "main"), aux_signature(&b, "main"));
    }

    #[test]
    fn snapshot_keeps_input_pdfs_and_drops_built_ones() {
        let dir = std::env::temp_dir().join(format!("rtex-snap-pdf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(project.join("images")).unwrap();
        std::fs::write(project.join("main.tex"), "x").unwrap();
        std::fs::write(project.join("main.pdf"), "built").unwrap();
        std::fs::write(project.join("figure.pdf"), "input").unwrap();
        std::fs::write(project.join("images/plot.pdf"), "input").unwrap();
        write_snapshot(&project, &BTreeMap::new(), &dir.join("snap")).unwrap();
        assert!(!dir.join("snap/main.pdf").exists());
        assert!(dir.join("snap/figure.pdf").is_file());
        assert!(dir.join("snap/images/plot.pdf").is_file());
    }

    #[test]
    fn body_snapshot_keeps_line_numbers() {
        let dir = std::env::temp_dir().join(format!("rtex-body-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let main = "\\documentclass{article}\n\\usepackage{xcolor}\n% two\n\\begin{document}\nFirst line of the body.\n\n\\end{document}\n";
        std::fs::write(project.join("main.tex"), main).unwrap();
        let mut files = BTreeMap::new();
        files.insert("main.tex".to_string(), main.to_string());
        let h = write_body_snapshot(&project, &files, "main.tex", &dir.join("snap")).unwrap();
        let pre = std::fs::read_to_string(dir.join("snap/rtex-preamble.tex")).unwrap();
        let body = std::fs::read_to_string(dir.join("snap/main.tex")).unwrap();
        assert_eq!(
            pre,
            "\\documentclass{article}\n\\usepackage{xcolor}\n% two\n"
        );
        assert_eq!(h, crate::document::hash_str(&pre));
        // \begin{document} is on line 4 in both the original and the padded body file
        assert_eq!(
            main.lines()
                .position(|l| l.starts_with("\\begin{document}")),
            Some(3)
        );
        assert_eq!(
            body.lines()
                .position(|l| l.starts_with("\\begin{document}")),
            Some(3)
        );
        assert_eq!(body.lines().count(), main.lines().count());
        assert_eq!(body.lines().nth(4), Some("First line of the body."));
    }
}
