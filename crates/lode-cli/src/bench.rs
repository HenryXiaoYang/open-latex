//! `lode bench`: the paper's in-engine line-breaking replica (hardware factor) and host-side
//! round-trip benchmarks over paragraph categories and document sizes, with the gates of
//! docs/BENCHMARKS.md.

use anyhow::{bail, Context, Result};
use lode_core::texlive::TexLive;
use lode_core::{Edit, Event, Session, SessionConfig};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PAPER_LINEBREAK_MS: [(&str, f64); 5] = [("short", 0.17), ("medium", 0.70), ("long", 1.99), ("inline-math", 0.20), ("display-math", 0.11)];
pub const PAPER_ROUNDTRIP_MS: [(&str, f64); 2] = [("short", 0.79), ("medium", 6.11)];

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stats {
    pub n: usize,
    pub median: f64,
    pub p5: f64,
    pub p95: f64,
    pub min: f64,
    pub max: f64,
}
pub fn stats(v: &[f64]) -> Stats {
    if v.is_empty() {
        return Stats::default();
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    let at = |f: f64| s[(((n as f64) * f).floor() as usize + 1).min(n) - 1];
    Stats { n, median: at(0.5), p5: at(0.05), p95: at(0.95), min: s[0], max: s[n - 1] }
}

/// Run the paper's own `systematic-benchmark.tex` (vendored verbatim in
/// bench/upstream/luatex-benchmark) and return per-category (median, p95) in ms, keyed by our
/// category names. `quick` is ignored: the upstream script fixes its own sample plan (30 × 100).
pub fn linebreak(tl: &TexLive, repo: &Path, out_dir: &Path, _quick: bool) -> Result<BTreeMap<String, (f64, f64)>> {
    std::fs::create_dir_all(out_dir)?;
    let out_dir = &out_dir.canonicalize()?;
    let dir = repo.join("bench/upstream/luatex-benchmark");
    let mut cmd = tl.lualatex_cmd(&dir);
    cmd.arg("-interaction=nonstopmode").arg(format!("--output-directory={}", out_dir.display())).arg("systematic-benchmark.tex");
    let out = cmd.output().context("running upstream systematic-benchmark.tex")?;
    let log = std::fs::read_to_string(out_dir.join("systematic-benchmark.log")).unwrap_or_default();
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), log);
    let re = regex::Regex::new(r"(?m)^(Short|Medium|Long|Inline math|Display math)\s+median=\s*([0-9.]+) ms\s+P5=\s*([0-9.]+)\s+P95=\s*([0-9.]+)").unwrap();
    let mut m = BTreeMap::new();
    for c in re.captures_iter(&text) {
        let key = match &c[1] { "Short" => "short", "Medium" => "medium", "Long" => "long", "Inline math" => "inline-math", _ => "display-math" };
        m.insert(key.to_string(), (c[2].parse().unwrap_or(0.0), c[4].parse().unwrap_or(0.0)));
    }
    if m.len() < 5 {
        bail!("could not parse the upstream benchmark summary; lualatex exit {:?}; tail:\n{}", out.status, text.chars().rev().take(600).collect::<String>().chars().rev().collect::<String>());
    }
    Ok(m)
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryResult {
    pub project: String,
    pub category: String,
    pub par_id: u64,
    pub lines: usize,
    pub glyphs: usize,
    pub individual_total: Stats,
    pub individual_tex: Stats,
    pub individual_traverse: Stats,
    pub individual_pack: Stats,
    pub amortized_total: Stats,
    pub overhead_share_median: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub name: String,
    pub value: f64,
    pub limit: f64,
    pub pass: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BenchReport {
    pub machine: String,
    pub hardware_factor: f64,
    pub linebreak_ms: BTreeMap<String, (f64, f64)>,
    pub results: Vec<CategoryResult>,
    pub gates: Vec<Gate>,
    pub pass: bool,
}

fn classify(kind: &str, lines: usize, text: &str) -> Option<&'static str> {
    let has_inline_math = text.contains('$') && !text.contains("\\[") && !text.contains("\\begin{equation") && !text.contains("\\begin{align");
    let has_display = text.contains("\\[") || text.contains("\\begin{equation") || text.contains("\\begin{align");
    let has_footnote = text.contains("\\footnote");
    match kind {
        "par" if has_display => Some("display-math"),
        "par" if has_footnote => Some("footnote"),
        "par" => match (lines, has_inline_math) {
            (1, false) => Some("short"),
            (4..=5, false) => Some("medium"),
            (l, false) if l >= 10 => Some("long"),
            (1..=3, true) => Some("inline-math"),
            _ => None,
        },
        k if k.starts_with("env:itemize") || k.starts_with("env:enumerate") => Some("list"),
        k if k.starts_with("env:figure") => Some("figure"),
        k if k.starts_with("env:table") => Some("table"),
        k if k.starts_with("heading:") => Some("heading"),
        _ => None,
    }
}

pub fn roundtrip(project: &Path, categories: &[String], samples: usize, inner: usize, build: &Path) -> Result<Vec<CategoryResult>> {
    let name = project.file_name().and_then(|s| s.to_str()).unwrap_or("project").to_string();
    let mut cfg = SessionConfig::new(project, "main.tex");
    cfg.build_dir = build.join(&name);
    // the benchmark measures; the gates judge the 5 ms budget, so the session must not re-route
    cfg.fast_budget = Duration::from_millis(500);
    let session = Session::open(cfg)?;
    let (first, _) = session.wait_for(Duration::from_secs(600), |e| matches!(e, Event::LayoutUpdate { .. }));
    let Some(Event::LayoutUpdate { eligible_paragraphs, placements, .. }) = first else { bail!("no layout") };
    session.pause_background(true); // no background passes competing for the CPU during timing
    // wait for the warm-up compile to finish so the engine is hot
    std::thread::sleep(Duration::from_millis(1500));
    let doc = session.document_text("main.tex").unwrap();
    let spans = session.spans("main.tex");
    let lines_of: BTreeMap<u64, (usize, String)> = placements.iter().map(|p| (p.par_id.0, (p.lines as usize, p.kind.clone()))).collect();
    let mut picked: BTreeMap<&str, (u64, usize, String)> = BTreeMap::new();
    // prefer paragraphs in the middle of the document
    let mut elig: Vec<_> = eligible_paragraphs.iter().filter_map(|id| spans.iter().find(|s| s.id == *id)).collect();
    let mid = elig.len() / 2;
    elig.rotate_left(mid);
    for sp in elig {
        let text = &doc[sp.range.clone()];
        let (lines, kind) = lines_of.get(&sp.id.0).cloned().unwrap_or((0, "par".into()));
        if let Some(cat) = classify(&kind, lines, text) {
            if categories.iter().any(|c| c == cat) && !picked.contains_key(cat) {
                picked.insert(cat, (sp.id.0, lines, text.to_string()));
            }
        }
    }
    let mut results = Vec::new();
    for cat in categories {
        let Some((pid, lines, text)) = picked.get(cat.as_str()).cloned() else {
            eprintln!("  {name}: no eligible paragraph for category {cat}");
            continue;
        };
        // positions are read fresh: earlier categories may have shifted later spans
        let spans_now = session.spans("main.tex");
        let sp = spans_now.iter().find(|s| s.id.0 == pid).unwrap().clone();
        let pos = sp.range.start + text.find(' ').unwrap_or(0);
        let mut total = Vec::new();
        let mut tex = Vec::new();
        let mut trav = Vec::new();
        let mut pack = Vec::new();
        let mut glyphs = 0;
        let mut toggled = false;
        let do_edit = |session: &Session, toggled: &mut bool| -> Result<(f64, f64, f64, f64, usize)> {
            let edit = if *toggled { Edit { start_byte: pos, end_byte: pos + 2, text: String::new() } } else { Edit { start_byte: pos, end_byte: pos, text: " x".into() } };
            *toggled = !*toggled;
            let t0 = Instant::now();
            let r = session.apply_edit("main.tex", edit)?;
            if r.routed != "fast" {
                bail!("edit not routed fast: {:?}", r.reasons);
            }
            let (ev, others) = session.wait_for(Duration::from_secs(30), |e| matches!(e, Event::ParagraphUpdate { .. }));
            let Some(Event::ParagraphUpdate { timing, dl, status, diagnostics, .. }) = ev else {
                let seen: Vec<String> = others.iter().map(|e| match e {
                    Event::EngineState { state, reason, .. } => format!("EngineState {state} {reason:?}"),
                    Event::BackgroundScheduled { reasons, .. } => format!("BackgroundScheduled {reasons:?}"),
                    Event::Diagnostics { items, .. } => format!("Diagnostics {:?}", items.iter().map(|d| d.message.clone()).collect::<Vec<_>>()),
                    other => format!("{}", serde_json::to_value(other).map(|v| v["event"].to_string()).unwrap_or_default()),
                }).collect();
                bail!("no paragraph update for par {pid} ({cat}); events seen: {seen:?}; edit result: {:?}", r.reasons)
            };
            if status != "ok" && status != "ok_degraded" {
                bail!("status {status} for {cat} par {pid} (edit {:?} at {pos}): {:?}", if *toggled { "insert" } else { "delete" }, diagnostics.iter().map(|d| format!("{} | {}", d.message, d.context.clone().unwrap_or_default().chars().take(200).collect::<String>())).collect::<Vec<_>>());
            }
            Ok((t0.elapsed().as_secs_f64() * 1e3, timing.tex_us as f64 / 1e3, timing.traverse_us as f64 / 1e3, timing.pack_us as f64 / 1e3, dl.glyph_count()))
        };
        // warm this paragraph's fonts
        for _ in 0..3 {
            let _ = do_edit(&session, &mut toggled)?;
        }
        for _ in 0..samples {
            let (t, a, b, c, g) = do_edit(&session, &mut toggled)?;
            total.push(t);
            tex.push(a);
            trav.push(b);
            pack.push(c);
            glyphs = g;
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut amort = Vec::new();
        for _ in 0..(samples / 10).max(5) {
            let t0 = Instant::now();
            for _ in 0..inner {
                do_edit(&session, &mut toggled)?;
            }
            amort.push(t0.elapsed().as_secs_f64() * 1e3 / inner as f64);
        }
        // leave the paragraph as it was (the toggle may have ended in the inserted state)
        if toggled {
            do_edit(&session, &mut toggled)?;
        }
        let st = stats(&total);
        let stx = stats(&tex);
        let overhead = if st.median > 0.0 { (st.median - stx.median).max(0.0) / st.median } else { 0.0 };
        println!("  {name} {cat:12} par {pid} lines {lines} glyphs {glyphs}: individual median {:.3} ms (P95 {:.3}; tex {:.3} traverse {:.3} pack {:.3}); amortized median {:.3} ms",
            st.median, st.p95, stx.median, stats(&trav).median, stats(&pack).median, stats(&amort).median);
        results.push(CategoryResult {
            project: name.clone(), category: cat.clone(), par_id: pid, lines, glyphs,
            individual_total: st, individual_tex: stx, individual_traverse: stats(&trav), individual_pack: stats(&pack),
            amortized_total: stats(&amort), overhead_share_median: overhead,
        });
    }
    session.close();
    Ok(results)
}

pub fn gates(report: &mut BenchReport) {
    let h = report.hardware_factor.max(1.0);
    let mut gates = Vec::new();
    let find = |proj_contains: &str, cat: &str| report.results.iter().find(|r| r.project.contains(proj_contains) && r.category == cat && !r.project.contains("units"));
    // size independence across 10/100/300
    for cat in ["short", "medium", "long", "inline-math"] {
        if let (Some(a), Some(b)) = (find("book-10-", cat), find("book-300-", cat)) {
            let ratio = b.individual_total.median / a.individual_total.median;
            gates.push(Gate { name: format!("size independence {cat} (300p/10p individual median)"), value: ratio, limit: 1.25, pass: (0.8..=1.25).contains(&ratio), note: format!("{:.3} ms vs {:.3} ms", b.individual_total.median, a.individual_total.median) });
        }
        if let (Some(a), Some(b)) = (find("book-10-", cat), find("book-100-", cat)) {
            let ratio = b.individual_total.median / a.individual_total.median;
            gates.push(Gate { name: format!("size independence {cat} (100p/10p individual median)"), value: ratio, limit: 1.25, pass: (0.8..=1.25).contains(&ratio), note: String::new() });
        }
    }
    // absolute gates on the largest document (hardest case), hardware-qualified
    let big: Vec<&CategoryResult> = report.results.iter().filter(|r| r.project.contains("book-300-") && !r.project.contains("units")).collect();
    let pick = |cat: &str| -> Option<&CategoryResult> { big.iter().copied().find(|r| r.category == cat).or_else(|| report.results.iter().find(|r| r.category == cat)) };
    if let Some(r) = pick("short") {
        gates.push(Gate { name: "short amortized median ≤ 1.0 ms × h".into(), value: r.amortized_total.median, limit: 1.0 * h, pass: r.amortized_total.median <= 1.0 * h, note: format!("h = {:.2}", h) });
        gates.push(Gate { name: "short individual median ≤ 1.5 ms × h".into(), value: r.individual_total.median, limit: 1.5 * h, pass: r.individual_total.median <= 1.5 * h, note: format!("P95 {:.3} ms", r.individual_total.p95) });
        gates.push(Gate { name: "short P95 ≤ 3 × median".into(), value: r.individual_total.p95 / r.individual_total.median, limit: 3.0, pass: r.individual_total.p95 <= 3.0 * r.individual_total.median, note: String::new() });
        // IPC + host plumbing (everything outside tex + traverse + pack): two process hops and two
        // thread hops; absolute, hardware-qualified. The paper's "overhead share" is reported, not
        // gated: our engine stage is several times faster than the paper's, so the same fixed
        // wake-up cost is a larger fraction.
        let host = (r.individual_total.median - r.individual_tex.median - r.individual_traverse.median - r.individual_pack.median).max(0.0);
        gates.push(Gate { name: "short IPC + host overhead ≤ 0.5 ms × h".into(), value: host, limit: 0.5 * h, pass: host <= 0.5 * h, note: format!("overhead share {:.0} % (paper: 18 %)", r.overhead_share_median * 100.0) });
    }
    if let Some(r) = pick("medium") {
        gates.push(Gate { name: "medium individual median ≤ 6.11 ms × h (paper reference)".into(), value: r.individual_total.median, limit: 6.11 * h, pass: r.individual_total.median <= 6.11 * h, note: String::new() });
        let tp = r.individual_traverse.median + r.individual_pack.median;
        gates.push(Gate { name: "medium traversal + serialization ≤ 0.5 ms".into(), value: tp, limit: 0.5, pass: tp <= 0.5, note: String::new() });
        let host = (r.individual_total.median - r.individual_tex.median - r.individual_traverse.median - r.individual_pack.median).max(0.0);
        gates.push(Gate { name: "medium IPC + host overhead ≤ 0.5 ms × h".into(), value: host, limit: 0.5 * h, pass: host <= 0.5 * h, note: format!("overhead share {:.0} % (paper: 18 %)", r.overhead_share_median * 100.0) });
    }
    // unit kinds (0.0.2): every real-time unit kind stays under 5 ms per keystroke on the largest
    // units fixture, and is document-size independent (100p/10p)
    let units_big: Vec<&CategoryResult> = report.results.iter().filter(|r| r.project.contains("units")).collect();
    for cat in ["display-math", "footnote", "list", "figure", "table", "heading"] {
        let largest = units_big.iter().copied().filter(|r| r.category == cat).max_by_key(|r| r.project.split('-').nth(1).and_then(|n| n.parse::<u32>().ok()).unwrap_or(0));
        if let Some(r) = largest {
            gates.push(Gate { name: format!("{cat} individual median ≤ 5.0 ms × h ({})", r.project), value: r.individual_total.median, limit: 5.0 * h, pass: r.individual_total.median <= 5.0 * h, note: format!("P95 {:.3} ms, tex {:.3} ms, {} rows", r.individual_total.p95, r.individual_tex.median, r.lines) });
        }
        let small = units_big.iter().copied().find(|r| r.category == cat && r.project.contains("book-10-"));
        let big100 = units_big.iter().copied().find(|r| r.category == cat && r.project.contains("book-100-"));
        if let (Some(a), Some(b)) = (small, big100) {
            let ratio = b.individual_total.median / a.individual_total.median;
            gates.push(Gate { name: format!("size independence {cat} (100p/10p individual median)"), value: ratio, limit: 1.25, pass: (0.75..=1.25).contains(&ratio), note: format!("{:.3} ms vs {:.3} ms", b.individual_total.median, a.individual_total.median) });
        }
    }
    report.pass = gates.iter().all(|g| g.pass);
    report.gates = gates;
}

pub fn write_markdown(report: &BenchReport, path: &Path) -> Result<()> {
    let mut md = String::new();
    md.push_str(&format!("# Round-trip benchmark\n\nMachine: {}  \nHardware factor h = {:.2} (in-engine line breaking vs the paper's Table 1)\n\n", report.machine, report.hardware_factor));
    md.push_str("## In-engine line breaking (upstream `texlode/luatex-benchmark`, vendored in bench/upstream)\n\n| Category | Paper | Here (amortized median) |\n|---|---|---|\n");
    for (cat, paper) in PAPER_LINEBREAK_MS {
        if let Some((med, _)) = report.linebreak_ms.get(cat) {
            md.push_str(&format!("| {cat} | {paper:.2} ms | {med:.3} ms |\n"));
        }
    }
    md.push_str("\n## Warm-session round trips (apply_edit → ParagraphUpdate in hand)\n\n| Project | Category | Lines | Glyphs | Individual median | P95 | TeX | Traverse | Pack | Amortized median | Overhead share |\n|---|---|---|---|---|---|---|---|---|---|---|\n");
    for r in &report.results {
        md.push_str(&format!("| {} | {} | {} | {} | {:.3} ms | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} ms | {:.0} % |\n", r.project, r.category, r.lines, r.glyphs, r.individual_total.median, r.individual_total.p95, r.individual_tex.median, r.individual_traverse.median, r.individual_pack.median, r.amortized_total.median, r.overhead_share_median * 100.0));
    }
    md.push_str(&format!("\nPaper references: short round trip {:.2} ms, medium {:.2} ms.\n\n## Gates\n\n| Gate | Value | Limit | Pass | Note |\n|---|---|---|---|---|\n", PAPER_ROUNDTRIP_MS[0].1, PAPER_ROUNDTRIP_MS[1].1));
    for g in &report.gates {
        md.push_str(&format!("| {} | {:.3} | {:.3} | {} | {} |\n", g.name, g.value, g.limit, if g.pass { "yes" } else { "**no**" }, g.note));
    }
    md.push_str(&format!("\nOverall: {}\n", if report.pass { "all gates pass" } else { "some gates fail" }));
    std::fs::write(path, md)?;
    Ok(())
}

pub fn write_csv(report: &BenchReport, path: &Path) -> Result<()> {
    let mut s = String::from("project,category,lines,glyphs,individual_median_ms,individual_p5_ms,individual_p95_ms,tex_median_ms,traverse_median_ms,pack_median_ms,amortized_median_ms,amortized_p95_ms,overhead_share\n");
    for r in &report.results {
        s.push_str(&format!("{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n", r.project, r.category, r.lines, r.glyphs, r.individual_total.median, r.individual_total.p5, r.individual_total.p95, r.individual_tex.median, r.individual_traverse.median, r.individual_pack.median, r.amortized_total.median, r.amortized_total.p95, r.overhead_share_median));
    }
    std::fs::write(path, s)?;
    Ok(())
}

pub fn machine_string() -> String {
    let cpu = std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| s.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())).unwrap_or_else(|| "unknown cpu".into());
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    format!("{cpu} ({n} threads)")
}

pub fn run_all(projects: Vec<PathBuf>, categories: Vec<String>, samples: usize, inner: usize, build: PathBuf, out: PathBuf, quick: bool) -> Result<BenchReport> {
    let tl = TexLive::discover()?;
    let repo = tl.lode_texdir.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&build)?;
    std::fs::create_dir_all(&out)?;
    println!("== in-engine line breaking (upstream texlode/luatex-benchmark systematic-benchmark.tex)");
    let lb = linebreak(&tl, &repo, &build.join("linebreak"), quick)?;
    for (cat, paper) in PAPER_LINEBREAK_MS {
        if let Some((med, _)) = lb.get(cat) {
            println!("  {cat:12} here {med:.3} ms   paper {paper:.2} ms");
        }
    }
    let h_short = lb.get("short").map(|v| v.0 / 0.17).unwrap_or(1.0);
    let h_medium = lb.get("medium").map(|v| v.0 / 0.70).unwrap_or(1.0);
    let h = h_short.max(h_medium);
    println!("  hardware factor h = max({h_short:.2}, {h_medium:.2}) = {h:.2}");
    let mut report = BenchReport { machine: machine_string(), hardware_factor: h, linebreak_ms: lb, ..Default::default() };
    for p in &projects {
        println!("== round trips: {}", p.display());
        let mut r = roundtrip(p, &categories, samples, inner, &build)?;
        report.results.append(&mut r);
    }
    gates(&mut report);
    for g in &report.gates {
        println!("  gate {:60} {:8.3} ≤ {:7.3}  {}", g.name, g.value, g.limit, if g.pass { "pass" } else { "FAIL" });
    }
    let stamp = chrono_like_now();
    write_markdown(&report, &out.join(format!("roundtrip-{stamp}.md")))?;
    write_csv(&report, &out.join(format!("roundtrip-{stamp}.csv")))?;
    std::fs::write(out.join(format!("roundtrip-{stamp}.json")), serde_json::to_string_pretty(&report)?)?;
    println!("results written to {}", out.display());
    Ok(report)
}

fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // days since epoch → civil date (no external crate)
    let days = secs / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}", (secs % 86400) / 3600, (secs % 3600) / 60)
}
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
