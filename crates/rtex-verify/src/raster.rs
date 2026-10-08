//! Rasterize a display list with the fonts it names (OpenType/TrueType via ttf-parser) and
//! compare against a reference raster of the PDF page (produced by PyMuPDF). The comparison
//! is deliberately tolerant to anti-aliasing differences between rasterizers: a pixel of ink
//! counts as matched when the other image has ink within one pixel.

use anyhow::{anyhow, Context, Result};
use rtex_dl::{DisplayList, Item, SP_PER_BP};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

struct FontData {
    data: Vec<u8>,
    index: u32,
}

#[derive(Default)]
pub struct FontCache {
    files: HashMap<(PathBuf, u32), std::rc::Rc<FontData>>,
}

impl FontCache {
    fn load(&mut self, path: &Path, index: u32) -> Result<std::rc::Rc<FontData>> {
        let key = (path.to_path_buf(), index);
        if let Some(f) = self.files.get(&key) {
            return Ok(f.clone());
        }
        let data =
            std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?;
        let fd = std::rc::Rc::new(FontData { data, index });
        self.files.insert(key, fd.clone());
        Ok(fd)
    }
}

struct Outline<'a> {
    pb: &'a mut PathBuilder,
    tf: Transform,
}
impl ttf_parser::OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = map(&self.tf, x, y);
        self.pb.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = map(&self.tf, x, y);
        self.pb.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = map(&self.tf, x1, y1);
        let (x, y) = map(&self.tf, x, y);
        self.pb.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = map(&self.tf, x1, y1);
        let (x2, y2) = map(&self.tf, x2, y2);
        let (x, y) = map(&self.tf, x, y);
        self.pb.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}
fn map(tf: &Transform, x: f32, y: f32) -> (f32, f32) {
    let mut p = tiny_skia::Point::from_xy(x, y);
    tf.map_point(&mut p);
    (p.x, p.y)
}

pub struct RasterStats {
    pub glyphs_drawn: usize,
    pub glyphs_skipped: usize,
    pub rules_drawn: usize,
    pub skipped_fonts: Vec<String>,
}

/// Rasterize page-coordinate display lists (sp, origin top-left) onto a white canvas of
/// `page_w_bp × page_h_bp` at `dpi`. Returns grayscale coverage (0 = white, 255 = black).
pub fn rasterize(
    dls: &[&DisplayList],
    page_w_bp: f64,
    page_h_bp: f64,
    dpi: f64,
    cache: &mut FontCache,
) -> Result<(Vec<u8>, usize, usize, RasterStats)> {
    let scale = dpi / 72.0; // px per bp
                            // MuPDF (the reference renderer) rounds the page box outwards: A4 at 150 dpi is 1241 px
                            // wide (595.276 bp × 150/72 = 1240.16), not 1240
    let w = (page_w_bp * scale - 1e-3).ceil() as usize;
    let h = (page_h_bp * scale - 1e-3).ceil() as usize;
    let mut pix = Pixmap::new(w as u32, h as u32).ok_or_else(|| anyhow!("pixmap"))?;
    pix.fill(tiny_skia::Color::WHITE);
    let mut paint = Paint::default();
    paint.set_color(tiny_skia::Color::BLACK);
    paint.anti_alias = true;
    let sp_to_px = scale / SP_PER_BP;
    let mut stats = RasterStats {
        glyphs_drawn: 0,
        glyphs_skipped: 0,
        rules_drawn: 0,
        skipped_fonts: vec![],
    };
    for dl in dls {
        let items = dl
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .chain(dl.other.iter());
        for it in items {
            match it {
                Item::Glyph {
                    font,
                    index,
                    x,
                    y,
                    expansion,
                    ..
                } => {
                    let Some(fd) = dl.font(*font) else {
                        stats.glyphs_skipped += 1;
                        continue;
                    };
                    let (Some(file), Some(idx)) = (fd.filename.as_ref(), index) else {
                        stats.glyphs_skipped += 1;
                        let name = fd.psname.clone().or(fd.name.clone()).unwrap_or_default();
                        if !stats.skipped_fonts.contains(&name) {
                            stats.skipped_fonts.push(name);
                        }
                        continue;
                    };
                    let path = Path::new(file);
                    let lower = file.to_ascii_lowercase();
                    if !(lower.ends_with(".otf")
                        || lower.ends_with(".ttf")
                        || lower.ends_with(".ttc"))
                    {
                        stats.glyphs_skipped += 1;
                        if !stats.skipped_fonts.contains(file) {
                            stats.skipped_fonts.push(file.clone());
                        }
                        continue;
                    }
                    let data = cache.load(path, fd.subfont.unwrap_or(0).max(0) as u32)?;
                    let face = ttf_parser::Face::parse(&data.data, data.index.saturating_sub(1))
                        .map_err(|e| anyhow!("{e:?}"))?;
                    let upem = face.units_per_em() as f32;
                    let size_pt = fd.size.unwrap_or(655360.0) as f32 / 65536.0;
                    let px_per_unit = size_pt * (72.0 / 72.27) * scale as f32 / upem;
                    let hx = px_per_unit
                        * (1.0 + *expansion as f32 / 1_000_000.0)
                        * fd.extend
                            .map(|e| e as f32 / 1000.0)
                            .filter(|e| *e != 0.0)
                            .unwrap_or(1.0);
                    let slant = fd.slant.map(|s| s as f32 / 1000.0).unwrap_or(0.0);
                    let ox = *x as f32 * sp_to_px as f32;
                    let oy = *y as f32 * sp_to_px as f32;
                    // font units → device: x' = ox + hx*ux + slant*px_per_unit*uy ; y' = oy - px_per_unit*uy
                    let tf =
                        Transform::from_row(hx, 0.0, slant * px_per_unit, -px_per_unit, ox, oy);
                    let mut pb = PathBuilder::new();
                    if face
                        .outline_glyph(
                            ttf_parser::GlyphId(*idx as u16),
                            &mut Outline { pb: &mut pb, tf },
                        )
                        .is_some()
                    {
                        if let Some(p) = pb.finish() {
                            pix.fill_path(
                                &p,
                                &paint,
                                FillRule::Winding,
                                Transform::identity(),
                                None,
                            );
                        }
                    }
                    stats.glyphs_drawn += 1;
                }
                Item::Rule {
                    x,
                    y_top,
                    width,
                    height,
                } => {
                    let r = Rect::from_xywh(
                        *x as f32 * sp_to_px as f32,
                        *y_top as f32 * sp_to_px as f32,
                        (*width as f32 * sp_to_px as f32).max(0.5),
                        (*height as f32 * sp_to_px as f32).max(0.5),
                    );
                    if let Some(r) = r {
                        pix.fill_rect(r, &paint, Transform::identity(), None);
                        stats.rules_drawn += 1;
                    }
                }
                _ => {}
            }
        }
    }
    let gray: Vec<u8> = pix.pixels().iter().map(|p| 255 - p.red()).collect();
    Ok((gray, w, h, stats))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RasterDiff {
    pub width: usize,
    pub height: usize,
    pub ink_a: usize,
    pub ink_b: usize,
    /// Ink pixels of A with no ink within 1 px in B, and vice versa.
    pub unmatched_a: usize,
    pub unmatched_b: usize,
    pub unmatched_fraction: f64,
    pub bbox_unmatched: Option<(usize, usize, usize, usize)>,
}

/// Tolerant comparison of two grayscale coverage images of identical size.
pub fn compare(a: &[u8], b: &[u8], w: usize, h: usize, ink_threshold: u8) -> RasterDiff {
    let ink = |img: &[u8], x: usize, y: usize| img[y * w + x] >= ink_threshold;
    let near = |img: &[u8], x: usize, y: usize| {
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                let xx = x as i64 + dx;
                let yy = y as i64 + dy;
                if xx >= 0
                    && yy >= 0
                    && (xx as usize) < w
                    && (yy as usize) < h
                    && ink(img, xx as usize, yy as usize)
                {
                    return true;
                }
            }
        }
        false
    };
    let (mut ink_a, mut ink_b, mut ua, mut ub) = (0, 0, 0, 0);
    let mut bbox: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        for x in 0..w {
            let ia = ink(a, x, y);
            let ib = ink(b, x, y);
            if ia {
                ink_a += 1;
                if !near(b, x, y) {
                    ua += 1;
                    bbox = Some(match bbox {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
            if ib {
                ink_b += 1;
                if !near(a, x, y) {
                    ub += 1;
                    bbox = Some(match bbox {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
    }
    let denom = (ink_a + ink_b).max(1) as f64;
    RasterDiff {
        width: w,
        height: h,
        ink_a,
        ink_b,
        unmatched_a: ua,
        unmatched_b: ub,
        unmatched_fraction: (ua + ub) as f64 / denom,
        bbox_unmatched: bbox,
    }
}

pub fn write_png(path: &Path, gray: &[u8], w: usize, h: usize) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(gray)?;
    Ok(())
}

pub fn read_png_gray(path: &Path) -> Result<(Vec<u8>, usize, usize)> {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let bytes = &buf[..info.buffer_size()];
    let gray: Vec<u8> = match info.color_type {
        png::ColorType::Grayscale => bytes.iter().map(|v| 255 - *v).collect(),
        png::ColorType::GrayscaleAlpha => bytes.chunks(2).map(|c| 255 - c[0]).collect(),
        png::ColorType::Rgb => bytes
            .chunks(3)
            .map(|c| 255 - ((c[0] as u32 + c[1] as u32 + c[2] as u32) / 3) as u8)
            .collect(),
        png::ColorType::Rgba => bytes
            .chunks(4)
            .map(|c| 255 - ((c[0] as u32 + c[1] as u32 + c[2] as u32) / 3) as u8)
            .collect(),
        other => anyhow::bail!("unsupported png color type {other:?}"),
    };
    Ok((gray, w, h))
}

/// Render page `page` (1-based) of `pdf` with PyMuPDF at `dpi` into a grayscale PNG.
///
/// The interpreter is `$RTEX_PYTHON` when set, else `python3` from `PATH`. The script
/// drops the current directory from `sys.path` (what `-I` would do) but keeps the user
/// site-packages, so a `pip install --user pymupdf` is found.
pub fn render_pdf_page_with_pymupdf(pdf: &Path, page: u32, dpi: u32, out: &Path) -> Result<()> {
    let script = concat!(
        "import os,sys\n",
        "sys.path[:]=[p for p in sys.path if p and os.path.abspath(p)!=os.getcwd()]\n",
        "import pymupdf\n",
        "d=pymupdf.open(sys.argv[1]);p=d[int(sys.argv[2])-1]\n",
        "pix=p.get_pixmap(dpi=int(sys.argv[3]),colorspace=pymupdf.csGRAY,alpha=False);pix.save(sys.argv[4])\n"
    );
    let python = std::env::var_os("RTEX_PYTHON").unwrap_or_else(|| "python3".into());
    let st = std::process::Command::new(&python)
        .arg("-c")
        .arg(script)
        .arg(pdf)
        .arg(page.to_string())
        .arg(dpi.to_string())
        .arg(out)
        .output()
        .with_context(|| format!("running {} (PyMuPDF)", python.to_string_lossy()))?;
    if !st.status.success() {
        anyhow::bail!(
            "pymupdf render failed: {}",
            String::from_utf8_lossy(&st.stderr)
        );
    }
    Ok(())
}
