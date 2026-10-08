//! `rtex verify`: run the fidelity layers over a whole project.
//!   layer 1: every fast-eligible paragraph through the persistent server vs the shipout extractor
//!   layer 2: every page's display list vs the clean PDF's content stream (independent parser)
//!   layer 3: every page rasterized from the display list vs PyMuPDF's raster of the PDF

use anyhow::Result;
use rtex_core::background::{run_pass_with, BibTool};
use rtex_core::eligibility::{check_engine_unit, classify_source, Policy, UnitShape};
use rtex_core::engine::FastServer;
use rtex_core::layout::LayoutStore;
use rtex_core::texlive::TexLive;
use rtex_dl::{DisplayList, Item, Line};
use rtex_verify::compare::compare_page;
use rtex_verify::{pdftext, raster};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub struct VerifyOpts {
    pub project: PathBuf,
    pub main: String,
    pub build: PathBuf,
    pub dpi: u32,
    pub raster: bool,
    pub json_out: Option<PathBuf>,
    pub max_paragraphs: Option<usize>,
    /// Only these unit ids (layer 1).
    pub units: Vec<i64>,
    /// Print fast and capture rows of differing units as text.
    pub dump_rows: bool,
    /// Fail unless at least this many units are eligible (regression gate).
    pub min_eligible: Option<usize>,
    /// Research: ignore the allow-list (unknown macros/environments are eligible) and let the
    /// row comparison judge every unit.
    pub permissive: bool,
    /// Pass layer 1 as long as at most this many units differ (probe-mode fixtures: the units
    /// known to be state-dependent must be caught, not absent).
    pub max_differing: Option<usize>,
    /// Pass layer 1 only with exactly this many differing units: the fixture's state-dependent
    /// units must be caught, neither more nor fewer.
    pub expect_differing: Option<usize>,
    /// Picture cache check: a second capture pass takes every cacheable picture from the
    /// first pass's PDF; units and placements must be identical, and (with `raster`) the two
    /// PDFs must render alike.
    pub pic_cache: bool,
    /// With `pic_cache`: fail unless at least this many pictures were recorded and reused.
    pub min_cached: Option<usize>,
}

#[derive(Serialize, Default)]
pub struct ParagraphVerdict {
    pub seq: i64,
    pub kind: String,
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
    pub pic_cache: Option<PicCacheReport>,
}

/// Result of the picture cache check (`--pic-cache`).
#[derive(Debug, Default, Serialize)]
pub struct PicCacheReport {
    /// Picture environments in the sources, and how many of them the cache may hold.
    pub pictures: usize,
    pub cacheable: usize,
    /// Pictures the first pass recorded (drawn as one box) and the second pass took from it.
    pub recorded: usize,
    pub hits: usize,
    /// Cached picture images the second pass's pages contain.
    pub cached_images: usize,
    /// Cached pictures whose skipped body did not end where the source scan said.
    pub mismatched: usize,
    pub units_equal: bool,
    pub placements_equal: bool,
    /// Worst unmatched ink fraction between the two passes' rendered pages (with `raster`).
    pub raster_unmatched_fraction: Option<f64>,
    pub wall_uncached_ms: u64,
    pub wall_cached_ms: u64,
    pub pass: bool,
}

use rtex_core::layout::compare_unit_rows as compare_rows;

fn row_text(l: &Line) -> String {
    let mut s = String::new();
    let mut last_x: Option<(i64, i64)> = None;
    for it in &l.items {
        if let Item::Glyph { char, x, width, .. } = it {
            if let Some((lx, lw)) = last_x {
                if *x - (lx + lw) > 60000 {
                    s.push(' ');
                }
            }
            s.push(char::from_u32(*char as u32).unwrap_or('?'));
            last_x = Some((*x, *width));
        }
    }
    s
}

pub fn run(opts: VerifyOpts) -> Result<Report> {
    let tl = TexLive::discover()?;
    let project = opts.project.canonicalize()?;
    std::fs::create_dir_all(&opts.build)?;
    println!("== verify {} ({})", project.display(), opts.main);
    // bibliography support for mixed fixtures: run biber when a .bcf shows up after the first pass
    let preamble = rtex_core::project_preamble(&project, &opts.main)?;
    let mut policy = Policy::from_preamble(&preamble, &[], &[]);
    policy.permissive = opts.permissive;
    let unit_envs = policy.unit_envs_env();
    // Both builds run to aux convergence (labels, TOC, bibliography) exactly like the
    // background compiler does: a single first pass would leave every \ref as "??" while
    // the fast server resolves the labels from the aux that pass writes.
    const MAX_PASSES: u32 = 5;
    let cap_pass = run_pass_with(
        &tl,
        &project,
        &opts.main,
        &opts.build.join("capture"),
        MAX_PASSES,
        BibTool::Auto,
        true,
        &unit_envs,
    )?;
    let clean_pass = run_pass_with(
        &tl,
        &project,
        &opts.main,
        &opts.build.join("clean"),
        MAX_PASSES,
        BibTool::Auto,
        false,
        "",
    )?;
    if !cap_pass.aux_stable || !clean_pass.aux_stable {
        println!(
            "warning: aux family not stable after {MAX_PASSES} passes (capture {}, clean {})",
            cap_pass.aux_stable, clean_pass.aux_stable
        );
    }
    let (cap, clean) = (cap_pass.capture, clean_pass.capture);
    println!(
        "capture: {} paragraphs, {} pages ({:.1}s, {} passes); clean build {:.1}s ({} passes)",
        cap.json.paragraphs.len(),
        cap.json.pages,
        cap.wall.as_secs_f64(),
        cap_pass.passes,
        clean.wall.as_secs_f64(),
        clean_pass.passes
    );
    let mut report = Report {
        paragraphs_total: cap.json.paragraphs.len(),
        ..Default::default()
    };
    // T3: the capture package must not change the output
    let t3 = rtex_verify::pdfcompare::compare(&cap.pdf, &clean.pdf)?;
    report.capture_pdf_equals_clean = t3.equal;
    println!(
        "T3 (instrumented PDF == clean PDF): {}{}",
        t3.equal,
        if t3.equal {
            String::new()
        } else {
            format!(" {:?}", t3.differences.iter().take(3).collect::<Vec<_>>())
        }
    );

    // ---- eligibility + layer 1 (per unit) ----
    let (store, fb) = LayoutStore::offline(&cap, &project, &opts.main, &policy)?;
    let mut server: Option<FastServer> = None;
    let mut page_cache: std::collections::BTreeMap<i64, DisplayList> =
        std::collections::BTreeMap::new();
    let mut checked = 0usize;
    report.paragraphs_total = cap.json.units.len();
    for (eu, span_id) in store.mapped_units() {
        let c = &eu.captured;
        let src = fb.fast_source(span_id).unwrap_or_default();
        let (shape, mut reasons) = classify_source(&src, &policy);
        let has_ctx = eu.has_context();
        reasons.extend(check_engine_unit(
            &c.kind,
            &c.everypar,
            has_ctx,
            eu.rows(),
            &eu.flags,
        ));
        let shape_ok = match (&shape, c.kind.as_str()) {
            (UnitShape::Par, "par") => true,
            (UnitShape::Env(n), "env") => Some(n.as_str()) == c.name.as_deref(),
            (UnitShape::Heading(n), "heading") => Some(n.as_str()) == c.name.as_deref(),
            _ => false,
        };
        if !shape_ok {
            reasons.push(rtex_core::eligibility::Reason::KindMismatch);
        }
        if c.placements.is_empty() {
            continue; // never shipped; nothing to compare
        }
        if !opts.units.is_empty() && !opts.units.contains(&eu.uid) {
            continue;
        }
        if !reasons.is_empty() {
            for r in reasons {
                let key = match r {
                    rtex_core::eligibility::Reason::DisallowedMacro(m) => format!("macro \\{m}"),
                    rtex_core::eligibility::Reason::DisallowedMathMacro(m) => {
                        format!("math macro \\{m}")
                    }
                    rtex_core::eligibility::Reason::NonDefaultEverypar => format!(
                        "NonDefaultEverypar [{}]",
                        c.everypar.chars().take(70).collect::<String>()
                    ),
                    rtex_core::eligibility::Reason::KindMismatch => format!(
                        "KindMismatch (source {:?}, capture {}:{}, text {:?})",
                        shape,
                        c.kind,
                        c.name.clone().unwrap_or_default(),
                        src.chars().take(50).collect::<String>()
                    ),
                    rtex_core::eligibility::Reason::NoContext => format!(
                        "NoContext ({}:{} at line {}, text {:?})",
                        c.kind,
                        c.name.clone().unwrap_or_default(),
                        c.begin_line,
                        src.chars().take(40).collect::<String>()
                    ),
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
        if server.is_none() {
            let s = FastServer::spawn(
                &tl,
                &project,
                &opts.build.join("serve"),
                &preamble,
                1,
                Some(&cap.out_dir.join(format!("{}.aux", cap.jobname))),
            )?;
            println!("server ready in {:.2}s", s.startup.as_secs_f64());
            let labels = rtex_core::background::read_aux_labels(
                &cap.out_dir.join(format!("{}.aux", cap.jobname)),
            );
            let mut s = s;
            if !labels.is_empty() {
                s.set_labels(&labels)?;
            }
            server = Some(s);
        }
        let srv = server.as_mut().unwrap();
        srv.set_context(eu.uid, &eu.context_json())?;
        let kind_name = match c.kind.as_str() {
            "par" => "par".to_string(),
            k => format!("{k}:{}", c.name.clone().unwrap_or_default()),
        };
        let (res, rt) = match srv.compile(eu.uid, &src) {
            Ok(x) => x,
            Err(e) => {
                report.paragraphs.push(ParagraphVerdict {
                    seq: eu.uid,
                    kind: kind_name,
                    status: format!("engine error: {e}"),
                    ..Default::default()
                });
                server = None;
                continue;
            }
        };
        let mut v = ParagraphVerdict {
            seq: eu.uid,
            kind: kind_name,
            status: res.status.clone(),
            t_total_ms: rt.total.as_secs_f64() * 1e3,
            ..Default::default()
        };
        if let Some(fast) = &res.dl {
            let mut pnos: Vec<i64> = c.placements.iter().map(|pl| pl.page).collect();
            pnos.sort();
            pnos.dedup();
            for pno in &pnos {
                if !page_cache.contains_key(pno) {
                    page_cache.insert(*pno, cap.page(*pno)?);
                }
            }
            let pages: Vec<(i64, &DisplayList)> =
                pnos.iter().map(|pno| (*pno, &page_cache[pno])).collect();
            let (same, total, notes) = compare_rows(fast, &pages, &eu.uids);
            if opts.dump_rows && (same != total || !notes.is_empty()) {
                println!("--- unit {} ({}) fast rows:", eu.uid, c.kind);
                for (k, l) in fast.lines.iter().enumerate() {
                    println!(
                        "  f{:2} x={} y={} w={} h={} gs={:.4}: {}",
                        k + 1,
                        l.x,
                        l.y,
                        l.w,
                        l.h,
                        l.gs,
                        row_text(l)
                    );
                }
                println!("--- capture rows:");
                for uid in &eu.uids {
                    for (_, pg) in &pages {
                        for l in pg.rows_of(*uid) {
                            println!(
                                "  c{:2} x={} y={} w={} h={} gs={:.4}: {}",
                                l.row,
                                l.x,
                                l.y,
                                l.w,
                                l.h,
                                l.gs,
                                row_text(l)
                            );
                        }
                    }
                }
            }
            v.lines = fast.lines.len();
            v.glyphs_identical = same;
            v.glyphs_total = total;
            v.notes = notes;
            if res.status == "ok_degraded" {
                v.notes.push(format!(
                    "degraded: {:?}",
                    fast.flags_map().keys().collect::<Vec<_>>()
                ));
            }
            if res.status == "error" {
                v.notes.insert(
                    0,
                    format!(
                        "engine errors: {:?}",
                        res.errors
                            .iter()
                            .map(|e| format!(
                                "{} | {}",
                                e.message.clone().unwrap_or_default(),
                                e.context
                                    .clone()
                                    .unwrap_or_default()
                                    .chars()
                                    .take(160)
                                    .collect::<String>()
                            ))
                            .collect::<Vec<_>>()
                    ),
                );
            }
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
    let l1_bad: Vec<&ParagraphVerdict> = report
        .paragraphs
        .iter()
        .filter(|p| {
            p.glyphs_identical != p.glyphs_total
                || p.notes.iter().any(|n| !n.starts_with("degraded"))
                || !(p.status == "ok" || p.status == "ok_degraded")
        })
        .collect();
    if let Some(min) = opts.min_eligible {
        if report.paragraphs_eligible < min {
            anyhow::bail!(
                "eligibility gate failed: {} eligible of {} units, {min} required; reasons: {:?}",
                report.paragraphs_eligible,
                report.paragraphs_total,
                report.ineligible_reasons
            );
        }
    }
    report.layer1_pass = match (opts.expect_differing, opts.max_differing) {
        (Some(n), _) => l1_bad.len() == n && !report.paragraphs.is_empty(),
        (None, Some(max)) => l1_bad.len() <= max && !report.paragraphs.is_empty(),
        (None, None) => l1_bad.is_empty() && !report.paragraphs.is_empty(),
    };
    println!("layer 1 (fast vs extractor): {} eligible of {} units; {} compiled; {}/{} glyphs identical; {} units with differences",
        report.paragraphs_eligible, report.paragraphs_total, report.paragraphs.len(), l1_same, l1_total, l1_bad.len());
    {
        // per kind: count, identical, median round trip
        let mut by_kind: std::collections::BTreeMap<String, Vec<&ParagraphVerdict>> =
            std::collections::BTreeMap::new();
        for p in &report.paragraphs {
            by_kind.entry(p.kind.clone()).or_default().push(p);
        }
        for (k, v) in &by_kind {
            let mut t: Vec<f64> = v.iter().map(|p| p.t_total_ms).collect();
            t.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let bad = v
                .iter()
                .filter(|p| l1_bad.iter().any(|b| b.seq == p.seq))
                .count();
            println!("  {k:16} {:4} units, {:3} with differences, round trip median {:.3} ms, max {:.3} ms", v.len(), bad, t[t.len() / 2], t[t.len() - 1]);
        }
    }
    for p in l1_bad.iter().take(8) {
        println!(
            "  unit {} ({}): status {} {:?}",
            p.seq,
            p.kind,
            p.status,
            p.notes.iter().take(3).collect::<Vec<_>>()
        );
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
        let colors_dl = dl
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .chain(dl.other.iter())
            .filter(|i| matches!(i, Item::Color { .. }))
            .count();
        // image rectangles with the PDF transformation state applied: graphicx scales (and
        // rotates) bitmap images with save / setmatrix / restore around the image. The affine map
        // p' = M p + t is tracked in TeX coordinates (a pure scale about a point has the same
        // form in both orientations; rotations are approximated by their bounding box).
        let dl_images: Vec<(f64, f64, f64, f64)> = {
            let mut out = Vec::new();
            let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            let mut stack: Vec<[f64; 6]> = Vec::new();
            let mut cur = identity;
            let items = dl
                .other
                .iter()
                .chain(dl.lines.iter().flat_map(|l| l.items.iter()));
            for it in items {
                match it {
                    Item::Matrix { op, x, y, data } => match op.as_str() {
                        "save" => stack.push(cur),
                        "restore" => cur = stack.pop().unwrap_or(identity),
                        _ => {
                            let v: Vec<f64> = data
                                .split_whitespace()
                                .filter_map(|t| t.parse().ok())
                                .collect();
                            if v.len() == 4 {
                                let (a2, b2, c2, d2) = (v[0], v[1], v[2], v[3]);
                                let (px, py) = (*x as f64, *y as f64);
                                // T2(p) = M2 p + (P - M2 P); T = T1 ∘ T2
                                let [a1, b1, c1, d1, tx1, ty1] = cur;
                                let t2x = px - (a2 * px + c2 * py);
                                let t2y = py - (b2 * px + d2 * py);
                                cur = [
                                    a1 * a2 + c1 * b2,
                                    b1 * a2 + d1 * b2,
                                    a1 * c2 + c1 * d2,
                                    b1 * c2 + d1 * d2,
                                    a1 * t2x + c1 * t2y + tx1,
                                    b1 * t2x + d1 * t2y + ty1,
                                ];
                            }
                        }
                    },
                    Item::Image {
                        x,
                        y_top,
                        width,
                        height,
                        ..
                    } => {
                        let [a, b, c, d, tx, ty] = cur;
                        let tr = |px: f64, py: f64| (a * px + c * py + tx, b * px + d * py + ty);
                        let (x0, y0) = (*x as f64, *y_top as f64);
                        let (x1, y1) = (x0 + *width as f64, y0 + *height as f64);
                        let pts = [tr(x0, y0), tr(x1, y0), tr(x0, y1), tr(x1, y1)];
                        let minx = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
                        let maxx = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
                        let miny = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
                        let maxy = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
                        out.push((
                            minx / rtex_dl::SP_PER_BP,
                            pdf_page.height - maxy / rtex_dl::SP_PER_BP,
                            (maxx - minx) / rtex_dl::SP_PER_BP,
                            (maxy - miny) / rtex_dl::SP_PER_BP,
                        ));
                    }
                    _ => {}
                }
            }
            out
        };
        let images_matched = dl_images
            .iter()
            .filter(|(x, y, w, h)| {
                pdf_page.images.iter().any(|im| {
                    (im.x - x).abs() < 0.01
                        && (im.y - y).abs() < 0.01
                        && (im.w - w).abs() < 0.01
                        && (im.h - h).abs() < 0.01
                })
            })
            .count();
        let mut pv = PageVerdict {
            page: page_no,
            dl_glyphs: rep.dl_glyphs,
            pdf_glyphs: rep.pdf_glyphs,
            matched: rep.matched,
            unmatched: rep.unmatched,
            anchored_max_dx_bp: rep.anchored_max_dx_bp,
            interior_max_dx_bp: interior_max,
            max_dy_bp: rep.max_dy_bp,
            tj_quantum_bp: tj_quantum,
            rules_dl: rep.rules_dl,
            rules_matched: rep.rules_matched,
            colors_dl,
            colors_pdf: pdf_page.color_ops.len(),
            exact: dl.is_exact(),
            flags: dl.flags.clone(),
            images_dl: dl_images.len(),
            images_matched,
            images_pdf: pdf_page.images.len(),
            ..Default::default()
        };
        let page_ok = rep.unmatched == 0
            && rep.dl_glyphs == rep.pdf_glyphs
            && rep.anchored_max_dx_bp <= dec_quantum * 1.5
            && interior_max <= tj_quantum * 1.05
            && rep.max_dy_bp <= dec_quantum * 1.5
            && rep.rules_matched == rep.rules_dl
            && images_matched == dl_images.len()
            && dl_images.len() == pdf_page.images.len();
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
            let (dl_gray, w, h, stats) = raster::rasterize(
                &[&dl],
                pdf_page.width,
                pdf_page.height,
                opts.dpi as f64,
                &mut cache,
            )?;
            raster::write_png(
                &opts.build.join(format!("dl-p{page_no}.png")),
                &dl_gray,
                w,
                h,
            )?;
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
        let worst = report
            .pages
            .iter()
            .filter_map(|p| p.raster_unmatched_fraction)
            .fold(0.0, f64::max);
        println!(
            "layer 3 (rendered, {} dpi): pass={:?}; worst unmatched ink fraction {:.3}%",
            opts.dpi,
            l3_ok,
            worst * 100.0
        );
    }
    if opts.pic_cache {
        report.pic_cache = Some(pic_cache_check(&tl, &project, &opts, &unit_envs, &cap)?);
    }
    if let Some(p) = &opts.json_out {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(report)
}

/// Copy the aux family (`.aux .toc .lof .lot .out .bbl .bcf`) of a pass directory into another,
/// subdirectories included (`\include`d chapters write `chapters/one.aux`).
fn copy_aux_family(root: &Path, dir: &Path, out: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) != Some("pic-cache") {
                copy_aux_family(root, &path, out)?;
            }
            continue;
        }
        let keep = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e, "aux" | "toc" | "lof" | "lot" | "out" | "bbl" | "bcf"))
            .unwrap_or(false);
        if keep {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let dst = out.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, dst)?;
        }
    }
    Ok(())
}

/// Picture cache check: build a cache from the converged capture (`cap`), run one more capture
/// pass with its manifest, and compare units, placements and (optionally) rendered pages.
fn pic_cache_check(
    tl: &TexLive,
    project: &Path,
    opts: &VerifyOpts,
    unit_envs: &str,
    cap: &rtex_core::capture::CaptureResult,
) -> Result<PicCacheReport> {
    use rtex_core::piccache::{scan_pictures, PicCache};
    let mut rep = PicCacheReport::default();
    let files = rtex_core::document::load_project_files(project, &opts.main)?;
    let pics = scan_pictures(
        &files,
        &opts.main,
        rtex_core::piccache::preamble_hash(&files, &opts.main),
    );
    rep.pictures = pics.len();
    rep.cacheable = pics.iter().filter(|p| p.cacheable).count();
    let recorded = cap.json.recorded_pics();
    rep.recorded = pics
        .iter()
        .filter(|p| p.cacheable && recorded.contains_key(&p.key))
        .count();
    rep.wall_uncached_ms = cap.wall.as_millis() as u64;
    if rep.cacheable == 0 {
        println!("picture cache: no cacheable picture environment in the sources");
        rep.units_equal = true;
        rep.placements_equal = true;
        rep.pass = true;
        return Ok(rep);
    }
    // the second pass runs on the converged aux family (same labels, TOC and bibliography)
    let out = opts.build.join("piccache");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;
    copy_aux_family(&cap.out_dir, &cap.out_dir, &out)?;
    let mut cache = PicCache::open(&out.join("pic-cache"));
    cache.absorb(&pics, &recorded, &[], &cap.pdf)?;
    rep.hits = cache.write_manifest(&pics, &out.join("pic-manifest.json"))?;
    let cached =
        rtex_core::capture::run_capture_with(tl, project, &opts.main, &out, true, unit_envs)?;
    rep.wall_cached_ms = cached.wall.as_millis() as u64;
    rep.mismatched = cached.json.pic_mismatch.len();
    // units and placements
    let (a, b) = (&cap.json.units, &cached.json.units);
    rep.units_equal = a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.kind == y.kind
                && x.name == y.name
                && x.begin_line == y.begin_line
                && x.end_line == y.end_line
        });
    let mut differing = Vec::new();
    for (x, y) in a.iter().zip(b) {
        let same = x.placements.len() == y.placements.len()
            && x.placements.iter().zip(&y.placements).all(|(p, q)| {
                (p.page, p.x, p.y, p.w, p.h, p.d) == (q.page, q.x, q.y, q.w, q.h, q.d)
            });
        if !same {
            differing.push(x.uid);
        }
    }
    rep.placements_equal = rep.units_equal && differing.is_empty();
    for n in 1..=cached.json.pages {
        if let Ok(dl) = cached.page(n) {
            rep.cached_images += dl
                .flags_map()
                .get("pic_cache")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
        }
    }
    // rendered pages of the two passes
    if opts.raster {
        let mut worst = 0.0f64;
        for n in 1..=cap.json.pages.min(cached.json.pages) {
            let pa = out.join(format!("uncached-p{n}.png"));
            let pb = out.join(format!("cached-p{n}.png"));
            raster::render_pdf_page_with_pymupdf(&cap.pdf, n as u32, opts.dpi, &pa)?;
            raster::render_pdf_page_with_pymupdf(&cached.pdf, n as u32, opts.dpi, &pb)?;
            let (ga, wa, ha) = raster::read_png_gray(&pa)?;
            let (gb, wb, hb) = raster::read_png_gray(&pb)?;
            if (wa, ha) != (wb, hb) {
                worst = 1.0;
                println!("  picture cache page {n}: raster size mismatch");
                continue;
            }
            let diff = raster::compare(&ga, &gb, wa, ha, 96);
            if diff.unmatched_fraction > 0.001 {
                println!(
                    "  picture cache page {n}: unmatched ink {:.3}% (bbox {:?})",
                    diff.unmatched_fraction * 100.0,
                    diff.bbox_unmatched
                );
            }
            worst = worst.max(diff.unmatched_fraction);
        }
        rep.raster_unmatched_fraction = Some(worst);
    }
    // the check is meaningless when nothing was cached: a cacheable document must record
    // and reuse pictures (at least `min_cached` of them when the caller says how many)
    let enough = rep.recorded > 0 && rep.recorded >= opts.min_cached.unwrap_or(1);
    rep.pass = rep.units_equal
        && rep.placements_equal
        && cap.json.pages == cached.json.pages
        && enough
        && rep.hits == rep.recorded
        && rep.cached_images == rep.hits
        && cached.json.pic_mismatch.is_empty()
        && rep.raster_unmatched_fraction.unwrap_or(0.0) <= 0.001;
    println!(
        "picture cache: {} pictures, {} cacheable, {} recorded by the first pass, {} taken from it by the second ({} cached images on its pages, {} mismatched); units equal {}, placements equal {}{}; pass {:.1}s -> {:.1}s; pass={}",
        rep.pictures,
        rep.cacheable,
        rep.recorded,
        rep.hits,
        rep.cached_images,
        rep.mismatched,
        rep.units_equal,
        rep.placements_equal,
        match rep.raster_unmatched_fraction {
            Some(f) => format!(", worst unmatched ink {:.3}%", f * 100.0),
            None => String::new(),
        },
        rep.wall_uncached_ms as f64 / 1000.0,
        rep.wall_cached_ms as f64 / 1000.0,
        rep.pass
    );
    if !differing.is_empty() {
        println!(
            "  units with different placements: {:?}",
            &differing[..differing.len().min(10)]
        );
    }
    Ok(rep)
}
