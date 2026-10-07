//! Independent extraction of glyph placements from a PDF's content streams.
//!
//! This deliberately shares no code with the Lua traversal: it replays the PDF text state
//! machine (Tf, Tm, Td, TD, T*, TL, Tc, Tw, Tz, Ts, TJ, Tj, ', ") with the CTM (cm, q, Q)
//! and the fonts' own width tables, producing absolute glyph origins in PDF user space (bp,
//! origin bottom-left) plus rules (`re f`) and color operators.

use anyhow::{anyhow, Context, Result};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}
impl Matrix {
    pub const IDENTITY: Matrix = Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };
    pub fn mul(&self, m: &Matrix) -> Matrix {
        // self × m  (apply self first, then m)
        Matrix {
            a: self.a * m.a + self.b * m.c,
            b: self.a * m.b + self.b * m.d,
            c: self.c * m.a + self.d * m.c,
            d: self.c * m.b + self.d * m.d,
            e: self.e * m.a + self.f * m.c + m.e,
            f: self.e * m.b + self.f * m.d + m.f,
        }
    }
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }
    pub fn translate(tx: f64, ty: f64) -> Matrix {
        Matrix { e: tx, f: ty, ..Matrix::IDENTITY }
    }
}

#[derive(Debug, Clone)]
pub struct PdfFont {
    pub resource: String,
    pub base_font: String,
    pub subtype: String,
    pub two_byte: bool,
    /// Widths in 1/1000 text-space units keyed by code (CID for Type0, byte for simple fonts).
    pub widths: HashMap<u32, f64>,
    pub default_width: f64,
}

#[derive(Debug, Clone)]
pub struct PdfGlyph {
    pub font: String,
    pub base_font: String,
    pub code: u32,
    pub size: f64,
    /// Glyph origin in PDF user space (bp, y up).
    pub x: f64,
    pub y: f64,
    /// Horizontal scale of the full transform (Tm × CTM a-component × Tz); 1.0 = unexpanded.
    pub hscale: f64,
    /// Advance applied after this glyph in user space (bp), from the PDF's own width table.
    pub advance: f64,
    /// True when this glyph's origin came directly from a Tm/Td (no accumulated TJ error).
    pub anchored: bool,
}

#[derive(Debug, Clone)]
pub struct PdfRule {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Default)]
pub struct PdfPage {
    pub number: u32,
    pub width: f64,
    pub height: f64,
    pub glyphs: Vec<PdfGlyph>,
    pub rules: Vec<PdfRule>,
    pub color_ops: Vec<String>,
    pub fonts: HashMap<String, PdfFont>,
    /// Number of decimals LuaTeX used for coordinates (inferred from the first Tm operands).
    pub decimals: usize,
}

fn f(o: &Object) -> Result<f64> {
    match o {
        Object::Integer(i) => Ok(*i as f64),
        Object::Real(r) => Ok(*r as f64),
        other => Err(anyhow!("not a number: {other:?}")),
    }
}

fn load_font(doc: &Document, resource: &str, id: ObjectId) -> Result<PdfFont> {
    let dict = doc.get_dictionary(id)?;
    let subtype = dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).to_string()).unwrap_or_default();
    let base_font = dict.get(b"BaseFont").ok().and_then(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).to_string()).unwrap_or_default();
    let mut widths = HashMap::new();
    let mut default_width = 0.0;
    let two_byte;
    if subtype == "Type0" {
        two_byte = true;
        let desc = dict.get(b"DescendantFonts")?;
        let desc = doc.dereference(desc)?.1;
        let arr = desc.as_array()?;
        let cid_id = arr.first().ok_or_else(|| anyhow!("no descendant"))?.as_reference()?;
        let cid = doc.get_dictionary(cid_id)?;
        default_width = cid.get(b"DW").ok().and_then(|o| f(o).ok()).unwrap_or(1000.0);
        if let Ok(w) = cid.get(b"W") {
            let w = doc.dereference(w)?.1.as_array()?.clone();
            let mut i = 0;
            while i < w.len() {
                let first = f(&doc.dereference(&w[i])?.1)? as u32;
                if i + 1 < w.len() {
                    let second = doc.dereference(&w[i + 1])?.1.clone();
                    if let Ok(list) = second.as_array() {
                        for (k, wv) in list.iter().enumerate() {
                            widths.insert(first + k as u32, f(&doc.dereference(wv)?.1)?);
                        }
                        i += 2;
                    } else {
                        let last = f(&second)? as u32;
                        let wv = f(&doc.dereference(&w[i + 2])?.1)?;
                        for c in first..=last {
                            widths.insert(c, wv);
                        }
                        i += 3;
                    }
                } else {
                    break;
                }
            }
        }
    } else {
        two_byte = false;
        let first = dict.get(b"FirstChar").ok().and_then(|o| f(o).ok()).unwrap_or(0.0) as u32;
        if let Ok(w) = dict.get(b"Widths") {
            let w = doc.dereference(w)?.1.as_array()?.clone();
            for (k, wv) in w.iter().enumerate() {
                widths.insert(first + k as u32, f(&doc.dereference(wv)?.1)?);
            }
        }
    }
    Ok(PdfFont { resource: resource.to_string(), base_font, subtype, two_byte, widths, default_width })
}

fn page_fonts(doc: &Document, page_id: ObjectId) -> Result<HashMap<String, PdfFont>> {
    let (res, res_ids) = doc.get_page_resources(page_id)?;
    let mut dicts: Vec<Dictionary> = Vec::new();
    if let Some(r) = res {
        dicts.push(r.clone());
    }
    for id in res_ids {
        if let Ok(d) = doc.get_dictionary(id) {
            dicts.push(d.clone());
        }
    }
    let mut fonts = HashMap::new();
    for d in dicts {
        if let Ok(fd) = d.get(b"Font") {
            let fd = doc.dereference(fd)?.1.clone();
            if let Ok(fd) = fd.as_dict() {
                for (name, obj) in fd.iter() {
                    let name = String::from_utf8_lossy(name).to_string();
                    if let Ok(id) = obj.as_reference() {
                        if let Ok(font) = load_font(doc, &name, id) {
                            fonts.insert(name, font);
                        }
                    }
                }
            }
        }
    }
    Ok(fonts)
}

fn media_box(doc: &Document, page_id: ObjectId) -> Result<(f64, f64)> {
    let mut id = page_id;
    for _ in 0..16 {
        let d = doc.get_dictionary(id)?;
        if let Ok(mb) = d.get(b"MediaBox") {
            let mb = doc.dereference(mb)?.1;
            let a = mb.as_array()?;
            let x0 = f(&a[0])?;
            let y0 = f(&a[1])?;
            let x1 = f(&a[2])?;
            let y1 = f(&a[3])?;
            return Ok((x1 - x0, y1 - y0));
        }
        match d.get(b"Parent").and_then(|p| p.as_reference()) {
            Ok(p) => id = p,
            Err(_) => break,
        }
    }
    Ok((612.0, 792.0))
}

fn decimals_of(o: &Object) -> usize {
    match o {
        Object::Real(r) => {
            let s = format!("{}", r);
            s.split('.').nth(1).map(|d| d.len()).unwrap_or(0)
        }
        _ => 0,
    }
}

pub fn extract(path: &Path) -> Result<Vec<PdfPage>> {
    let doc = Document::load(path).with_context(|| format!("loading {}", path.display()))?;
    let mut pages = Vec::new();
    for (num, page_id) in doc.get_pages() {
        let (width, height) = media_box(&doc, page_id)?;
        let fonts = page_fonts(&doc, page_id)?;
        let data = doc.get_page_content(page_id)?;
        let content = Content::decode(&data)?;
        let mut page = PdfPage { number: num, width, height, fonts, decimals: 0, ..Default::default() };
        let mut ctm = Matrix::IDENTITY;
        let mut stack: Vec<Matrix> = Vec::new();
        let mut tm = Matrix::IDENTITY;
        let mut tlm = Matrix::IDENTITY;
        let mut font: Option<String> = None;
        let mut size = 0.0;
        let (mut tc, mut tw, mut tz, mut tl, mut ts) = (0.0, 0.0, 1.0, 0.0, 0.0);
        let mut anchored = false;
        let mut max_dec = 0usize;
        // path state for stroked rules (LuaTeX draws rules as `q … cm lw w 0 0 m x 0 l S Q`)
        let mut line_width = 1.0f64;
        let mut path_start: Option<(f64, f64)> = None;
        let mut path_segments: Vec<((f64, f64), (f64, f64))> = Vec::new();
        for op in &content.operations {
            let ops = &op.operands;
            match op.operator.as_str() {
                "q" => stack.push(ctm),
                "Q" => ctm = stack.pop().unwrap_or(Matrix::IDENTITY),
                "cm" if ops.len() == 6 => {
                    let m = Matrix { a: f(&ops[0])?, b: f(&ops[1])?, c: f(&ops[2])?, d: f(&ops[3])?, e: f(&ops[4])?, f: f(&ops[5])? };
                    ctm = m.mul(&ctm);
                }
                "BT" => {
                    tm = Matrix::IDENTITY;
                    tlm = tm;
                }
                "ET" => {}
                "Tf" if ops.len() == 2 => {
                    font = ops[0].as_name().ok().map(|n| String::from_utf8_lossy(n).to_string());
                    size = f(&ops[1])?;
                }
                "Tc" => tc = f(&ops[0])?,
                "Tw" => tw = f(&ops[0])?,
                "Tz" => tz = f(&ops[0])? / 100.0,
                "TL" => tl = f(&ops[0])?,
                "Ts" => ts = f(&ops[0])?,
                "Tm" if ops.len() == 6 => {
                    for o in ops.iter().skip(4) {
                        max_dec = max_dec.max(decimals_of(o));
                    }
                    tm = Matrix { a: f(&ops[0])?, b: f(&ops[1])?, c: f(&ops[2])?, d: f(&ops[3])?, e: f(&ops[4])?, f: f(&ops[5])? };
                    tlm = tm;
                    anchored = true;
                }
                "Td" if ops.len() == 2 => {
                    tlm = Matrix::translate(f(&ops[0])?, f(&ops[1])?).mul(&tlm);
                    tm = tlm;
                    anchored = true;
                }
                "TD" if ops.len() == 2 => {
                    tl = -f(&ops[1])?;
                    tlm = Matrix::translate(f(&ops[0])?, f(&ops[1])?).mul(&tlm);
                    tm = tlm;
                    anchored = true;
                }
                "T*" => {
                    tlm = Matrix::translate(0.0, -tl).mul(&tlm);
                    tm = tlm;
                    anchored = true;
                }
                "Tj" | "'" | "\"" | "TJ" => {
                    if op.operator == "'" || op.operator == "\"" {
                        tlm = Matrix::translate(0.0, -tl).mul(&tlm);
                        tm = tlm;
                        anchored = true;
                    }
                    let fname = font.clone().unwrap_or_default();
                    let fnt = page.fonts.get(&fname).cloned();
                    let elements: Vec<Object> = if op.operator == "TJ" {
                        ops.first().and_then(|a| a.as_array().ok()).cloned().unwrap_or_default()
                    } else {
                        vec![ops.last().cloned().unwrap_or(Object::Null)]
                    };
                    for el in elements {
                        match el {
                            Object::String(bytes, _) => {
                                let codes: Vec<u32> = match &fnt {
                                    Some(fn_) if fn_.two_byte => bytes.chunks(2).map(|c| ((c[0] as u32) << 8) | *c.get(1).unwrap_or(&0) as u32).collect(),
                                    _ => bytes.iter().map(|b| *b as u32).collect(),
                                };
                                for code in codes {
                                    let w0 = fnt.as_ref().map(|fn_| *fn_.widths.get(&code).unwrap_or(&fn_.default_width)).unwrap_or(0.0) / 1000.0;
                                    let trm = Matrix { a: size * tz, b: 0.0, c: 0.0, d: size, e: 0.0, f: ts }.mul(&tm).mul(&ctm);
                                    let (x, y) = trm.apply(0.0, 0.0);
                                    let hscale = trm.a / size.max(1e-9);
                                    let is_space = code == 32 && !fnt.as_ref().map(|f| f.two_byte).unwrap_or(false);
                                    let tx = (w0 * size + tc + if is_space { tw } else { 0.0 }) * tz;
                                    let adv_user = tx * tm.a * ctm.a;
                                    page.glyphs.push(PdfGlyph {
                                        font: fname.clone(),
                                        base_font: fnt.as_ref().map(|f| f.base_font.clone()).unwrap_or_default(),
                                        code,
                                        size,
                                        x,
                                        y,
                                        hscale,
                                        advance: adv_user,
                                        anchored,
                                    });
                                    anchored = false;
                                    tm = Matrix::translate(tx, 0.0).mul(&tm);
                                }
                            }
                            Object::Integer(_) | Object::Real(_) => {
                                let n = f(&el)?;
                                let tx = -n / 1000.0 * size * tz;
                                tm = Matrix::translate(tx, 0.0).mul(&tm);
                            }
                            _ => {}
                        }
                    }
                }
                "w" if ops.len() == 1 => line_width = f(&ops[0])?,
                "m" if ops.len() == 2 => path_start = Some(ctm.apply(f(&ops[0])?, f(&ops[1])?)),
                "l" if ops.len() == 2 => {
                    let p1 = ctm.apply(f(&ops[0])?, f(&ops[1])?);
                    if let Some(p0) = path_start {
                        path_segments.push((p0, p1));
                    }
                    path_start = Some(p1);
                }
                "S" | "s" | "B" | "b" => {
                    let lw = line_width * ctm.a.abs().max(ctm.d.abs());
                    for ((x0, y0), (x1, y1)) in path_segments.drain(..) {
                        if (y0 - y1).abs() < 1e-9 {
                            page.rules.push(PdfRule { x: x0.min(x1), y: y0 - lw / 2.0, w: (x1 - x0).abs(), h: lw });
                        } else if (x0 - x1).abs() < 1e-9 {
                            page.rules.push(PdfRule { x: x0 - lw / 2.0, y: y0.min(y1), w: lw, h: (y1 - y0).abs() });
                        }
                    }
                    path_start = None;
                }
                "n" | "h" => {
                    path_segments.clear();
                    path_start = None;
                }
                "re" if ops.len() == 4 => {
                    let (x, y, w, h) = (f(&ops[0])?, f(&ops[1])?, f(&ops[2])?, f(&ops[3])?);
                    let (x0, y0) = ctm.apply(x, y);
                    let (x1, y1) = ctm.apply(x + w, y + h);
                    page.rules.push(PdfRule { x: x0.min(x1), y: y0.min(y1), w: (x1 - x0).abs(), h: (y1 - y0).abs() });
                }
                "g" | "G" | "rg" | "RG" | "k" | "K" | "cs" | "CS" | "sc" | "SC" | "scn" | "SCN" => {
                    let s = ops.iter().map(|o| match o {
                        Object::Integer(i) => i.to_string(),
                        Object::Real(r) => format!("{}", r),
                        Object::Name(n) => format!("/{}", String::from_utf8_lossy(n)),
                        _ => "?".into(),
                    }).collect::<Vec<_>>().join(" ");
                    page.color_ops.push(format!("{} {}", s, op.operator));
                }
                _ => {}
            }
        }
        page.decimals = max_dec;
        pages.push(page);
    }
    Ok(pages)
}
