//! Display-list model for rtex.
//!
//! The JSON form produced by `tex/rtex-dl.lua` is the provisional interchange format until the
//! binary encoding is revision 1 (docs/DISPLAY_LIST.md). Coordinates are in scaled points (sp); y grows down.

pub mod binary;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Scaled points (1 pt = 65536 sp; 1 bp = 65781.76 sp).
pub type Sp = i64;
pub const SP_PER_PT: f64 = 65536.0;
pub const SP_PER_BP: f64 = 65536.0 * 72.27 / 72.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FontDesc {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub fullname: Option<String>,
    #[serde(default)]
    pub psname: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub designsize: Option<f64>,
    #[serde(default)]
    pub slant: Option<f64>,
    #[serde(default)]
    pub extend: Option<f64>,
    #[serde(default)]
    pub squeeze: Option<f64>,
    #[serde(default)]
    pub subfont: Option<i64>,
    #[serde(default)]
    pub encodingbytes: Option<i64>,
    #[serde(default)]
    pub embedding: Option<String>,
}

impl FontDesc {
    /// Stable identity across processes: file + subfont + size (+ transforms).
    pub fn key(&self) -> String {
        format!(
            "{}#{}@{}/{}/{}/{}",
            self.filename.clone().or_else(|| self.psname.clone()).or_else(|| self.name.clone()).unwrap_or_default(),
            self.subfont.unwrap_or(0),
            self.size.map(|s| s.round() as i64).unwrap_or(0),
            self.slant.unwrap_or(0.0),
            self.extend.unwrap_or(0.0),
            self.squeeze.unwrap_or(0.0)
        )
    }
}

/// One display-list item. Encoded in JSON as a compact array whose first element is a tag.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// Glyph: font id, char code, glyph index (None for TFM fonts), x, baseline y, advance, expansion factor.
    Glyph { font: i64, char: i64, index: Option<i64>, x: Sp, y: Sp, width: Sp, expansion: i64 },
    /// Filled rectangle: x, top y, width, height.
    Rule { x: Sp, y_top: Sp, width: Sp, height: Sp },
    /// pdf_colorstack whatsit: stack id, command, data (raw PDF color operators).
    Color { stack: i64, cmd: Option<i64>, data: String },
    /// pdf_literal / special passthrough (mode -1 = \special).
    Literal { mode: i64, data: String },
    /// Something the extractor cannot represent; the page is degraded.
    Unsupported { kind: String, detail: serde_json::Value },
    /// Inline math boundary marker (on/off) at x.
    Math { on: bool, x: Sp },
    /// Image placement: engine image resource index, x, top y, width, height.
    Image { index: i64, x: Sp, y_top: Sp, width: Sp, height: Sp },
    /// PDF transformation state: `save` (q), `set` (the matrix "a b c d" applied about (x, y) to
    /// everything up to the matching `restore`), `restore` (Q). graphicx scaling and rotation.
    Matrix { op: String, x: Sp, y: Sp, data: String },
}

impl Serialize for Item {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde_json::{json, Value};
        let v: Value = match self {
            Item::Glyph { font, char, index, x, y, width, expansion } => {
                json!(["g", font, char, index, x, y, width, expansion])
            }
            Item::Rule { x, y_top, width, height } => json!(["r", x, y_top, width, height]),
            Item::Color { stack, cmd, data } => json!(["c", stack, cmd, data]),
            Item::Literal { mode, data } => json!(["l", mode, data]),
            Item::Unsupported { kind, detail } => json!(["u", kind, detail]),
            Item::Math { on, x } => json!(["m", if *on { "on" } else { "off" }, x]),
            Item::Image { index, x, y_top, width, height } => json!(["i", index, x, y_top, width, height]),
            Item::Matrix { op, x, y, data } => json!(["M", op, x, y, data]),
        };
        v.serialize(s)
    }
}

fn num(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64))
}

impl<'de> Deserialize<'de> for Item {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let arr = v.as_array().ok_or_else(|| D::Error::custom("item must be an array"))?;
        let tag = arr.first().and_then(|t| t.as_str()).ok_or_else(|| D::Error::custom("item tag"))?;
        let g = |i: usize| arr.get(i).and_then(num).ok_or_else(|| D::Error::custom(format!("item field {i}")));
        Ok(match tag {
            "g" => Item::Glyph {
                font: g(1)?,
                char: g(2)?,
                index: arr.get(3).and_then(num),
                x: g(4)?,
                y: g(5)?,
                width: g(6)?,
                expansion: g(7).unwrap_or(0),
            },
            "r" => Item::Rule { x: g(1)?, y_top: g(2)?, width: g(3)?, height: g(4)? },
            "c" => Item::Color {
                stack: g(1).unwrap_or(0),
                cmd: arr.get(2).and_then(num),
                data: arr.get(3).and_then(|d| d.as_str()).unwrap_or("").to_string(),
            },
            "l" => Item::Literal {
                mode: g(1).unwrap_or(0),
                data: arr.get(2).and_then(|d| d.as_str()).unwrap_or("").to_string(),
            },
            "u" => Item::Unsupported {
                kind: arr.get(1).and_then(|d| d.as_str()).unwrap_or("?").to_string(),
                detail: arr.get(2).cloned().unwrap_or(serde_json::Value::Null),
            },
            "m" => Item::Math { on: arr.get(1).and_then(|d| d.as_str()) == Some("on"), x: g(2)? },
            "i" => Item::Image { index: g(1).unwrap_or(0), x: g(2)?, y_top: g(3)?, width: g(4)?, height: g(5)? },
            "M" => Item::Matrix {
                op: arr.get(1).and_then(|d| d.as_str()).unwrap_or("set").to_string(),
                x: g(2)?,
                y: g(3)?,
                data: arr.get(4).and_then(|d| d.as_str()).unwrap_or("").to_string(),
            },
            other => return Err(D::Error::custom(format!("unknown item tag {other}"))),
        })
    }
}

/// A typeset line (an hlist). In paragraph lists `par` is 0; in page lists it is the capture
/// paragraph sequence number.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    #[serde(default)]
    pub par: i64,
    #[serde(default)]
    pub i: i64,
    /// Capture unit (fast-path region) owning this row on a page; 0 when none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unit: i64,
    /// 1-based row index within the unit (page lists); 0 when unknown.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub row: i64,
    pub x: Sp,
    pub y: Sp,
    pub w: Sp,
    pub h: Sp,
    pub d: Sp,
    #[serde(default)]
    pub gs: f64,
    #[serde(default)]
    pub gsign: i64,
    #[serde(default)]
    pub gorder: i64,
    #[serde(default)]
    pub items: Vec<Item>,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Lua encodes an empty table as `[]`; accept that where a map is expected.
fn map_or_empty_array<'de, D: serde::Deserializer<'de>, V: serde::de::DeserializeOwned>(d: D) -> Result<BTreeMap<String, V>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Object(m) => m.into_iter().map(|(k, v)| serde_json::from_value(v).map(|x| (k, x)).map_err(serde::de::Error::custom)).collect(),
        _ => Ok(BTreeMap::new()),
    }
}

/// Source of an image resource index used by `Item::Image`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ImageInfo {
    #[serde(default)]
    pub index: i64,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DisplayList {
    pub kind: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub fonts: BTreeMap<String, FontDesc>,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub other: Vec<Item>,
    /// Degradation flags (a JSON object; an empty Lua table arrives as `[]`).
    #[serde(default)]
    pub flags: serde_json::Value,
    #[serde(default)]
    pub glyphs: i64,
    /// Insert nodes (footnotes, \marginpar) seen in a paragraph box: their text is not part of
    /// this list and keeps the page's version until the next layout.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub inserts: i64,
    /// Image resource indices → files (from the capture or the server).
    #[serde(default, rename = "images_info", skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "map_or_empty_array")]
    pub images: BTreeMap<String, ImageInfo>,
    #[serde(default)]
    pub width: Sp,
    #[serde(default)]
    pub height: Sp,
    #[serde(default)]
    pub depth: Sp,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub page_width: Option<Sp>,
    #[serde(default)]
    pub page_height: Option<Sp>,
    #[serde(default)]
    pub origin: Option<(Sp, Sp)>,
}

impl DisplayList {
    pub fn from_binary(bytes: &[u8]) -> Result<Self, binary::BinError> {
        binary::decode(bytes)
    }
    pub fn to_binary(&self) -> Vec<u8> {
        binary::encode(self)
    }
    /// Degradation flags as a map (empty when the extractor saw nothing unsupported).
    pub fn flags_map(&self) -> BTreeMap<String, serde_json::Value> {
        match &self.flags {
            serde_json::Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            _ => BTreeMap::new(),
        }
    }
    pub fn is_exact(&self) -> bool {
        self.flags_map().is_empty()
    }
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
    pub fn font(&self, id: i64) -> Option<&FontDesc> {
        self.fonts.get(&id.to_string())
    }
    pub fn glyph_count(&self) -> usize {
        self.lines.iter().map(|l| l.items.iter().filter(|i| matches!(i, Item::Glyph { .. })).count()).sum::<usize>()
            + self.other.iter().filter(|i| matches!(i, Item::Glyph { .. })).count()
    }
    /// Rows belonging to capture unit `unit`, in row order.
    pub fn rows_of(&self, unit: i64) -> Vec<&Line> {
        let mut v: Vec<&Line> = self.lines.iter().filter(|l| l.unit == unit).collect();
        v.sort_by_key(|l| l.row);
        v
    }
    /// Lines belonging to capture paragraph `par`, in line order.
    pub fn lines_of(&self, par: i64) -> Vec<&Line> {
        let mut v: Vec<&Line> = self.lines.iter().filter(|l| l.par == par).collect();
        v.sort_by_key(|l| l.i);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn item_roundtrip() {
        let s = r#"{"kind":"paragraph","unit":"sp","fonts":{"27":{"id":27,"size":717619}},"lines":[{"par":0,"i":1,"x":0,"y":10,"w":100,"h":5,"d":2,"gs":0.5,"gsign":1,"gorder":0,"items":[["g",27,65,36,0,10,50,-17000],["r",1,2,3,4],["c",0,null,"0 g"],["m","on",7]]}],"other":[],"flags":{},"glyphs":1}"#;
        let dl = DisplayList::from_json(s).unwrap();
        assert_eq!(dl.lines[0].items.len(), 4);
        assert_eq!(dl.glyph_count(), 1);
        let back = serde_json::to_string(&dl).unwrap();
        let dl2 = DisplayList::from_json(&back).unwrap();
        assert_eq!(dl, dl2);
    }
}
