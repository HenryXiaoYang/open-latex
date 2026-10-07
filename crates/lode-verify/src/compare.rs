//! Compare a lode page/paragraph display list with glyph placements extracted from a PDF.

use crate::pdftext::{PdfGlyph, PdfPage};
use lode_dl::{DisplayList, Item, Line, SP_PER_BP};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct GlyphDiff {
    pub line: i64,
    pub par: i64,
    pub index: Option<i64>,
    pub dl_x_bp: f64,
    pub dl_y_bp: f64,
    pub pdf_x_bp: Option<f64>,
    pub pdf_y_bp: Option<f64>,
    pub dx_bp: Option<f64>,
    pub dy_bp: Option<f64>,
    pub dl_scale: f64,
    pub pdf_scale: Option<f64>,
    pub dl_advance_bp: f64,
    pub pdf_advance_bp: Option<f64>,
    pub pdf_font: Option<String>,
    pub anchored: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CompareReport {
    pub page: u32,
    pub dl_glyphs: usize,
    pub pdf_glyphs: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub max_dx_bp: f64,
    pub max_dy_bp: f64,
    pub max_scale_err: f64,
    pub max_advance_err_bp: f64,
    pub within_tolerance: usize,
    pub tolerance_bp: f64,
    pub worst: Vec<GlyphDiff>,
    pub rules_dl: usize,
    pub rules_pdf: usize,
    pub rules_matched: usize,
    /// Glyphs whose PDF origin was set directly by a Tm/Td (no accumulated TJ rounding).
    pub anchored: usize,
    pub anchored_max_dx_bp: f64,
    pub anchored_max_dy_bp: f64,
    /// Max |dx| per PDF base font (shows which fonts drift).
    pub max_dx_by_font: std::collections::BTreeMap<String, f64>,
    pub rule_mismatches: Vec<String>,
}

struct DlGlyph<'a> {
    line: i64,
    par: i64,
    item: &'a Item,
    scale: f64,
}

fn dl_glyphs<'a>(dl: &'a DisplayList, lines: &[&'a Line]) -> Vec<DlGlyph<'a>> {
    let mut v = Vec::new();
    for l in lines {
        for it in &l.items {
            if let Item::Glyph { expansion, .. } = it {
                v.push(DlGlyph { line: l.i, par: l.par, item: it, scale: 1.0 + *expansion as f64 / 1_000_000.0 });
            }
        }
    }
    let _ = dl;
    v
}

/// Compare the glyphs of `lines` (page coordinates, sp) against the PDF page. When `lines` is
/// empty, all lines and `other` items of the display list are used.
pub fn compare_page(dl: &DisplayList, pdf: &PdfPage, lines: &[&Line], tolerance_bp: f64) -> CompareReport {
    let all_lines: Vec<&Line> = if lines.is_empty() { dl.lines.iter().collect() } else { lines.to_vec() };
    let mut glyphs = dl_glyphs(dl, &all_lines);
    if lines.is_empty() {
        for it in &dl.other {
            if let Item::Glyph { expansion, .. } = it {
                glyphs.push(DlGlyph { line: 0, par: 0, item: it, scale: 1.0 + *expansion as f64 / 1_000_000.0 });
            }
        }
    }
    let page_h = pdf.height;
    let mut used = vec![false; pdf.glyphs.len()];
    // index PDF glyphs by code for fast nearest-neighbour lookup
    let mut by_code: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, g) in pdf.glyphs.iter().enumerate() {
        by_code.entry(g.code).or_default().push(i);
    }
    let mut rep = CompareReport { page: pdf.number, dl_glyphs: glyphs.len(), pdf_glyphs: pdf.glyphs.len(), tolerance_bp, ..Default::default() };
    let mut diffs: Vec<GlyphDiff> = Vec::new();
    for g in &glyphs {
        let Item::Glyph { char, index, x, y, width, .. } = g.item else { continue };
        let code = index.unwrap_or(*char) as u32;
        let dl_x = *x as f64 / SP_PER_BP;
        let dl_y = page_h - *y as f64 / SP_PER_BP;
        let dl_adv = *width as f64 / SP_PER_BP;
        let mut best: Option<(usize, f64)> = None;
        if let Some(cands) = by_code.get(&code) {
            for &ci in cands {
                if used[ci] {
                    continue;
                }
                let pg: &PdfGlyph = &pdf.glyphs[ci];
                let d = ((pg.x - dl_x).powi(2) + (pg.y - dl_y).powi(2)).sqrt();
                if d < 2.0 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((ci, d));
                }
            }
        }
        let mut diff = GlyphDiff { line: g.line, par: g.par, index: *index, dl_x_bp: dl_x, dl_y_bp: dl_y, dl_scale: g.scale, dl_advance_bp: dl_adv, ..Default::default() };
        match best {
            Some((ci, _)) => {
                used[ci] = true;
                let pg = &pdf.glyphs[ci];
                let dx = pg.x - dl_x;
                let dy = pg.y - dl_y;
                diff.pdf_x_bp = Some(pg.x);
                diff.pdf_y_bp = Some(pg.y);
                diff.dx_bp = Some(dx);
                diff.dy_bp = Some(dy);
                diff.pdf_scale = Some(pg.hscale);
                diff.pdf_advance_bp = Some(pg.advance);
                diff.pdf_font = Some(pg.base_font.clone());
                diff.anchored = pg.anchored;
                if pg.anchored {
                    rep.anchored += 1;
                    rep.anchored_max_dx_bp = rep.anchored_max_dx_bp.max(dx.abs());
                    rep.anchored_max_dy_bp = rep.anchored_max_dy_bp.max(dy.abs());
                }
                let e = rep.max_dx_by_font.entry(pg.base_font.clone()).or_insert(0.0);
                *e = e.max(dx.abs());
                rep.matched += 1;
                rep.max_dx_bp = rep.max_dx_bp.max(dx.abs());
                rep.max_dy_bp = rep.max_dy_bp.max(dy.abs());
                rep.max_scale_err = rep.max_scale_err.max((pg.hscale - g.scale).abs());
                rep.max_advance_err_bp = rep.max_advance_err_bp.max((pg.advance - dl_adv * 1.0).abs());
                if dx.abs() <= tolerance_bp && dy.abs() <= tolerance_bp {
                    rep.within_tolerance += 1;
                }
            }
            None => rep.unmatched += 1,
        }
        diffs.push(diff);
    }
    diffs.sort_by(|a, b| {
        let da = a.dx_bp.map(|d| d.abs()).unwrap_or(f64::INFINITY).max(a.dy_bp.map(|d| d.abs()).unwrap_or(f64::INFINITY));
        let db = b.dx_bp.map(|d| d.abs()).unwrap_or(f64::INFINITY).max(b.dy_bp.map(|d| d.abs()).unwrap_or(f64::INFINITY));
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });
    rep.worst = diffs.into_iter().take(5).collect();
    // rules
    let mut dl_rules = Vec::new();
    for l in &all_lines {
        for it in &l.items {
            if let Item::Rule { x, y_top, width, height } = it {
                dl_rules.push((*x as f64 / SP_PER_BP, page_h - (*y_top + *height) as f64 / SP_PER_BP, *width as f64 / SP_PER_BP, *height as f64 / SP_PER_BP));
            }
        }
    }
    if lines.is_empty() {
        for it in &dl.other {
            if let Item::Rule { x, y_top, width, height } = it {
                dl_rules.push((*x as f64 / SP_PER_BP, page_h - (*y_top + *height) as f64 / SP_PER_BP, *width as f64 / SP_PER_BP, *height as f64 / SP_PER_BP));
            }
        }
    }
    rep.rules_dl = dl_rules.len();
    rep.rules_pdf = pdf.rules.len();
    for (x, y, w, h) in dl_rules {
        if pdf.rules.iter().any(|r| (r.x - x).abs() < 0.01 && (r.y - y).abs() < 0.01 && (r.w - w).abs() < 0.01 && (r.h - h).abs() < 0.01) {
            rep.rules_matched += 1;
        } else {
            let nearest = pdf.rules.iter().min_by(|a, b| ((a.x - x).abs() + (a.y - y).abs()).partial_cmp(&((b.x - x).abs() + (b.y - y).abs())).unwrap());
            rep.rule_mismatches.push(format!("dl rule x={x:.4} y={y:.4} w={w:.4} h={h:.4}; nearest pdf {:?}", nearest.map(|r| (r.x, r.y, r.w, r.h))));
        }
    }
    rep
}
