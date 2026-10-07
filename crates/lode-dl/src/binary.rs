//! Binary display-list encoding, revision 1 (docs/DISPLAY_LIST.md): decoder and encoder.

use crate::{DisplayList, FontDesc, Item, Line, Sp};
#[cfg(test)]
use std::collections::BTreeMap;
use thiserror::Error;

pub const MAGIC: &[u8; 4] = b"LODL";
pub const VERSION: u16 = 1;

#[derive(Debug, Error)]
pub enum BinError {
    #[error("truncated at offset {0}")]
    Truncated(usize),
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported version {0}")]
    Version(u16),
    #[error("malformed record {tag:#x} at offset {offset}")]
    Malformed { tag: u8, offset: usize },
    #[error("invalid utf-8 in string")]
    Utf8,
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn need(&self, n: usize) -> Result<(), BinError> {
        if self.pos + n > self.b.len() {
            Err(BinError::Truncated(self.pos))
        } else {
            Ok(())
        }
    }
    fn u8(&mut self) -> Result<u8, BinError> {
        self.need(1)?;
        let v = self.b[self.pos];
        self.pos += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16, BinError> {
        self.need(2)?;
        let v = u16::from_le_bytes([self.b[self.pos], self.b[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }
    fn u32(&mut self) -> Result<u32, BinError> {
        self.need(4)?;
        let v = u32::from_le_bytes(self.b[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        Ok(v)
    }
    fn i32(&mut self) -> Result<i32, BinError> {
        Ok(self.u32()? as i32)
    }
    fn f64(&mut self) -> Result<f64, BinError> {
        self.need(8)?;
        let v = f64::from_le_bytes(self.b[self.pos..self.pos + 8].try_into().unwrap());
        self.pos += 8;
        Ok(v)
    }
    fn str(&mut self) -> Result<String, BinError> {
        let n = self.u16()? as usize;
        self.need(n)?;
        let s = std::str::from_utf8(&self.b[self.pos..self.pos + n]).map_err(|_| BinError::Utf8)?.to_string();
        self.pos += n;
        Ok(s)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], BinError> {
        self.need(n)?;
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

fn kind_name(k: u8) -> Option<&'static str> {
    match k {
        1 => Some("opentype"),
        2 => Some("truetype"),
        3 => Some("type1"),
        4 => Some("type3"),
        _ => None,
    }
}
fn kind_code(d: &FontDesc) -> u8 {
    if d.kind.as_deref() == Some("virtual") {
        return 5;
    }
    match d.format.as_deref() {
        Some("opentype") => 1,
        Some("truetype") => 2,
        Some("type1") => 3,
        Some("type3") => 4,
        _ => 0,
    }
}

fn opt_str(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Decode a binary display list.
pub fn decode(bytes: &[u8]) -> Result<DisplayList, BinError> {
    let mut r = Reader { b: bytes, pos: 0 };
    if r.bytes(4)? != MAGIC {
        return Err(BinError::BadMagic);
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(BinError::Version(version));
    }
    let flags = r.u16()?;
    let _total = r.u32()?;
    let _reserved = r.u32()?;
    let mut dl = DisplayList { kind: if flags & 1 == 1 { "page".into() } else { "paragraph".into() }, unit: "sp".into(), ..Default::default() };
    let mut flag_map = serde_json::Map::new();
    let mut cur: Option<Line> = None;
    loop {
        let start = r.pos;
        let tag = r.u8()?;
        let len = r.u32()? as usize;
        let payload = r.bytes(len)?;
        let mut p = Reader { b: payload, pos: 0 };
        let malformed = |_e: BinError| BinError::Malformed { tag, offset: start };
        match tag {
            0x01 => {
                dl.width = p.i32().map_err(malformed)? as Sp;
                dl.height = p.i32().map_err(malformed)? as Sp;
                dl.depth = p.i32().map_err(malformed)? as Sp;
                let page = p.i32().map_err(malformed)?;
                dl.page = if page > 0 { Some(page as i64) } else { None };
                let pw = p.i32().map_err(malformed)?;
                let ph = p.i32().map_err(malformed)?;
                let ox = p.i32().map_err(malformed)?;
                let oy = p.i32().map_err(malformed)?;
                if dl.kind == "page" {
                    dl.page_width = Some(pw as Sp);
                    dl.page_height = Some(ph as Sp);
                    dl.origin = Some((ox as Sp, oy as Sp));
                }
                dl.glyphs = p.u32().map_err(malformed)? as i64;
                let _images = p.u32().map_err(malformed)?;
            }
            0x02 => {
                let id = p.u32().map_err(malformed)? as i64;
                let size = p.i32().map_err(malformed)?;
                let kind = p.u8().map_err(malformed)?;
                let _pad = p.u8().map_err(malformed)?;
                let subfont = p.u16().map_err(malformed)?;
                let slant = p.i32().map_err(malformed)?;
                let extend = p.i32().map_err(malformed)?;
                let squeeze = p.i32().map_err(malformed)?;
                let designsize = p.i32().map_err(malformed)?;
                let filename = p.str().map_err(malformed)?;
                let psname = p.str().map_err(malformed)?;
                let name = p.str().map_err(malformed)?;
                let fullname = p.str().map_err(malformed)?;
                let format = p.str().map_err(malformed)?;
                dl.fonts.insert(id.to_string(), FontDesc {
                    id, name: opt_str(name), fullname: opt_str(fullname), psname: opt_str(psname), filename: opt_str(filename),
                    format: opt_str(format).or_else(|| kind_name(kind).map(|s| s.to_string())),
                    kind: Some(if kind == 5 { "virtual".into() } else { "real".into() }),
                    size: Some(size as f64), designsize: Some(designsize as f64), slant: Some(slant as f64), extend: Some(extend as f64),
                    squeeze: Some(squeeze as f64), subfont: Some(subfont as i64), encodingbytes: None, embedding: None,
                });
            }
            0x03 => {
                let k = p.str().map_err(malformed)?;
                let v = p.i32().map_err(malformed)?;
                flag_map.insert(k, serde_json::Value::from(v));
            }
            0x10 => {
                if cur.is_some() {
                    return Err(BinError::Malformed { tag, offset: start });
                }
                let par = p.i32().map_err(malformed)? as i64;
                let i = p.i32().map_err(malformed)? as i64;
                let x = p.i32().map_err(malformed)? as Sp;
                let y = p.i32().map_err(malformed)? as Sp;
                let w = p.i32().map_err(malformed)? as Sp;
                let h = p.i32().map_err(malformed)? as Sp;
                let d = p.i32().map_err(malformed)? as Sp;
                let gs = p.f64().map_err(malformed)?;
                let gsign = p.u8().map_err(malformed)? as i64;
                let gorder = p.u8().map_err(malformed)? as i64;
                cur = Some(Line { par, i, x, y, w, h, d, gs, gsign, gorder, items: Vec::new() });
            }
            0x11 => {
                let l = cur.take().ok_or(BinError::Malformed { tag, offset: start })?;
                dl.lines.push(l);
            }
            0x20..=0x26 => {
                let item = match tag {
                    0x20 => {
                        let font = p.u32().map_err(malformed)? as i64;
                        let y = p.i32().map_err(malformed)? as Sp;
                        let ef = p.i32().map_err(malformed)? as i64;
                        let n = p.u32().map_err(malformed)? as usize;
                        let target = cur.as_mut().map(|l| &mut l.items).unwrap_or(&mut dl.other);
                        for _ in 0..n {
                            let ch = p.u32().map_err(malformed)? as i64;
                            let idx = p.u32().map_err(malformed)?;
                            let x = p.i32().map_err(malformed)? as Sp;
                            let w = p.i32().map_err(malformed)? as Sp;
                            target.push(Item::Glyph { font, char: ch, index: if idx == u32::MAX { None } else { Some(idx as i64) }, x, y, width: w, expansion: ef });
                        }
                        continue;
                    }
                    0x21 => Item::Rule { x: p.i32().map_err(malformed)? as Sp, y_top: p.i32().map_err(malformed)? as Sp, width: p.i32().map_err(malformed)? as Sp, height: p.i32().map_err(malformed)? as Sp },
                    0x22 => {
                        let cmd = p.u8().map_err(malformed)?;
                        let _pad = p.u8().map_err(malformed)?;
                        let stack = p.u16().map_err(malformed)? as i64;
                        let data = p.str().map_err(malformed)?;
                        Item::Color { stack, cmd: if cmd == 255 { None } else { Some(cmd as i64) }, data }
                    }
                    0x23 => Item::Literal { mode: p.i32().map_err(malformed)? as i64, data: p.str().map_err(malformed)? },
                    0x24 => {
                        let kind = p.str().map_err(malformed)?;
                        let detail = p.str().map_err(malformed)?;
                        Item::Unsupported { kind, detail: serde_json::Value::String(detail) }
                    }
                    0x25 => Item::Math { on: p.u8().map_err(malformed)? == 1, x: p.i32().map_err(malformed)? as Sp },
                    0x26 => Item::Image { index: p.i32().map_err(malformed)? as i64, x: p.i32().map_err(malformed)? as Sp, y_top: p.i32().map_err(malformed)? as Sp, width: p.i32().map_err(malformed)? as Sp, height: p.i32().map_err(malformed)? as Sp },
                    _ => unreachable!(),
                };
                match cur.as_mut() {
                    Some(l) => l.items.push(item),
                    None => dl.other.push(item),
                }
            }
            0xFF => break,
            _ => { /* unknown record: skipped (forward compatibility) */ }
        }
    }
    dl.flags = serde_json::Value::Object(flag_map);
    Ok(dl)
}

struct Writer(Vec<u8>);
impl Writer {
    fn u8(&mut self, v: u8) { self.0.push(v) }
    fn u16(&mut self, v: u16) { self.0.extend_from_slice(&v.to_le_bytes()) }
    fn u32(&mut self, v: u32) { self.0.extend_from_slice(&v.to_le_bytes()) }
    fn i32(&mut self, v: i64) { self.0.extend_from_slice(&(v as i32).to_le_bytes()) }
    fn f64(&mut self, v: f64) { self.0.extend_from_slice(&v.to_le_bytes()) }
    fn str(&mut self, s: &str) {
        let b = &s.as_bytes()[..s.len().min(65535)];
        self.u16(b.len() as u16);
        self.0.extend_from_slice(b);
    }
    fn rec(&mut self, tag: u8, payload: &[u8]) {
        self.u8(tag);
        self.u32(payload.len() as u32);
        self.0.extend_from_slice(payload);
    }
}

fn write_items(out: &mut Writer, items: &[Item]) {
    let mut i = 0;
    while i < items.len() {
        match &items[i] {
            Item::Glyph { font, y, expansion, .. } => {
                let (f0, y0, e0) = (*font, *y, *expansion);
                let mut j = i;
                let mut run = Writer(Vec::new());
                let mut n = 0u32;
                while j < items.len() {
                    if let Item::Glyph { font, char, index, x, y, width, expansion } = &items[j] {
                        if *font != f0 || *y != y0 || *expansion != e0 {
                            break;
                        }
                        run.u32(*char as u32);
                        run.u32(index.map(|v| v as u32).unwrap_or(u32::MAX));
                        run.i32(*x);
                        run.i32(*width);
                        n += 1;
                        j += 1;
                    } else {
                        break;
                    }
                }
                let mut p = Writer(Vec::new());
                p.u32(f0 as u32);
                p.i32(y0);
                p.i32(e0);
                p.u32(n);
                p.0.extend_from_slice(&run.0);
                out.rec(0x20, &p.0);
                i = j;
                continue;
            }
            Item::Rule { x, y_top, width, height } => {
                let mut p = Writer(Vec::new());
                p.i32(*x); p.i32(*y_top); p.i32(*width); p.i32(*height);
                out.rec(0x21, &p.0);
            }
            Item::Color { stack, cmd, data } => {
                let mut p = Writer(Vec::new());
                p.u8(cmd.map(|c| c as u8).unwrap_or(255)); p.u8(0); p.u16(*stack as u16); p.str(data);
                out.rec(0x22, &p.0);
            }
            Item::Literal { mode, data } => {
                let mut p = Writer(Vec::new());
                p.i32(*mode); p.str(data);
                out.rec(0x23, &p.0);
            }
            Item::Unsupported { kind, detail } => {
                let mut p = Writer(Vec::new());
                p.str(kind);
                p.str(&match detail { serde_json::Value::String(s) => s.clone(), serde_json::Value::Null => String::new(), other => other.to_string() });
                out.rec(0x24, &p.0);
            }
            Item::Math { on, x } => {
                let mut p = Writer(Vec::new());
                p.u8(*on as u8); p.i32(*x);
                out.rec(0x25, &p.0);
            }
            Item::Image { index, x, y_top, width, height } => {
                let mut p = Writer(Vec::new());
                p.i32(*index); p.i32(*x); p.i32(*y_top); p.i32(*width); p.i32(*height);
                out.rec(0x26, &p.0);
            }
        }
        i += 1;
    }
}

/// Encode a display list into the binary format.
pub fn encode(dl: &DisplayList) -> Vec<u8> {
    let mut body = Writer(Vec::new());
    {
        let mut p = Writer(Vec::new());
        p.i32(dl.width); p.i32(dl.height); p.i32(dl.depth); p.i32(dl.page.unwrap_or(0));
        p.i32(dl.page_width.unwrap_or(0)); p.i32(dl.page_height.unwrap_or(0));
        let (ox, oy) = dl.origin.unwrap_or((0, 0));
        p.i32(ox); p.i32(oy);
        p.u32(dl.glyphs as u32);
        let images = dl.lines.iter().flat_map(|l| l.items.iter()).chain(dl.other.iter()).filter(|i| matches!(i, Item::Image { .. })).count();
        p.u32(images as u32);
        body.rec(0x01, &p.0);
    }
    for d in dl.fonts.values() {
        let mut p = Writer(Vec::new());
        p.u32(d.id as u32); p.i32(d.size.unwrap_or(0.0).round() as i64); p.u8(kind_code(d)); p.u8(0); p.u16(d.subfont.unwrap_or(0) as u16);
        p.i32(d.slant.unwrap_or(0.0).round() as i64); p.i32(d.extend.unwrap_or(0.0).round() as i64); p.i32(d.squeeze.unwrap_or(0.0).round() as i64); p.i32(d.designsize.unwrap_or(0.0).round() as i64);
        p.str(d.filename.as_deref().unwrap_or("")); p.str(d.psname.as_deref().unwrap_or("")); p.str(d.name.as_deref().unwrap_or("")); p.str(d.fullname.as_deref().unwrap_or("")); p.str(d.format.as_deref().unwrap_or(""));
        body.rec(0x02, &p.0);
    }
    for (k, v) in dl.flags_map() {
        let mut p = Writer(Vec::new());
        p.str(&k); p.i32(v.as_i64().unwrap_or(1));
        body.rec(0x03, &p.0);
    }
    write_items(&mut body, &dl.other);
    for l in &dl.lines {
        let mut p = Writer(Vec::new());
        p.i32(l.par); p.i32(l.i); p.i32(l.x); p.i32(l.y); p.i32(l.w); p.i32(l.h); p.i32(l.d); p.f64(l.gs); p.u8(l.gsign as u8); p.u8(l.gorder as u8);
        body.rec(0x10, &p.0);
        write_items(&mut body, &l.items);
        body.rec(0x11, &[]);
    }
    body.rec(0xFF, &[]);
    let mut out = Writer(Vec::new());
    out.0.extend_from_slice(MAGIC);
    out.u16(VERSION);
    out.u16(if dl.kind == "page" { 1 } else { 0 });
    out.u32(16 + body.0.len() as u32);
    out.u32(0);
    out.0.extend_from_slice(&body.0);
    out.0
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> DisplayList {
        let mut fonts = BTreeMap::new();
        fonts.insert("27".to_string(), FontDesc { id: 27, name: Some("TeXGyrePagella".into()), fullname: None, psname: Some("TeXGyrePagella-Regular".into()), filename: Some("/f/texgyrepagella-regular.otf".into()), format: Some("opentype".into()), kind: Some("real".into()), size: Some(717619.0), designsize: Some(0.0), slant: Some(0.0), extend: Some(1000.0), squeeze: Some(0.0), subfont: Some(0), encodingbytes: None, embedding: None });
        DisplayList {
            kind: "page".into(), unit: "sp".into(), fonts,
            lines: vec![Line { par: 3, i: 1, x: 100, y: 2000, w: 5000, h: 500, d: 200, gs: 0.25, gsign: 1, gorder: 0, items: vec![
                Item::Glyph { font: 27, char: 65, index: Some(36), x: 100, y: 2000, width: 400, expansion: 20000 },
                Item::Glyph { font: 27, char: 66, index: Some(37), x: 500, y: 2000, width: 420, expansion: 20000 },
                Item::Rule { x: 950, y_top: 1500, width: 30, height: 700 },
                Item::Color { stack: 0, cmd: Some(1), data: "1 0 0 rg".into() },
                Item::Glyph { font: 27, char: 67, index: None, x: 1000, y: 2000, width: 400, expansion: 0 },
                Item::Color { stack: 0, cmd: Some(2), data: String::new() },
                Item::Math { on: true, x: 1400 },
                Item::Image { index: 2, x: 1500, y_top: 1200, width: 800, height: 800 },
                Item::Literal { mode: 0, data: "q Q".into() },
                Item::Unsupported { kind: "leaders".into(), detail: serde_json::Value::String("101".into()) },
            ]}],
            other: vec![Item::Color { stack: 0, cmd: Some(0), data: "0 g 0 G".into() }],
            flags: serde_json::json!({"literal": 1}), glyphs: 3, width: 5000, height: 700, depth: 200,
            page: Some(4), page_width: Some(40000), page_height: Some(52000), origin: Some((4736286, 4736286)),
        }
    }
    #[test]
    fn roundtrip() {
        let dl = sample();
        let bytes = encode(&dl);
        assert_eq!(&bytes[..4], b"LODL");
        let back = decode(&bytes).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&dl).unwrap());
        // unknown record is skipped
        let mut with_unknown = bytes.clone();
        let end = with_unknown.len() - 5; // before END record
        with_unknown.splice(end..end, [0x7E, 3, 0, 0, 0, 1, 2, 3]);
        assert!(decode(&with_unknown).is_ok());
        // truncation is detected
        assert!(decode(&bytes[..bytes.len() - 3]).is_err());
    }
}
