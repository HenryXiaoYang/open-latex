//! Picture cache for background passes. A TikZ-heavy document spends most of a layout pass
//! drawing pictures that did not change. Each pass records where every picture environment
//! landed (page, position, dimensions; `rtex-capture.lua`), and the next pass replaces every
//! picture whose source and surroundings are unchanged with that region of the earlier pass's
//! PDF (an image of exactly the picture's size), so the pass only typesets what changed.
//!
//! What identifies a picture: its environment's text, plus everything that can change its
//! rendering without changing its text: the preamble, and the definitions and settings made in
//! the document body before it (`\def`, `\newcommand`, `\tikzset`, `\definecolor` …).
//! Pictures that depend on more than that (labels, references, counters, external files,
//! `remember picture`/`overlay`, nested pictures) are never cached.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Picture environments the cache handles (each has its own `\end{…}` delimiter).
pub const PICTURE_ENVS: &[&str] = &["tikzpicture", "circuitikz"];

/// A picture environment found in the document sources.
#[derive(Debug, Clone)]
pub struct PictureRef {
    /// `file:line` of its `\begin{…}` (what the capture reports).
    pub key: String,
    pub env: String,
    pub hash: u64,
    pub cacheable: bool,
}

/// Where a pass drew a picture (`pics` in the capture JSON).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecordedPic {
    #[serde(default)]
    pub env: String,
    pub page: i64,
    /// Page coordinates (sp from the page's top-left): left edge and baseline.
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub d: i64,
    pub page_height: i64,
    /// Font, color and width in force where the picture began (the capture compares it before
    /// using the cached picture).
    #[serde(default)]
    pub state: String,
}

/// A cached picture: a region of a kept pass PDF.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub env: String,
    #[serde(default)]
    pub state: String,
    pub pdf: String,
    pub page: i64,
    /// PDF-space bounding box (llx, lly, urx, ury) in sp of a bp (`img.new{bbox}`).
    pub bbox: [i64; 4],
    pub w: i64,
    pub h: i64,
    pub d: i64,
    /// Pass serial the entry was last wanted in (for eviction).
    pub last_used: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    serial: u64,
    entries: BTreeMap<String, CacheEntry>, // hash (decimal) -> entry
}

pub struct PicCache {
    dir: PathBuf,
    index: Index,
}

/// Passes an entry survives without being wanted (edits that toggle a picture back and forth).
const KEEP_PASSES: u64 = 4;

impl PicCache {
    pub fn open(dir: &Path) -> PicCache {
        // the manifest names the cached PDFs by absolute path: lualatex runs in the snapshot
        // directory, not where the host started
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let index = std::fs::read_to_string(dir.join("index.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        PicCache { dir, index }
    }

    pub fn entries(&self) -> usize {
        self.index.entries.len()
    }

    fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(
            self.dir.join("index.json"),
            serde_json::to_vec(&self.index)?,
        )?;
        Ok(())
    }

    /// Write the manifest the next capture pass reads: every current picture the cache holds,
    /// keyed by its `file:line`. Returns the number of hits. Removes the file when there are
    /// none (the capture then draws everything).
    pub fn write_manifest(&mut self, pics: &[PictureRef], manifest: &Path) -> Result<usize> {
        self.index.serial += 1;
        let serial = self.index.serial;
        let mut m: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        for p in pics.iter().filter(|p| p.cacheable) {
            if let Some(e) = self.index.entries.get_mut(&p.hash.to_string()) {
                if e.env != p.env || !self.dir.join(&e.pdf).exists() {
                    continue;
                }
                e.last_used = serial;
                m.insert(
                    p.key.clone(),
                    serde_json::json!({
                        "env": e.env,
                        "state": e.state,
                        "pdf": self.dir.join(&e.pdf).to_string_lossy(),
                        "page": e.page, "bbox": e.bbox, "w": e.w, "h": e.h, "d": e.d,
                    }),
                );
            }
        }
        if m.is_empty() {
            let _ = std::fs::remove_file(manifest);
        } else {
            std::fs::write(manifest, serde_json::to_vec(&m)?)
                .with_context(|| format!("writing {}", manifest.display()))?;
        }
        let _ = self.save();
        Ok(m.len())
    }

    /// After a pass: remember every current picture the pass drew (its region of `pass_pdf`,
    /// copied into the cache), evict what no picture wanted for a while, drop PDFs nothing
    /// references. Returns the number of new entries.
    pub fn absorb(
        &mut self,
        pics: &[PictureRef],
        recorded: &BTreeMap<String, RecordedPic>,
        pass_pdf: &Path,
    ) -> Result<usize> {
        let serial = self.index.serial;
        let mut copied: Option<String> = None;
        let mut added = 0;
        for p in pics.iter().filter(|p| p.cacheable) {
            let hk = p.hash.to_string();
            let Some(r) = recorded.get(&p.key) else {
                continue;
            };
            // an entry drawn under another state (font, color, width) is replaced
            if self
                .index
                .entries
                .get(&hk)
                .map(|e| e.state == r.state)
                .unwrap_or(false)
            {
                continue;
            }
            if r.env != p.env || r.w <= 0 || r.h + r.d <= 0 {
                continue;
            }
            let pdf = match &copied {
                Some(name) => name.clone(),
                None => {
                    std::fs::create_dir_all(&self.dir)?;
                    let name = format!("p{serial}.pdf");
                    std::fs::copy(pass_pdf, self.dir.join(&name)).with_context(|| {
                        format!("copying {} into the picture cache", pass_pdf.display())
                    })?;
                    copied = Some(name.clone());
                    name
                }
            };
            // PDF user space: x unchanged (sp of a bp == sp), y measured from the page bottom
            let bbox = [
                r.x,
                r.page_height - (r.y + r.d),
                r.x + r.w,
                r.page_height - (r.y - r.h),
            ];
            self.index.entries.insert(
                hk,
                CacheEntry {
                    env: p.env.clone(),
                    state: r.state.clone(),
                    pdf,
                    page: r.page,
                    bbox,
                    w: r.w,
                    h: r.h,
                    d: r.d,
                    last_used: serial,
                },
            );
            added += 1;
        }
        // eviction: entries no current picture wants for KEEP_PASSES passes
        self.index
            .entries
            .retain(|_, e| e.last_used + KEEP_PASSES >= serial);
        // PDFs nothing references any more
        let referenced: BTreeSet<&str> = self
            .index
            .entries
            .values()
            .map(|e| e.pdf.as_str())
            .collect();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                if name.starts_with('p')
                    && name.ends_with(".pdf")
                    && !referenced.contains(name.as_str())
                {
                    let _ = std::fs::remove_file(ent.path());
                }
            }
        }
        self.save()?;
        Ok(added)
    }
}

/// Text that makes a picture depend on more than its own source and the definitions before it.
const UNCACHEABLE: &[&str] = &[
    "\\label",
    "\\ref",
    "\\pageref",
    "\\eqref",
    "\\cite",
    "remember picture",
    "overlay",
    "\\verb",
    "\\input",
    "\\include",
    "\\includegraphics",
    "\\pgfplotstableread",
    "table {",
    "table{",
    "\\the",
    "\\value",
    "\\arabic",
    "\\roman",
    "\\alph",
    "\\today",
    "\\footnote",
    "\\pgfmathrandom",
    "\\random",
    "\\pgfmathsetseed",
    "\\pdfsavepos",
    "\\savepos",
    "\\write",
    "\\newcounter",
    "\\stepcounter",
    "\\refstepcounter",
    "\\addtocounter",
    "\\setcounter",
    "\\index",
    "\\marginpar",
];

/// Body-level statements whose effect a later picture may depend on (hashed into every
/// picture after them).
const PRELUDE_HEADS: &[&str] = &[
    "\\def",
    "\\edef",
    "\\gdef",
    "\\let",
    "\\newcommand",
    "\\renewcommand",
    "\\providecommand",
    "\\newenvironment",
    "\\renewenvironment",
    "\\tikzset",
    "\\tikzstyle",
    "\\pgfplotsset",
    "\\pgfkeys",
    "\\definecolor",
    "\\colorlet",
    "\\setlength",
    "\\newlength",
    "\\pgfmathsetmacro",
    "\\pgfmathsetlengthmacro",
    "\\pgfdeclare",
    "\\ctikzset",
    "\\usetikzlibrary",
    "\\usepgfplotslibrary",
    "\\newcolumntype",
    "\\linespread",
    "\\selectfont",
];

fn hash_bytes(h: &mut std::collections::hash_map::DefaultHasher, s: &str) {
    use std::hash::Hasher;
    h.write(s.as_bytes());
    h.write_u8(0);
}

/// Strip an unescaped `%` comment from a source line.
fn uncomment(line: &str) -> &str {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == b'%' {
            return &line[..i];
        }
        i += 1;
    }
    line
}

/// Every picture environment in the project's body text, with its hash (preamble hash, the
/// definitions made before it, its own text) and whether it may be cached.
pub fn scan_pictures(
    texts: &BTreeMap<String, String>,
    main: &str,
    preamble_hash: u64,
) -> Vec<PictureRef> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let mut out = Vec::new();
    let has_remember = texts.values().any(|t| t.contains("remember picture"));
    // definitions in document order: the main file's body, then the other files
    let mut order: Vec<(&String, &String)> = Vec::new();
    if let Some((k, t)) = texts.get_key_value(main) {
        order.push((k, t));
    }
    for (n, t) in texts {
        if n != main {
            order.push((n, t));
        }
    }
    let mut prelude = DefaultHasher::new();
    prelude.write_u64(preamble_hash);
    for (name, text) in order {
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        // skip the preamble of the main file
        if name == main {
            if let Some(k) = lines.iter().position(|l| l.contains("\\begin{document}")) {
                i = k + 1;
            }
        }
        while i < lines.len() {
            let line = uncomment(lines[i]);
            let trimmed = line.trim_start();
            let mut opened: Option<&str> = None;
            for env in PICTURE_ENVS {
                if trimmed.contains(&format!("\\begin{{{env}}}")) {
                    opened = Some(env);
                    break;
                }
            }
            if let Some(env) = opened {
                let begin_marker = format!("\\begin{{{env}}}");
                let end_marker = format!("\\end{{{env}}}");
                let start = i;
                let mut depth = 0i32;
                let mut end = None;
                let mut j = i;
                while j < lines.len() {
                    let l = uncomment(lines[j]);
                    depth += l.matches(&begin_marker).count() as i32;
                    depth -= l.matches(&end_marker).count() as i32;
                    if depth <= 0 {
                        end = Some(j);
                        break;
                    }
                    j += 1;
                }
                let Some(end) = end else { break };
                let body: Vec<&str> = lines[start..=end].iter().map(|l| uncomment(l)).collect();
                let text_all = body.join("\n");
                let mut cacheable = !has_remember
                    && text_all.matches(&begin_marker).count() == 1
                    && !UNCACHEABLE.iter().any(|u| text_all.contains(u))
                    && !lines[start].contains("\\end{"); // begin and end on one line with text around: keep simple
                                                         // another picture starting on the same line: keys would collide
                if uncomment(lines[start]).matches("\\begin{").count() > 1 {
                    cacheable = false;
                }
                let mut h = prelude.clone();
                hash_bytes(&mut h, &text_all);
                let key = format!("{}:{}", name.trim_start_matches("./"), start + 1);
                out.push(PictureRef {
                    key,
                    env: env.to_string(),
                    hash: h.finish(),
                    cacheable,
                });
                i = end + 1;
                continue;
            }
            if PRELUDE_HEADS.iter().any(|h| trimmed.starts_with(h)) {
                hash_bytes(&mut prelude, trimmed);
            }
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_and_hash() {
        let mut texts = BTreeMap::new();
        texts.insert(
            "main.tex".to_string(),
            "\\documentclass{article}\n\\begin{document}\n\\def\\H{4}\nText.\n\\begin{tikzpicture}\n  \\draw (0,0) -- (\\H,1);\n\\end{tikzpicture}\n\n\\begin{tikzpicture}[remember picture]\n\\end{tikzpicture}\n\\begin{tikzpicture}\n\\node {\\ref{x}};\n\\end{tikzpicture}\n\\end{document}\n".to_string(),
        );
        let pics = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(pics.len(), 3);
        assert!(
            !pics.iter().any(|p| p.cacheable),
            "remember picture disables the cache"
        );
        let t = texts.get_mut("main.tex").unwrap();
        *t = t.replace("[remember picture]", "");
        let pics = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(pics[0].key, "main.tex:5");
        assert!(pics[0].cacheable && pics[1].cacheable && !pics[2].cacheable);
        let h0 = pics[0].hash;
        // a definition before the picture changes its hash; text after it does not
        let t = texts.get_mut("main.tex").unwrap();
        *t = t.replace("\\def\\H{4}", "\\def\\H{5}");
        let pics2 = scan_pictures(&texts, "main.tex", 1);
        assert_ne!(pics2[0].hash, h0);
        let t = texts.get_mut("main.tex").unwrap();
        *t = t.replace("Text.", "Other text.");
        let pics3 = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(pics3[0].hash, pics2[0].hash);
        // preamble hash participates
        assert_ne!(scan_pictures(&texts, "main.tex", 2)[0].hash, pics3[0].hash);
    }

    #[test]
    fn cache_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rtex-piccache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pdf = dir.join("pass.pdf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&pdf, b"%PDF-1.5 fake").unwrap();
        let pics = vec![PictureRef {
            key: "main.tex:5".into(),
            env: "tikzpicture".into(),
            hash: 42,
            cacheable: true,
        }];
        let mut cache = PicCache::open(&dir.join("cache"));
        let manifest = dir.join("pic-manifest.json");
        assert_eq!(cache.write_manifest(&pics, &manifest).unwrap(), 0);
        assert!(!manifest.exists());
        let mut rec = BTreeMap::new();
        rec.insert(
            "main.tex:5".to_string(),
            RecordedPic {
                env: "tikzpicture".into(),
                page: 2,
                x: 100,
                y: 1000,
                w: 300,
                h: 200,
                d: 50,
                page_height: 5000,
                state: "font/0 g 0 G".into(),
            },
        );
        assert_eq!(cache.absorb(&pics, &rec, &pdf).unwrap(), 1);
        assert_eq!(cache.write_manifest(&pics, &manifest).unwrap(), 1);
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        let e = &m["main.tex:5"];
        assert_eq!(e["page"], 2);
        assert_eq!(
            e["bbox"],
            serde_json::json!([100, 5000 - 1050, 400, 5000 - 800])
        );
        assert_eq!(e["d"], 50);
        assert_eq!(e["state"], "font/0 g 0 G");
        // the same picture drawn under another state (font, color, width) replaces the entry
        let mut rec2 = rec.clone();
        rec2.get_mut("main.tex:5").unwrap().state = "other font/0 g 0 G".into();
        rec2.get_mut("main.tex:5").unwrap().h = 400;
        assert_eq!(cache.absorb(&pics, &rec2, &pdf).unwrap(), 1);
        assert_eq!(cache.entries(), 1);
        cache.write_manifest(&pics, &manifest).unwrap();
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(m["main.tex:5"]["state"], "other font/0 g 0 G");
        assert_eq!(m["main.tex:5"]["h"], 400);
        // a changed picture: no hit; after KEEP_PASSES unwanted passes the entry and its PDF go
        let changed = vec![PictureRef {
            hash: 43,
            ..pics[0].clone()
        }];
        for _ in 0..(KEEP_PASSES + 1) {
            assert_eq!(cache.write_manifest(&changed, &manifest).unwrap(), 0);
            cache.absorb(&changed, &BTreeMap::new(), &pdf).unwrap();
        }
        assert_eq!(cache.entries(), 0);
        assert!(std::fs::read_dir(dir.join("cache"))
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".pdf")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
