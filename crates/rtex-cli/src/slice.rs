//! M1 vertical slice: capture → persistent server → display list → independent PDF check → timing.

use anyhow::{bail, Context, Result};
use rtex_core::capture::{run_capture, CaptureResult};
use rtex_core::engine::{FastServer, Response};
use rtex_core::texlive::TexLive;
use rtex_dl::{DisplayList, Item, Line, SP_PER_BP};
use rtex_verify::compare::compare_page;
use rtex_verify::pdftext;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct SliceOpts {
    pub project: PathBuf,
    pub main: String,
    pub paragraph: Option<usize>,
    pub edits: usize,
    pub build: PathBuf,
    pub json_out: Option<PathBuf>,
}

fn stats(v: &mut Vec<f64>) -> (f64, f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let at = |frac: f64| v[((n as f64 * frac).floor() as usize + 1).min(n) - 1];
    (at(0.5), at(0.05), at(0.95), v[n - 1])
}

/// Compare the fast-path paragraph DL (paragraph coordinates) with the capture page lines for
/// the same paragraph (page coordinates), sp-exact after translating by the first line's origin.
fn compare_with_capture(fast: &DisplayList, page: &DisplayList, unit: i64) -> (usize, usize, Vec<String>) {
    let ref_lines: Vec<&Line> = page.rows_of(unit);
    let mut notes = Vec::new();
    if ref_lines.len() != fast.lines.len() {
        notes.push(format!("line count differs: fast {} vs capture {}", fast.lines.len(), ref_lines.len()));
    }
    let Some(first) = ref_lines.first() else { return (0, 0, notes) };
    let fast_first = &fast.lines[0];
    let ox = first.x - fast_first.x;
    let oy = first.y - fast_first.y;
    let mut same = 0;
    let mut total = 0;
    for (fl, rl) in fast.lines.iter().zip(ref_lines.iter()) {
        if fl.x + ox != rl.x || fl.y + oy != rl.y || fl.w != rl.w || fl.h != rl.h || fl.d != rl.d {
            notes.push(format!("line {} box differs: fast ({},{},{},{},{}) capture ({},{},{},{},{})", fl.i, fl.x + ox, fl.y + oy, fl.w, fl.h, fl.d, rl.x, rl.y, rl.w, rl.h, rl.d));
        }
        if (fl.gs - rl.gs).abs() > 1e-12 {
            notes.push(format!("line {} glue_set differs: {} vs {}", fl.i, fl.gs, rl.gs));
        }
        let fg: Vec<&Item> = fl.items.iter().filter(|i| matches!(i, Item::Glyph { .. })).collect();
        let rg: Vec<&Item> = rl.items.iter().filter(|i| matches!(i, Item::Glyph { .. })).collect();
        if fg.len() != rg.len() {
            notes.push(format!("line {} glyph count differs: {} vs {}", fl.i, fg.len(), rg.len()));
        }
        for (a, b) in fg.iter().zip(rg.iter()) {
            total += 1;
            if let (Item::Glyph { font: fa, char: ca, index: ia, x: xa, y: ya, width: wa, expansion: ea }, Item::Glyph { font: fb, char: cb, index: ib, x: xb, y: yb, width: wb, expansion: eb }) = (a, b) {
                let font_ok = match (fast.font(*fa), page.font(*fb)) {
                    (Some(da), Some(db)) => da.key() == db.key(),
                    _ => fa == fb,
                };
                if font_ok && ca == cb && ia == ib && xa + ox == *xb && ya + oy == *yb && wa == wb && ea == eb {
                    same += 1;
                } else if notes.len() < 12 {
                    notes.push(format!("line {} glyph differs: fast {:?} capture {:?} (font_ok={font_ok})", fl.i, a, b));
                }
            }
        }
    }
    (same, total, notes)
}

pub fn run(opts: SliceOpts) -> Result<serde_json::Value> {
    let tl = TexLive::discover()?;
    let project = opts.project.canonicalize()?;
    let build = opts.build.clone();
    std::fs::create_dir_all(&build)?;
    println!("== M1 slice: project {} ({})", project.display(), opts.main);

    // 1. instrumented full compile (reference extractor + contexts + placements)
    let cap: CaptureResult = run_capture(&tl, &project, &opts.main, &build.join("capture"), true)?;
    println!("capture: {} paragraphs, {} pages, {:.2}s, exit_ok={}", cap.json.paragraphs.len(), cap.json.pages, cap.wall.as_secs_f64(), cap.exit_ok);
    // 2. clean compile for the independent PDF check (no capture package)
    let clean = run_capture(&tl, &project, &opts.main, &build.join("clean"), false)?;
    println!("clean build: {:.2}s, exit_ok={}", clean.wall.as_secs_f64(), clean.exit_ok);

    let main_text = std::fs::read_to_string(project.join(&opts.main))?;
    let (preamble, _) = rtex_core::split_preamble(&main_text).context("no \\begin{document}")?;
    let policy = rtex_core::eligibility::Policy::from_preamble(preamble, &[], &[]);
    let (store, fb) = rtex_core::layout::LayoutStore::offline(&cap, &project, &opts.main, &policy)?;
    let candidates: Vec<(&rtex_core::layout::EngineUnit, rtex_core::ParaId)> = store.mapped_units().into_iter().filter(|(u, _)| u.kind() == "par" && u.rows() > 0 && u.first_para.as_ref().map(|p| p.begin.is_some()).unwrap_or(false)).collect();
    if candidates.is_empty() {
        bail!("no paragraph units with placements");
    }
    let pick = opts.paragraph.unwrap_or(candidates.len() / 2).min(candidates.len() - 1);
    let (para, span_id) = candidates[pick];
    let source = fb.span_text(span_id).unwrap_or("").trim_end_matches('\n').to_string();
    let placements = para.captured.placements.clone();
    let page_no = placements[0].page;
    println!("unit {} lines {}..{:?} ({} rows, page {}), {} chars of source", para.uid, para.captured.begin_line, para.captured.end_line, para.rows(), page_no, source.len());

    // 3. persistent server
    let mut server = FastServer::spawn(&tl, &project, &build.join("serve"), preamble, 1)?;
    println!("server ready in {:.2}s: {}", server.startup.as_secs_f64(), server.banner);
    server.set_context(para.uid, &para.context_json())?;
    let ping = server.ping()?;
    println!("ping round trip: {:.3} ms", ping.as_secs_f64() * 1e3);

    // 4. first compile: fidelity checks
    let (res, rt) = server.compile(para.uid, &source)?;
    if res.status == "error" {
        bail!("fast compile reported errors: {:?}", res.errors);
    }
    let fast = res.dl.clone().context("no display list")?;
    println!("fast compile: status={} lines={} glyphs={} tex={:.3}ms traverse={:.3}ms pack={:.3}ms total={:.3}ms flags={:?}",
        res.status, fast.lines.len(), fast.glyph_count(), rt.t_tex.as_secs_f64() * 1e3, rt.t_traverse.as_secs_f64() * 1e3, rt.t_pack.as_secs_f64() * 1e3, rt.total.as_secs_f64() * 1e3, fast.flags_map());

    let page_dl = cap.page(page_no)?;
    let (same, total, notes) = compare_with_capture(&fast, &page_dl, para.uid);
    println!("fast vs capture extractor: {same}/{total} glyphs identical (sp-exact, same font/index/expansion)");
    for n in &notes {
        println!("  note: {n}");
    }

    // 5. independent PDF check: capture page lines of this paragraph vs clean PDF content stream
    let pdf_pages = pdftext::extract(&clean.pdf)?;
    let pdf_page = pdf_pages.iter().find(|p| p.number as i64 == page_no).context("pdf page")?;
    let quantum = 10f64.powi(-(pdf_page.decimals.max(1) as i32));
    let tol = quantum * 1.0;
    let ref_lines = page_dl.rows_of(para.uid);
    let rep_par = compare_page(&page_dl, pdf_page, &ref_lines, tol);
    println!("PDF check (paragraph lines, {} decimals, tol {:.4} bp): matched {}/{} glyphs, within tol {}, max |dx| {:.5} bp, max |dy| {:.5} bp, max scale err {:.6}, max advance err {:.5} bp",
        pdf_page.decimals, tol, rep_par.matched, rep_par.dl_glyphs, rep_par.within_tolerance, rep_par.max_dx_bp, rep_par.max_dy_bp, rep_par.max_scale_err, rep_par.max_advance_err_bp);
    let rep_page = compare_page(&page_dl, pdf_page, &[], tol);
    println!("PDF check (whole page {}): matched {}/{} DL glyphs (PDF has {}), within tol {}, max |dx| {:.5} bp, max |dy| {:.5} bp, rules {}/{} matched",
        page_no, rep_page.matched, rep_page.dl_glyphs, rep_page.pdf_glyphs, rep_page.within_tolerance, rep_page.max_dx_bp, rep_page.max_dy_bp, rep_page.rules_matched, rep_page.rules_dl);
    println!("  anchored glyphs (origin set by Tm/Td): {} with max |dx| {:.5} bp, max |dy| {:.5} bp", rep_page.anchored, rep_page.anchored_max_dx_bp, rep_page.anchored_max_dy_bp);
    println!("  max |dx| by PDF font: {:?}", rep_page.max_dx_by_font.iter().map(|(k, v)| (k.split('+').last().unwrap_or(k).to_string(), (v * 1e5).round() / 1e5)).collect::<Vec<_>>());
    for w in rep_page.worst.iter().take(3) {
        println!("  worst: line {} idx {:?} font {:?} anchored={} dl=({:.4},{:.4}) pdf=({:.4},{:.4}) dx={:.5} dy={:.5}", w.line, w.index, w.pdf_font, w.anchored, w.dl_x_bp, w.dl_y_bp, w.pdf_x_bp.unwrap_or(0.0), w.pdf_y_bp.unwrap_or(0.0), w.dx_bp.unwrap_or(0.0), w.dy_bp.unwrap_or(0.0));
    }
    for m in &rep_page.rule_mismatches {
        println!("  rule mismatch: {m}");
    }
    // The fast-path DL itself vs PDF: translate paragraph coords to page coords via placement 1.
    let mut fast_on_page = fast.clone();
    let ox = placements[0].x - fast.lines[0].x;
    let oy = placements[0].y - fast.lines[0].y;
    for l in &mut fast_on_page.lines {
        l.x += ox;
        l.y += oy;
        for it in &mut l.items {
            match it {
                Item::Glyph { x, y, .. } => { *x += ox; *y += oy; }
                Item::Rule { x, y_top, .. } => { *x += ox; *y_top += oy; }
                Item::Math { x, .. } => { *x += ox; }
                Item::Image { x, y_top, .. } => { *x += ox; *y_top += oy; }
                _ => {}
            }
        }
    }
    let fast_lines: Vec<&Line> = fast_on_page.lines.iter().collect();
    let rep_fast = compare_page(&fast_on_page, pdf_page, &fast_lines, tol);
    println!("PDF check (fast-path DL anchored at placement): matched {}/{} glyphs, within tol {}, max |dx| {:.5} bp, max |dy| {:.5} bp",
        rep_fast.matched, rep_fast.dl_glyphs, rep_fast.within_tolerance, rep_fast.max_dx_bp, rep_fast.max_dy_bp);

    // 6. edit loop timing: individual edits (toggle a trailing word) and amortized
    let mut totals = Vec::new();
    let mut tex = Vec::new();
    let mut trav = Vec::new();
    let mut pack = Vec::new();
    let words: Vec<&str> = source.split_whitespace().collect();
    let t_loop = Instant::now();
    for i in 0..opts.edits {
        let edited = if i % 2 == 1 { format!("{source} edit{i}") } else { source.clone() };
        let (r, rt) = server.compile(para.uid, &edited)?;
        if r.status == "error" {
            bail!("edit {i} failed: {:?}", r.errors);
        }
        totals.push(rt.total.as_secs_f64() * 1e3);
        tex.push(rt.t_tex.as_secs_f64() * 1e3);
        trav.push(rt.t_traverse.as_secs_f64() * 1e3);
        pack.push(rt.t_pack.as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = words;
    let loop_wall = t_loop.elapsed();
    let (med, p5, p95, max) = stats(&mut totals);
    let (tmed, _, tp95, _) = stats(&mut tex);
    let (vmed, _, vp95, _) = stats(&mut trav);
    let (pmed, _, pp95, _) = stats(&mut pack);
    println!("round trip over {} individual edits: median {:.3} ms, P5 {:.3}, P95 {:.3}, max {:.3} (loop wall {:.2}s)", opts.edits, med, p5, p95, max, loop_wall.as_secs_f64());
    println!("  stages median/P95: tex {:.3}/{:.3} ms, traverse {:.3}/{:.3} ms, pack {:.3}/{:.3} ms, ipc+parse ≈ {:.3} ms", tmed, tp95, vmed, vp95, pmed, pp95, med - tmed - vmed - pmed);
    // amortized: 10 samples × 20 compiles back to back
    let mut amort = Vec::new();
    for _ in 0..10 {
        let t0 = Instant::now();
        for _ in 0..20 {
            server.compile(para.uid, &source)?;
        }
        amort.push(t0.elapsed().as_secs_f64() * 1e3 / 20.0);
    }
    let (amed, _, ap95, _) = stats(&mut amort);
    println!("amortized (mean of 20 back-to-back, 10 samples): median {:.3} ms, P95 {:.3} ms", amed, ap95);

    // 7. error path and engine health
    let (bad, _) = server.compile(para.uid, "Text with \\undefinedmacro inside.")?;
    println!("error path: status={} errors={:?}", bad.status, bad.errors.iter().map(|e| (e.message.clone(), e.line)).collect::<Vec<_>>());
    let (ok_again, _) = server.compile(para.uid, &source)?;
    println!("after error: status={} lines={}", ok_again.status, ok_again.dl.as_ref().map(|d| d.lines.len()).unwrap_or(0));
    let st = server.stats()?;
    if let Response::Stats { requests, font_nextid, grouplevel, nest, .. } = &st {
        println!("engine stats: requests={requests} font_nextid={font_nextid} grouplevel={grouplevel} nest={nest}");
    }
    server.shutdown()?;

    let report = serde_json::json!({
        "project": project, "paragraph_seq": para.uid, "source_lines": [para.captured.begin_line, para.captured.end_line],
        "typeset_lines": fast.lines.len(), "glyphs": fast.glyph_count(),
        "fast_vs_capture": {"identical": same, "total": total, "notes": notes},
        "pdf_check_paragraph": rep_par, "pdf_check_page": rep_page, "pdf_check_fast": rep_fast,
        "roundtrip_ms": {"median": med, "p5": p5, "p95": p95, "max": max, "edits": opts.edits,
                         "tex_median": tmed, "traverse_median": vmed, "pack_median": pmed,
                         "amortized_median": amed, "amortized_p95": ap95},
        "server_startup_s": server_startup_placeholder(),
        "capture_s": cap.wall.as_secs_f64(), "clean_build_s": clean.wall.as_secs_f64(),
    });
    if let Some(p) = &opts.json_out {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)?;
        println!("report written to {}", p.display());
    }
    let _ = SP_PER_BP;
    Ok(report)
}

fn server_startup_placeholder() -> f64 {
    0.0
}
