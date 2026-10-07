//! `lode verify`: run the fidelity layers over a whole project.
//!   layer 1: every fast-eligible paragraph through the persistent server vs the shipout extractor
//!   layer 2: every page's display list vs the clean PDF's content stream (independent parser)
//!   layer 3: every page rasterized from the display list vs PyMuPDF's raster of the PDF

use anyhow::{Context, Result};
use lode_core::capture::run_capture;
use lode_core::eligibility::{check_engine, check_source, Policy};
use lode_core::engine::FastServer;
use lode_core::texlive::TexLive;
use lode_dl::{DisplayList, Item, Line};
use lode_verify::compare::compare_page;
use lode_verify::{pdftext, raster};
use serde::Serialize;
use std::path::PathBuf;

pub struct VerifyOpts {
    pub project: PathBuf,
    pub main: String,
    pub build: PathBuf,
    pub dpi: u32,
    pub raster: bool,
    pub json_out: Option<PathBuf>,
    pub max_paragraphs: Option<usize>,
}

#[derive(Serialize, Default)]
pub struct ParagraphVerdict {
    pub seq: i64,
    pub lines: usize,
    pub glyphs_total: usize,
    pub glyphs_identical: usize,
    pub status: String,
    pub notes: Vec<String>,
    pub t_total_ms: f64,
}

#[derive(Serialize, Default)]
pub struct PageVerdict {
    pub page: i64,
    pub dl_glyphs: usize,
    pub pdf_glyphs: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub anchored_max_dx_bp: f64,
    pub interior_max_dx_bp: f64,
    pub max_dy_bp: f64,
    pub tj_quantum_bp: f64,
    pub rules_dl: usize,
    pub rules_matched: usize,
    pub colors_dl: usize,
    pub colors_pdf: usize,
    pub exact: bool,
    pub flags: serde_json::Value,
    pub raster_unmatched_fraction: Option<f64>,
    pub raster_glyphs_skipped: Option<usize>,
    pub images_dl: usize,
    pub images_matched: usize,
    pub images_pdf: usize,
}

#[derive(Serialize, Default)]
pub struct Report {
    pub paragraphs_total: usize,
    pub paragraphs_eligible: usize,
    pub ineligible_reasons: std::collections::BTreeMap<String, usize>,
    pub paragraphs: Vec<ParagraphVerdict>,
    pub pages: Vec<PageVerdict>,
    pub pages_degraded: usize,
    pub capture_pdf_equals_clean: bool,
    pub layer1_pass: bool,
    pub layer2_pass: bool,
    pub layer3_pass: Option<bool>,
}

/// Compare a fast-path paragraph display list with the capture's lines for the same paragraph,
/// which may be spread over several pages. Each page fragment gets its own vertical offset
/// (taken from its first line); the horizontal offset comes from the paragraph's first line.
fn compare_with_capture(fast: &DisplayList, pages: &[(i64, &DisplayList)], par: i64) -> (usize, usize, Vec<String>) {
    let mut ref_lines: Vec<(i64, &Line)> = Vec::new();
    for (pno, page) in pages {
        for l in page.lines_of(par) {
            ref_lines.push((*pno, l));
        }
    }
    ref_lines.sort_by_key(|(_, l)| l.i);
    let mut notes = Vec::new();
    if ref_lines.len() != fast.lines.len() {
        notes.push(format!("line count: fast {} vs capture {}", fast.lines.len(), ref_lines.len()));
    }
    if ref_lines.is_empty() || fast.lines.is_empty() {
        return (0, 0, notes);
    }
    // book-class margins alternate between odd and even pages, so each page fragment has its
    // own (x, y) offset, taken from its first line.
    let mut off_by_page: std::collections::BTreeMap<i64, (i64, i64)> = std::collections::BTreeMap::new();
    let (mut same, mut total) = (0, 0);
    for (fl, (pno, rl)) in fast.lines.iter().zip(ref_lines.iter()) {
        let page: &DisplayList = pages.iter().find(|(p, _)| p == pno).map(|(_, d)| *d).unwrap();
        let (ox, oy) = *off_by_page.entry(*pno).or_insert((rl.x - fl.x, rl.y - fl.y));
        if fl.x + ox != rl.x || fl.y + oy != rl.y || fl.w != rl.w || fl.h != rl.h || fl.d != rl.d || (fl.gs - rl.gs).abs() > 1e-12 {
            notes.push(format!("line {} box/glue differs", fl.i));
        }
        let fg: Vec<&Item> = fl.items.iter().filter(|i| matches!(i, Item::Glyph { .. })).collect();
        let rg: Vec<&Item> = rl.items.iter().filter(|i| matches!(i, Item::Glyph { .. })).collect();
        if fg.len() != rg.len() {
            notes.push(format!("line {} glyph count {} vs {}", fl.i, fg.len(), rg.len()));
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
                } else if notes.len() < 6 {
                    notes.push(format!("line {} glyph differs: fast {:?} capture {:?}", fl.i, a, b));
                }
            }
        }
        // rules and colors inside lines
        let fr = fl.items.iter().filter(|i| matches!(i, Item::Rule { .. })).count();
        let rr = rl.items.iter().filter(|i| matches!(i, Item::Rule { .. })).count();
        if fr != rr {
            notes.push(format!("line {} rule count {} vs {}", fl.i, fr, rr));
        }
    }
    (same, total, notes)
}

pub fn run(opts: VerifyOpts) -> Result<Report> {
    let tl = TexLive::discover()?;
    let project = opts.project.canonicalize()?;
    std::fs::create_dir_all(&opts.build)?;
    println!("== verify {} ({})", project.display(), opts.main);
    // bibliography support for mixed fixtures: run biber when a .bcf shows up after the first pass
    let cap = run_capture(&tl, &project, &opts.main, &opts.build.join("capture"), true)?;
    let bcf = opts.build.join("capture").join(format!("{}.bcf", cap.jobname));
    let cap = if bcf.exists() {
        let _ = std::process::Command::new("biber").current_dir(opts.build.join("capture")).arg(&cap.jobname).output();
        run_capture(&tl, &project, &opts.main, &opts.build.join("capture"), true)?;
        run_capture(&tl, &project, &opts.main, &opts.build.join("capture"), true)?
    } else {
        cap
    };
    let clean = run_capture(&tl, &project, &opts.main, &opts.build.join("clean"), false)?;
    let clean_bcf = opts.build.join("clean").join(format!("{}.bcf", clean.jobname));
    let clean = if clean_bcf.exists() {
        let _ = std::process::Command::new("biber").current_dir(opts.build.join("clean")).arg(&clean.jobname).output();
        run_capture(&tl, &project, &opts.main, &opts.build.join("clean"), false)?;
        run_capture(&tl, &project, &opts.main, &opts.build.join("clean"), false)?
    } else {
        clean
    };
    println!("capture: {} paragraphs, {} pages ({:.1}s); clean build {:.1}s", cap.json.paragraphs.len(), cap.json.pages, cap.wall.as_secs_f64(), clean.wall.as_secs_f64());
    let mut report = Report { paragraphs_total: cap.json.paragraphs.len(), ..Default::default() };
    // T3: the capture package must not change the output
    let t3 = lode_verify::pdfcompare::compare(&cap.pdf, &clean.pdf)?;
    report.capture_pdf_equals_clean = t3.equal;
    println!("T3 (instrumented PDF == clean PDF): {}{}", t3.equal, if t3.equal { String::new() } else { format!(" {:?}", t3.differences.iter().take(3).collect::<Vec<_>>()) });

    // ---- eligibility + layer 1 ----
    let policy = Policy::default();
    let mut server: Option<FastServer> = None;
    let main_text = std::fs::read_to_string(project.join(&opts.main))?;
    let (preamble, _) = lode_core::split_preamble(&main_text).context("no \\begin{document}")?;
    let mut page_cache: std::collections::BTreeMap<i64, DisplayList> = std::collections::BTreeMap::new();
    let mut checked = 0usize;
    for p in &cap.json.paragraphs {
        let mut reasons = check_engine(&p.groupcode, p.nest, &p.everypar, p.begin.is_some(), &p.flags);
        let src = if p.begin.is_some() { crate::slice::paragraph_source(&project, p).ok() } else { None };
        match &src {
            Some(s) => reasons.extend(check_source(s, &policy)),
            None => reasons.push(lode_core::eligibility::Reason::NoContext),
        }
        if p.placements.as_ref().map(|v| v.is_empty()).unwrap_or(true) {
            continue; // never shipped (e.g. inside a box that went nowhere); nothing to compare
        }
        if !reasons.is_empty() {
            for r in reasons {
                let key = match r {
                    lode_core::eligibility::Reason::DisallowedMacro(m) => format!("macro \\{m}"),
                    lode_core::eligibility::Reason::DisallowedMathMacro(m) => format!("math macro \\{m}"),
                    other => format!("{other:?}"),
                };
                *report.ineligible_reasons.entry(key).or_default() += 1;
            }
            continue;
        }
        report.paragraphs_eligible += 1;
        if let Some(max) = opts.max_paragraphs {
            if checked >= max {
                continue;
            }
        }
        checked += 1;
        let src = src.unwrap();
        if server.is_none() {
            let s = FastServer::spawn(&tl, &project, &opts.build.join("serve"), preamble, 1)?;
            println!("server ready in {:.2}s", s.startup.as_secs_f64());
            server = Some(s);
        }
        let srv = server.as_mut().unwrap();
        srv.set_context(p.seq, &p.context_json())?;
        let (res, rt) = match srv.compile(p.seq, &src) {
            Ok(x) => x,
            Err(e) => {
                report.paragraphs.push(ParagraphVerdict { seq: p.seq, status: format!("engine error: {e}"), ..Default::default() });
                server = None;
                continue;
            }
        };
        let mut v = ParagraphVerdict { seq: p.seq, status: res.status.clone(), t_total_ms: rt.total.as_secs_f64() * 1e3, ..Default::default() };
        if let Some(fast) = &res.dl {
            let mut pnos: Vec<i64> = p.placements.as_ref().unwrap().iter().map(|pl| pl.page).collect();
            pnos.sort();
            pnos.dedup();
            for pno in &pnos {
                if !page_cache.contains_key(pno) {
                    page_cache.insert(*pno, cap.page(*pno)?);
                }
            }
            let pages: Vec<(i64, &DisplayList)> = pnos.iter().map(|pno| (*pno, &page_cache[pno])).collect();
            let (same, total, notes) = compare_with_capture(fast, &pages, p.seq);
            v.lines = fast.lines.len();
            v.glyphs_identical = same;
            v.glyphs_total = total;
            v.notes = notes;
        } else {
            v.notes.push(format!("errors: {:?}", res.errors));
        }
        report.paragraphs.push(v);
    }
    if let Some(mut s) = server {
        s.shutdown()?;
    }
    let l1_total: usize = report.paragraphs.iter().map(|p| p.glyphs_total).sum();
    let l1_same: usize = report.paragraphs.iter().map(|p| p.glyphs_identical).sum();
    let l1_bad: Vec<&ParagraphVerdict> = report.paragraphs.iter().filter(|p| p.glyphs_identical != p.glyphs_total || !p.notes.is_empty() || p.status != "ok").collect();
    report.layer1_pass = l1_bad.is_empty() && !report.paragraphs.is_empty();
    println!("layer 1 (fast vs extractor): {} eligible of {} paragraphs; {} compiled; {}/{} glyphs identical; {} paragraphs with differences",
        report.paragraphs_eligible, report.paragraphs_total, report.paragraphs.len(), l1_same, l1_total, l1_bad.len());
    for p in l1_bad.iter().take(5) {
        println!("  seq {}: status {} {:?}", p.seq, p.status, p.notes.iter().take(3).collect::<Vec<_>>());
    }
    if !report.ineligible_reasons.is_empty() {
        println!("  ineligible reasons: {:?}", report.ineligible_reasons);
    }

    // ---- layer 2 + 3: pages ----
    let pdf_pages = pdftext::extract(&clean.pdf)?;
    let mut cache = raster::FontCache::default();
    let mut l2_ok = true;
    let mut l3_ok: Option<bool> = if opts.raster { Some(true) } else { None };
    for pdf_page in &pdf_pages {
        let page_no = pdf_page.number as i64;
        let dl = cap.page(page_no)?;
        let dec_quantum = 10f64.powi(-(pdf_page.decimals.max(1) as i32));
        let rep = compare_page(&dl, pdf_page, &[], dec_quantum);
        // TJ quantum for the largest text size on the page
        let max_size = pdf_page.glyphs.iter().map(|g| g.size).fold(0.0, f64::max);
        let tj_quantum = max_size / 1000.0;
        let interior_max = rep.max_dx_bp;
        let colors_dl = dl.lines.iter().flat_map(|l| l.items.iter()).chain(dl.other.iter()).filter(|i| matches!(i, Item::Color { .. })).count();
        let dl_images: Vec<(f64, f64, f64, f64)> = dl.lines.iter().flat_map(|l| l.items.iter()).chain(dl.other.iter()).filter_map(|i| match i {
            Item::Image { x, y_top, width, height, .. } => Some((*x as f64 / lode_dl::SP_PER_BP, pdf_page.height - (*y_top + *height) as f64 / lode_dl::SP_PER_BP, *width as f64 / lode_dl::SP_PER_BP, *height as f64 / lode_dl::SP_PER_BP)),
            _ => None,
        }).collect();
        let images_matched = dl_images.iter().filter(|(x, y, w, h)| pdf_page.images.iter().any(|im| (im.x - x).abs() < 0.01 && (im.y - y).abs() < 0.01 && (im.w - w).abs() < 0.01 && (im.h - h).abs() < 0.01)).count();
        let mut pv = PageVerdict {
            page: page_no, dl_glyphs: rep.dl_glyphs, pdf_glyphs: rep.pdf_glyphs, matched: rep.matched, unmatched: rep.unmatched,
            anchored_max_dx_bp: rep.anchored_max_dx_bp, interior_max_dx_bp: interior_max, max_dy_bp: rep.max_dy_bp, tj_quantum_bp: tj_quantum,
            rules_dl: rep.rules_dl, rules_matched: rep.rules_matched, colors_dl, colors_pdf: pdf_page.color_ops.len(),
            exact: dl.is_exact(), flags: dl.flags.clone(), images_dl: dl_images.len(), images_matched, images_pdf: pdf_page.images.len(), ..Default::default()
        };
        let page_ok = rep.unmatched == 0 && rep.dl_glyphs == rep.pdf_glyphs && rep.anchored_max_dx_bp <= dec_quantum * 1.5 && interior_max <= tj_quantum * 1.05 && rep.max_dy_bp <= dec_quantum * 1.5 && rep.rules_matched == rep.rules_dl && images_matched == dl_images.len() && dl_images.len() == pdf_page.images.len();
        let degraded = !dl.is_exact();
        if !page_ok && !degraded {
            l2_ok = false;
        }
        if degraded {
            report.pages_degraded += 1;
            println!("  page {page_no}: DEGRADED {} (host falls back to the PDF page); glyphs matched {}/{} (pdf {})", dl.flags, rep.matched, rep.dl_glyphs, rep.pdf_glyphs);
        }
        if opts.raster {
            let ref_png = opts.build.join(format!("ref-p{page_no}.png"));
            raster::render_pdf_page_with_pymupdf(&clean.pdf, pdf_page.number, opts.dpi, &ref_png)?;
            let (ref_gray, rw, rh) = raster::read_png_gray(&ref_png)?;
            let (dl_gray, w, h, stats) = raster::rasterize(&[&dl], pdf_page.width, pdf_page.height, opts.dpi as f64, &mut cache)?;
            raster::write_png(&opts.build.join(format!("dl-p{page_no}.png")), &dl_gray, w, h)?;
            if (rw, rh) == (w, h) {
                let diff = raster::compare(&dl_gray, &ref_gray, w, h, 96);
                pv.raster_unmatched_fraction = Some(diff.unmatched_fraction);
                pv.raster_glyphs_skipped = Some(stats.glyphs_skipped);
                if (diff.unmatched_fraction > 0.01 || stats.glyphs_skipped > 0) && !degraded {
                    l3_ok = Some(false);
                    println!("  page {page_no} raster: unmatched {:.3}% (A {} B {} unmatched {}+{}), skipped glyphs {} {:?}, bbox {:?}", diff.unmatched_fraction * 100.0, diff.ink_a, diff.ink_b, diff.unmatched_a, diff.unmatched_b, stats.glyphs_skipped, stats.skipped_fonts, diff.bbox_unmatched);
                }
            } else {
                l3_ok = Some(false);
                println!("  page {page_no} raster size mismatch {w}x{h} vs {rw}x{rh}");
            }
        }
        if !page_ok && !degraded {
            println!("  page {page_no}: matched {}/{} (pdf {}), anchored max dx {:.5}, interior max dx {:.5} (tj {:.5}), dy {:.5}, rules {}/{}, images {}/{} (pdf {}), flags {}",
                rep.matched, rep.dl_glyphs, rep.pdf_glyphs, rep.anchored_max_dx_bp, interior_max, tj_quantum, rep.max_dy_bp, rep.rules_matched, rep.rules_dl, images_matched, dl_images.len(), pdf_page.images.len(), dl.flags);
            for w in rep.worst.iter().take(2) {
                println!("    worst: par {} line {} idx {:?} font {:?} dl=({:.4},{:.4}) pdf=({:.4},{:.4}) dx={:.5}", w.par, w.line, w.index, w.pdf_font, w.dl_x_bp, w.dl_y_bp, w.pdf_x_bp.unwrap_or(0.0), w.pdf_y_bp.unwrap_or(0.0), w.dx_bp.unwrap_or(f64::NAN));
            }
        }
        report.pages.push(pv);
    }
    report.layer2_pass = l2_ok;
    report.layer3_pass = l3_ok;
    let pages_exact = report.pages.iter().filter(|p| p.exact).count();
    println!("layer 2 (pages vs PDF content stream): {} pages, {} exact, {} degraded (PDF fallback), pass={}; worst anchored dx {:.5} bp, worst interior dx {:.5} bp",
        report.pages.len(), pages_exact, report.pages_degraded, l2_ok,
        report.pages.iter().map(|p| p.anchored_max_dx_bp).fold(0.0, f64::max), report.pages.iter().map(|p| p.interior_max_dx_bp).fold(0.0, f64::max));
    if opts.raster {
        let worst = report.pages.iter().filter_map(|p| p.raster_unmatched_fraction).fold(0.0, f64::max);
        println!("layer 3 (rendered, {} dpi): pass={:?}; worst unmatched ink fraction {:.3}%", opts.dpi, l3_ok, worst * 100.0);
    }
    if let Some(p) = &opts.json_out {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(report)
}
