//! Running an instrumented full LuaLaTeX pass (`lode-capture`) and reading its results.

use crate::texlive::TexLive;
use anyhow::{bail, Context, Result};
use lode_dl::{DisplayList, Sp};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Nfss {
    pub enc: Option<String>,
    pub family: Option<String>,
    pub series: Option<String>,
    pub shape: Option<String>,
    pub size: Option<String>,
    pub baselineskip: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParaBegin {
    pub line: i64,
    pub nest: i64,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub font: Option<i64>,
    #[serde(default)]
    pub nfss: Nfss,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub mathversion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Placement {
    pub page: i64,
    pub i: i64,
    pub x: Sp,
    pub y: Sp,
    pub w: Sp,
    pub h: Sp,
    pub d: Sp,
}

/// A paragraph as seen by `pre_linebreak_filter` in the instrumented run, with its context.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedParagraph {
    pub seq: i64,
    pub groupcode: String,
    pub nest: i64,
    pub end_line: i64,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub lines: Option<i64>,
    #[serde(default)]
    pub ints: BTreeMap<String, i64>,
    #[serde(default)]
    pub dims: BTreeMap<String, i64>,
    #[serde(default)]
    pub glues: BTreeMap<String, Vec<f64>>,
    #[serde(default)]
    pub parshape: Option<serde_json::Value>,
    #[serde(default)]
    pub everypar: String,
    #[serde(default)]
    pub flags: BTreeMap<String, i64>,
    #[serde(default)]
    pub begin: Option<ParaBegin>,
    #[serde(default)]
    pub placements: Option<Vec<Placement>>,
}

impl CapturedParagraph {
    pub fn is_top_level(&self) -> bool {
        self.groupcode.is_empty() && self.nest == 1
    }
    /// Source line range (1-based, inclusive start, inclusive end as reported by the filter).
    pub fn start_line(&self) -> Option<i64> {
        self.begin.as_ref().map(|b| b.line)
    }
    /// The context the fast server needs (serialized as the `ctx` of a `context` request).
    pub fn context_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ints": self.ints, "dims": self.dims, "glues": self.glues,
            "parshape": self.parshape, "everypar": self.everypar,
            "begin": self.begin,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CaptureJson {
    pub version: i64,
    pub jobname: String,
    pub pages: i64,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub paragraphs: Vec<CapturedParagraph>,
}

#[derive(Debug)]
pub struct CaptureResult {
    pub out_dir: PathBuf,
    pub jobname: String,
    pub json: CaptureJson,
    pub pdf: PathBuf,
    pub log: PathBuf,
    pub wall: std::time::Duration,
    pub exit_ok: bool,
}

impl CaptureResult {
    pub fn page(&self, n: i64) -> Result<DisplayList> {
        let p = self.out_dir.join(format!("{}.lode-page{}.json", self.jobname, n));
        let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        Ok(DisplayList::from_json(&s)?)
    }
    pub fn paragraph(&self, seq: i64) -> Option<&CapturedParagraph> {
        self.json.paragraphs.iter().find(|p| p.seq == seq)
    }
}

/// Run `lualatex '\RequirePackage{lode-capture}\input{main}'` in `src_dir`, writing to `out_dir`.
/// `instrumented=false` runs the same build without the capture package (clean reference).
pub fn run_capture(tl: &TexLive, src_dir: &Path, main: &str, out_dir: &Path, instrumented: bool) -> Result<CaptureResult> {
    std::fs::create_dir_all(out_dir)?;
    let out_dir = &out_dir.canonicalize()?;
    let jobname = Path::new(main).file_stem().and_then(|s| s.to_str()).unwrap_or("main").to_string();
    let mut cmd = tl.lualatex_cmd(src_dir);
    cmd.arg("-interaction=nonstopmode")
        .arg("-file-line-error")
        .arg(format!("--jobname={jobname}"))
        .arg(format!("--output-directory={}", out_dir.display()))
        .env("LODE_CAPTURE_DIR", out_dir);
    if instrumented {
        cmd.arg(format!("\\RequirePackage{{lode-capture}}\\input{{{main}}}"));
    } else {
        cmd.arg(format!("\\input{{{main}}}"));
    }
    let t0 = Instant::now();
    let out = cmd.output().context("spawning lualatex")?;
    let wall = t0.elapsed();
    let log = out_dir.join(format!("{jobname}.log"));
    let pdf = out_dir.join(format!("{jobname}.pdf"));
    let json = if instrumented {
        let jp = out_dir.join(format!("{jobname}.lode.json"));
        if !jp.exists() {
            bail!(
                "capture run produced no {}; lualatex exit {:?}\n{}",
                jp.display(),
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>()
            );
        }
        serde_json::from_str(&std::fs::read_to_string(&jp)?).context("parsing capture json")?
    } else {
        CaptureJson { jobname: jobname.clone(), ..Default::default() }
    };
    Ok(CaptureResult { out_dir: out_dir.to_path_buf(), jobname, json, pdf, log, wall, exit_ok: out.status.success() })
}
