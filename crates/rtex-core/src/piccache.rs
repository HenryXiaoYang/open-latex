//! Picture cache for background passes. A TikZ-heavy document spends most of a layout pass
//! drawing pictures that did not change. Each pass records where every picture environment
//! landed (page, position, dimensions; `rtex-capture.lua`), and the next pass replaces every
//! picture whose source and surroundings are unchanged with that region of the earlier pass's
//! PDF (an image of exactly the picture's size), so the pass only typesets what changed.
//!
//! What identifies a picture: its environment's text, plus everything that can change its
//! rendering without changing its text: the preamble, and the definitions and settings made in
//! the document body before it (`\def`, `\newcommand`, `\tikzset`, `\definecolor` …), in the
//! order TeX reads them (`\input` chains followed). The font, color and width in force at the
//! picture are recorded by the capture and compared at use (`RecordedPic::state`). Pictures
//! that depend on more than that (labels, references, counters, external files, data files,
//! `remember picture`/`overlay`, group-escaping assignments) are never cached.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Picture environments: units of their own when they start a line, inline boxes after text;
/// the cache handles each (every one has its own `\end{…}` delimiter). The capture's list
/// (`rtex_capture.PICTURE_ENVS`) names the same environments.
pub const PICTURE_ENVS: &[&str] = &["tikzpicture", "circuitikz", "pgfpicture"];

/// A picture environment found in the document sources.
#[derive(Debug, Clone)]
pub struct PictureRef {
    /// `file:line` of its `\begin{…}` (what the capture reports).
    pub key: String,
    pub env: String,
    pub hash: u64,
    pub cacheable: bool,
    /// 1-based line of its `\end{…}` (the capture checks it after skipping the body).
    pub end_line: usize,
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
    /// Names the PDFs of this index's lifetime (`p<epoch>-<serial>.pdf`): an index lost and
    /// rebuilt from scratch never reuses a name a reader may still hold.
    #[serde(default)]
    epoch: u64,
    entries: BTreeMap<String, CacheEntry>, // hash (decimal) -> entry
    /// `file:line` keys whose cached body did not end where the source scan said (a pass
    /// reported them mismatched), with the serial until which they are drawn, not cached.
    #[serde(default)]
    bad: BTreeMap<String, u64>,
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
        let mut index: Index = std::fs::read_to_string(dir.join("index.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if index.epoch == 0 {
            index.epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(1)
                .max(1);
        }
        PicCache { dir, index }
    }

    pub fn entries(&self) -> usize {
        self.index.entries.len()
    }

    fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        write_atomic(
            &self.dir.join("index.json"),
            &serde_json::to_vec(&self.index)?,
        )
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
                e.last_used = serial;
            }
            if let Some(v) = self.entry_json(p) {
                m.insert(p.key.clone(), v);
            }
        }
        if m.is_empty() {
            let _ = std::fs::remove_file(manifest);
        } else {
            write_atomic(manifest, &serde_json::to_vec(&m)?)?;
        }
        let _ = self.save();
        Ok(m.len())
    }

    /// The cache entry for a picture, as the capture and the live engine read it (`pdf` by
    /// absolute path, `bbox` in PDF user space, `state` to compare, `end_line` of the source).
    pub fn entry_json(&self, p: &PictureRef) -> Option<serde_json::Value> {
        if !p.cacheable || self.index.bad.contains_key(&p.key) {
            return None;
        }
        let e = self.index.entries.get(&p.hash.to_string())?;
        if e.env != p.env || !self.dir.join(&e.pdf).exists() {
            return None;
        }
        Some(serde_json::json!({
            "env": e.env,
            "state": e.state,
            "end_line": p.end_line,
            "pdf": self.dir.join(&e.pdf).to_string_lossy(),
            "page": e.page, "bbox": e.bbox, "w": e.w, "h": e.h, "d": e.d,
        }))
    }

    /// After a pass: remember every current picture the pass drew (its page of `pass_pdf`,
    /// extracted into the cache), forget the pictures the pass reported as mismatched
    /// (`stale_keys`: a cached body that did not end where the source scan said), evict what
    /// no picture wanted for a while, drop PDFs nothing references. Returns the number of new
    /// entries.
    pub fn absorb(
        &mut self,
        pics: &[PictureRef],
        recorded: &BTreeMap<String, RecordedPic>,
        stale_keys: &[String],
        pass_pdf: &Path,
    ) -> Result<usize> {
        let serial = self.index.serial;
        // a mismatched key is drawn for a while instead of cached again right away (the
        // mismatch would repeat on every other pass)
        self.index.bad.retain(|_, until| *until >= serial);
        for k in stale_keys {
            if let Some(p) = pics.iter().find(|p| &p.key == k) {
                self.index.entries.remove(&p.hash.to_string());
            }
            self.index.bad.insert(k.clone(), serial + KEEP_PASSES);
        }
        let mut new: Vec<(&PictureRef, &RecordedPic)> = Vec::new();
        for p in pics
            .iter()
            .filter(|p| p.cacheable && !self.index.bad.contains_key(&p.key))
        {
            let Some(r) = recorded.get(&p.key) else {
                continue;
            };
            if r.env != p.env || r.w <= 0 || r.h + r.d <= 0 {
                continue;
            }
            // an entry drawn under another state (font, color, width), or whose PDF is gone,
            // is replaced
            if self
                .index
                .entries
                .get(&p.hash.to_string())
                .map(|e| e.state == r.state && self.dir.join(&e.pdf).exists())
                .unwrap_or(false)
            {
                continue;
            }
            new.push((p, r));
        }
        let added = new.len();
        if !new.is_empty() {
            std::fs::create_dir_all(&self.dir)?;
            let name = format!("p{}-{serial}.pdf", self.index.epoch);
            // only the pages that carry new pictures (a 100-page PDF per edited picture
            // would fill the disk); page numbers are remapped onto the extract
            let pages: Vec<i64> = new
                .iter()
                .map(|(_, r)| r.page)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let remap: BTreeMap<i64, i64> =
                match extract_pages(pass_pdf, &pages, &self.dir.join(&name)) {
                    Ok(()) => pages
                        .iter()
                        .enumerate()
                        .map(|(i, p)| (*p, i as i64 + 1))
                        .collect(),
                    Err(e) => {
                        log::warn!(
                            "picture cache: page extraction failed ({e:#}), copying the pass PDF"
                        );
                        std::fs::copy(pass_pdf, self.dir.join(&name)).with_context(|| {
                            format!("copying {} into the picture cache", pass_pdf.display())
                        })?;
                        pages.iter().map(|p| (*p, *p)).collect()
                    }
                };
            for (p, r) in new {
                // PDF user space: x unchanged (sp of a bp == sp), y measured from the page bottom
                let bbox = [
                    r.x,
                    r.page_height - (r.y + r.d),
                    r.x + r.w,
                    r.page_height - (r.y - r.h),
                ];
                self.index.entries.insert(
                    p.hash.to_string(),
                    CacheEntry {
                        env: p.env.clone(),
                        state: r.state.clone(),
                        pdf: name.clone(),
                        page: remap[&r.page],
                        bbox,
                        w: r.w,
                        h: r.h,
                        d: r.d,
                        last_used: serial,
                    },
                );
            }
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

/// Write `data` to `path` through a temporary file and a rename: a reader (the live engine's
/// index lookup, the capture's manifest read) never sees a torn file.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

/// Write the given pages (1-based, ascending) of `src` as `dst`, in that order.
fn extract_pages(src: &Path, pages: &[i64], dst: &Path) -> Result<()> {
    let mut doc = lopdf::Document::load(src).context("loading the pass PDF")?;
    let all: Vec<u32> = doc.get_pages().keys().copied().collect();
    let keep: BTreeSet<u32> = pages.iter().map(|p| *p as u32).collect();
    if !keep.iter().all(|p| all.contains(p)) {
        anyhow::bail!("page out of range");
    }
    let delete: Vec<u32> = all.into_iter().filter(|p| !keep.contains(p)).collect();
    doc.delete_pages(&delete);
    doc.prune_objects();
    doc.renumber_objects();
    doc.save(dst).context("writing the page extract")?;
    Ok(())
}

/// Control words that make a picture depend on more than its own source and the definitions
/// before it (matched as whole words: `\ref`, not `\reflectbox`).
const UNCACHEABLE_WORDS: &[&str] = &[
    // the aux file
    "label",
    "ref",
    "pageref",
    "eqref",
    "footnotemark",
    "footnotetext",
    // links and anchors (not in the page content an image carries)
    "href",
    "hyperref",
    "hyperlink",
    "hypertarget",
    "url",
    "autoref",
    "cref",
    "Cref",
    "vref",
    "nameref",
    // counters, dates
    "the",
    "value",
    "arabic",
    "roman",
    "Roman",
    "alph",
    "Alph",
    "fnsymbol",
    "today",
    "newcounter",
    "stepcounter",
    "refstepcounter",
    "addtocounter",
    "setcounter",
    // other files, verbatim, notes
    "verb",
    "input",
    "pgfimage",
    "includesvg",
    "includepdf",
    "lstinputlisting",
    "include",
    "footnote",
    "index",
    "marginpar",
    "write",
    "immediate",
    // randomness, positions
    "pgfmathrandom",
    "random",
    "pgfmathsetseed",
    "pdfsavepos",
    "savepos",
    // assignments and boxes that escape the picture's group: a skipped body must have no
    // effect on what follows it
    "global",
    "xdef",
    "gdef",
    "savebox",
    "sbox",
    "pgfdeclarelayer",
    "newbox",
    "newsavebox",
    "usebox",
    "setbox",
];

/// Control-word prefixes (`\citep`, `\includegraphics*`, `\pgfplotstableread` …).
const UNCACHEABLE_PREFIXES: &[&str] = &["cite", "includegraphics", "pgfplotstable", "footcite"];

/// Plain text. `trim left`/`trim right` (pgfplots `trim axis left/right`) are kerns pgf puts
/// outside the picture box, which a cached box does not carry; `legend to name` writes a
/// label.
const UNCACHEABLE_TEXT: &[&str] = &[
    "remember picture",
    "overlay",
    "trim left",
    "trim right",
    "trim axis",
    "legend to name",
];

/// Body-level statements whose effect a later picture may depend on (hashed into every
/// picture after them, the whole statement when it spans lines).
const PRELUDE_HEADS: &[&str] = &[
    "def",
    "edef",
    "gdef",
    "xdef",
    "let",
    "newcommand",
    "renewcommand",
    "providecommand",
    "newenvironment",
    "renewenvironment",
    "tikzset",
    "tikzstyle",
    "pgfplotsset",
    "pgfkeys",
    "definecolor",
    "colorlet",
    "setlength",
    "newlength",
    "pgfmathsetmacro",
    "pgfmathsetlengthmacro",
    "pgfdeclare",
    "ctikzset",
    "usetikzlibrary",
    "usepgfplotslibrary",
    "newcolumntype",
    "linespread",
    "selectfont",
    "pgfdeclarelayer",
    "pgfsetlayers",
];

fn is_letter(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

/// Does `text` contain the control word `\name` (not as a prefix of a longer word)?
fn has_cs(text: &str, name: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(k) = text[from..].find('\\') {
        let start = from + k + 1;
        let end = start + name.len();
        if text[start..].starts_with(name) && (end >= b.len() || !is_letter(b[end])) {
            return true;
        }
        from = start;
    }
    false
}

/// Does `text` print a counter with `\the<counter>` (`\thesection`, `\thepage`, `\theequation`
/// and every user `\the…`)? `\theta` and `\therefore` are symbols, `\the` itself is a word of
/// its own.
fn has_the_counter(text: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(k) = text[from..].find("\\the") {
        let start = from + k + 4;
        let mut end = start;
        while end < b.len() && is_letter(b[end]) {
            end += 1;
        }
        let rest = &text[start..end];
        if !rest.is_empty() && !matches!(rest, "ta" | "refore") {
            return true;
        }
        from = start;
    }
    false
}

/// Does `text` contain a control word starting with `\name`?
fn has_cs_prefix(text: &str, name: &str) -> bool {
    text.contains(&format!("\\{name}"))
}

/// Does the word `word` (letters around it excluded) appear followed by `[` or `{`, like the
/// pgfplots data sources `\addplot table[x=a] {f.dat}` and `\addplot file {f.dat}`?
fn word_before_arg(text: &str, word: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(k) = text[from..].find(word) {
        let start = from + k;
        let end = start + word.len();
        let left_ok = start == 0 || !is_letter(b[start - 1]);
        let mut j = end;
        while j < b.len() && (b[j] == b' ' || b[j] == b'\t' || b[j] == b'\n') {
            j += 1;
        }
        if left_ok
            && j < b.len()
            && (b[j] == b'[' || b[j] == b'{')
            && (end == b.len() || !is_letter(b[end]))
        {
            return true;
        }
        from = end;
    }
    false
}

/// Does a picture with this (comment-stripped) text depend on more than its source and the
/// definitions before it?
pub fn uncacheable(text: &str) -> bool {
    UNCACHEABLE_WORDS.iter().any(|w| has_cs(text, w))
        || has_the_counter(text)
        || UNCACHEABLE_PREFIXES.iter().any(|w| has_cs_prefix(text, w))
        || UNCACHEABLE_TEXT.iter().any(|t| text.contains(t))
        || word_before_arg(text, "table")
        || word_before_arg(text, "file")
}

fn hash_bytes(h: &mut std::collections::hash_map::DefaultHasher, s: &str) {
    use std::hash::Hasher;
    h.write(s.as_bytes());
    h.write_u8(0);
}

/// Hash of the preamble (with its `\input`s expanded) for the picture hashes: every picture
/// depends on it.
pub fn preamble_hash(texts: &BTreeMap<String, String>, main: &str) -> u64 {
    texts
        .get(main)
        .and_then(|t| crate::split_preamble(t))
        .map(|(p, _)| crate::document::hash_str(&crate::document::expand_inputs(p, texts)))
        .unwrap_or(0)
}

/// Number of lines, from `lines[0]`, a statement spans: until its braces balance (a `\tikzset{`
/// block over several lines). At least 1, at most 64.
fn statement_lines(lines: &[&str]) -> usize {
    let mut depth = 0i32;
    for (n, line) in lines.iter().enumerate().take(64) {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 1,
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        if depth <= 0 {
            return n + 1;
        }
    }
    lines.len().clamp(1, 64)
}

/// Every picture environment in the project's body text, with its hash (preamble hash, the
/// definitions made before it, its own text) and whether it may be cached.
pub fn scan_pictures(
    texts: &BTreeMap<String, String>,
    main: &str,
    preamble_hash: u64,
) -> Vec<PictureRef> {
    use crate::document::strip_comment;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let body_start = |name: &str| -> usize {
        if name != main {
            return 0;
        }
        texts[name]
            .lines()
            .position(|l| l.contains("\\begin{document}"))
            .map(|k| k + 1)
            .unwrap_or(0)
    };
    // `remember picture` anywhere (comments aside; the preamble too: `\tikzset{every
    // picture/.style={remember picture}}`) disables the cache: such pictures place material
    // relative to others
    let has_remember = texts.values().any(|text| {
        text.lines()
            .any(|l| strip_comment(l).contains("remember picture"))
    });
    struct Walker<'a> {
        texts: &'a BTreeMap<String, String>,
        prelude: DefaultHasher,
        out: Vec<PictureRef>,
        has_remember: bool,
        stack: Vec<String>,
        /// How often each file was read: a file `\input` twice yields two pictures per
        /// `file:line` key, which the capture cannot tell apart.
        visits: BTreeMap<String, u32>,
    }
    impl Walker<'_> {
        fn walk(&mut self, name: &str, start: usize) {
            if self.stack.len() > 8 || self.stack.iter().any(|s| s == name) {
                return;
            }
            *self.visits.entry(name.to_string()).or_default() += 1;
            self.stack.push(name.to_string());
            let text = &self.texts[name];
            let lines: Vec<&str> = text.lines().collect();
            let mut i = start;
            while i < lines.len() {
                let line = strip_comment(lines[i]);
                let trimmed = line.trim_start();
                let opened = PICTURE_ENVS
                    .iter()
                    .find(|e| trimmed.contains(&format!("\\begin{{{e}}}")));
                if let Some(env) = opened {
                    let begin_marker = format!("\\begin{{{env}}}");
                    let end_marker = format!("\\end{{{env}}}");
                    let start = i;
                    let mut depth = 0i32;
                    let mut end = None;
                    let mut j = i;
                    while j < lines.len() {
                        let l = strip_comment(lines[j]);
                        depth += l.matches(&begin_marker).count() as i32;
                        depth -= l.matches(&end_marker).count() as i32;
                        if depth <= 0 {
                            end = Some(j);
                            break;
                        }
                        j += 1;
                    }
                    let Some(end) = end else { break };
                    let body: Vec<&str> = lines[start..=end]
                        .iter()
                        .map(|l| strip_comment(l))
                        .collect();
                    let text_all = body.join("\n");
                    // the capture keys a picture by the line its \begin executes on, which is
                    // this line only when nothing precedes the \begin on it (a macro argument
                    // closing here would execute its pictures with this line number)
                    let cacheable = !self.has_remember
                        && trimmed.starts_with(&begin_marker)
                        && text_all.matches(&begin_marker).count() == 1
                        && !line.contains("\\end{")
                        && !uncacheable(&text_all);
                    let mut h = self.prelude.clone();
                    hash_bytes(&mut h, &text_all);
                    self.out.push(PictureRef {
                        key: format!("{}:{}", name.trim_start_matches("./"), start + 1),
                        env: env.to_string(),
                        hash: h.finish(),
                        cacheable,
                        end_line: end + 1,
                    });
                    i = end + 1;
                    continue;
                }
                // files read here come before everything after this line
                for (_, target, _) in crate::document::find_inputs(line) {
                    if self.texts.contains_key(&target) {
                        self.walk(&target, 0);
                    }
                }
                if PRELUDE_HEADS.iter().any(|h| has_cs(line, h)) {
                    let n = statement_lines(&lines[i..]);
                    for l in &lines[i..i + n] {
                        hash_bytes(&mut self.prelude, strip_comment(l).trim());
                    }
                    i += n;
                    continue;
                }
                i += 1;
            }
            self.stack.pop();
        }
    }
    let mut prelude = DefaultHasher::new();
    prelude.write_u64(preamble_hash);
    let mut w = Walker {
        texts,
        prelude,
        out: Vec::new(),
        has_remember,
        stack: Vec::new(),
        visits: BTreeMap::new(),
    };
    if texts.contains_key(main) {
        w.walk(main, body_start(main));
    }
    let twice: Vec<&str> = w
        .visits
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(f, _)| f.trim_start_matches("./"))
        .collect();
    if !twice.is_empty() {
        for p in &mut w.out {
            if p.key
                .rsplit_once(':')
                .is_some_and(|(f, _)| twice.contains(&f))
            {
                p.cacheable = false;
            }
        }
    }
    w.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts_of(main: &str) -> BTreeMap<String, String> {
        let mut t = BTreeMap::new();
        t.insert("main.tex".to_string(), main.to_string());
        t
    }

    #[test]
    fn scan_and_hash() {
        let mut texts = texts_of(
            "\\documentclass{article}\n\\begin{document}\n\\def\\H{4}\nText.\n\\begin{tikzpicture}\n  \\draw (0,0) -- (\\H,1);\n\\end{tikzpicture}\n\n\\begin{tikzpicture}[remember picture]\n\\end{tikzpicture}\n\\begin{tikzpicture}\n\\node {\\ref{x}};\n\\end{tikzpicture}\n\\end{document}\n",
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
        assert_eq!(pics[0].end_line, 7);
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
        // the preamble is part of every hash
        let pics4 = scan_pictures(&texts, "main.tex", 2);
        assert_ne!(pics4[0].hash, pics3[0].hash);
        // a comment mentioning remember picture does not disable the cache
        let t = texts.get_mut("main.tex").unwrap();
        *t = t.replace("Other text.", "Other text. % use remember picture one day");
        assert!(scan_pictures(&texts, "main.tex", 1)[0].cacheable);
        // remember picture set in the preamble (tikzmark, overlays) disables it as well
        let t = texts.get_mut("main.tex").unwrap();
        *t = t.replace(
            "\\documentclass{article}\n",
            "\\documentclass{article}\n\\tikzset{every picture/.style={remember picture}}\n",
        );
        assert!(!scan_pictures(&texts, "main.tex", 1)[0].cacheable);
    }

    #[test]
    fn counters_links_and_double_inputs_are_not_cached() {
        // \the<counter> prints a number the picture's text does not show
        assert!(uncacheable("\\node {Section \\thesection};"));
        assert!(uncacheable("\\node {p.~\\thepage};"));
        assert!(!uncacheable("\\draw (0,0) -- (\\theta:1);"));
        assert!(!uncacheable("\\node {$\\therefore x$};"));
        assert!(uncacheable("\\node {\\href{https://x.y}{link}};"));
        assert!(uncacheable("\\node {text\\footnotemark};"));
        assert!(uncacheable("\\pgfimage{fig}"));
        // a file read twice: its pictures share their file:line keys
        let mut texts = texts_of(
            "\\begin{document}\n\\input{fig}\n\\def\\H{2}\n\\input{fig}\n\\end{document}\n",
        );
        texts.insert(
            "fig.tex".into(),
            "\\begin{tikzpicture}\n\\draw (0,0) -- (\\H,1);\n\\end{tikzpicture}\n".into(),
        );
        let pics = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(pics.len(), 2);
        assert_eq!(pics[0].key, pics[1].key);
        assert!(!pics[0].cacheable && !pics[1].cacheable);
        let t = texts.get_mut("main.tex").unwrap();
        *t = t.replacen("\\input{fig}\n", "", 1);
        let pics = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(pics.len(), 1);
        assert!(pics[0].cacheable);
    }

    #[test]
    fn prelude_order_and_multiline_statements() {
        // a multi-line \tikzset: a change on its second line reaches the pictures after it
        let mut texts = texts_of(
            "\\begin{document}\n\\tikzset{%\n  every node/.style={draw,red},\n}\n\\begin{tikzpicture}\n\\node {a};\n\\end{tikzpicture}\n\\input{styles}\n\\begin{tikzpicture}[mystyle]\n\\node {b};\n\\end{tikzpicture}\n\\end{document}\n",
        );
        texts.insert(
            "styles.tex".to_string(),
            "\\centering\\tikzset{mystyle/.style={thick}}\n".to_string(),
        );
        let p1 = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(p1.len(), 2);
        let changed = texts["main.tex"].replace("draw,red", "draw,blue");
        texts.insert("main.tex".to_string(), changed);
        let p2 = scan_pictures(&texts, "main.tex", 1);
        assert_ne!(p1[0].hash, p2[0].hash, "second line of the \\tikzset block");
        // a definition in an \input file (not at the start of its line) reaches the pictures
        // after the \input, not the ones before it
        texts.insert(
            "styles.tex".to_string(),
            "\\centering\\tikzset{mystyle/.style={dashed}}\n".to_string(),
        );
        let p3 = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(p2[0].hash, p3[0].hash);
        assert_ne!(p2[1].hash, p3[1].hash);
    }

    #[test]
    fn cacheability_rules() {
        assert!(!uncacheable(
            "\\node {$\\alpha$}; \\draw (0,0) -- (\\theta:1);"
        ));
        assert!(!uncacheable(
            "\\reflectbox{x} \\romannumeral 4 \\indexspace"
        ));
        assert!(uncacheable("\\node {\\ref{x}};"));
        assert!(uncacheable("\\node {\\alph{page}};"));
        assert!(uncacheable("\\citep{k}"));
        assert!(uncacheable("\\addplot table[x=t,y=v] {results.dat};"));
        assert!(uncacheable("\\addplot table [col sep=comma] {f.csv};"));
        assert!(uncacheable("\\addplot file {f.dat};"));
        assert!(!uncacheable("\\node {a table of files};"));
        assert!(uncacheable("\\pgfmathsetmacro\\w{3} \\xdef\\figwidth{\\w}"));
        assert!(uncacheable("\\global\\advance\\c by 1"));
        assert!(uncacheable("\\includegraphics*[width=1cm]{f}"));
        // a picture after text on its line, or with text before its \begin, is not keyed by
        // the line its \begin is on
        let texts = texts_of(
            "\\begin{document}\n\\foo{\n\\begin{tikzpicture} a\n\\end{tikzpicture}}\\begin{tikzpicture}\nb\n\\end{tikzpicture}\n\\end{document}\n",
        );
        // the scan sees one picture (two \begins before the last \end): not cacheable
        let pics = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(pics.len(), 1);
        assert!(!pics[0].cacheable);
        let texts = texts_of(
            "\\begin{document}\n\\foo{x}\\begin{tikzpicture}\nb\n\\end{tikzpicture}\n\\end{document}\n",
        );
        let pics = scan_pictures(&texts, "main.tex", 1);
        assert_eq!(pics.len(), 1);
        assert!(!pics[0].cacheable, "text before the \\begin on its line");
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
            end_line: 9,
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
        // not a real PDF: the whole file is copied and page numbers stay
        assert_eq!(cache.absorb(&pics, &rec, &[], &pdf).unwrap(), 1);
        assert_eq!(cache.entries(), 1);
        assert_eq!(cache.write_manifest(&pics, &manifest).unwrap(), 1);
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        let e = &m["main.tex:5"];
        assert_eq!(e["page"], 2);
        assert_eq!(e["end_line"], 9);
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
        assert_eq!(cache.absorb(&pics, &rec2, &[], &pdf).unwrap(), 1);
        assert_eq!(cache.entries(), 1);
        cache.write_manifest(&pics, &manifest).unwrap();
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(m["main.tex:5"]["state"], "other font/0 g 0 G");
        assert_eq!(m["main.tex:5"]["h"], 400);
        // a pass that reports the picture as mismatched forgets it
        assert_eq!(
            cache
                .absorb(&pics, &BTreeMap::new(), &["main.tex:5".into()], &pdf)
                .unwrap(),
            0
        );
        assert_eq!(cache.entries(), 0);
        // and draws it (no entry) for KEEP_PASSES passes before caching it again
        assert_eq!(cache.absorb(&pics, &rec, &[], &pdf).unwrap(), 0);
        for _ in 0..KEEP_PASSES {
            assert_eq!(cache.write_manifest(&pics, &manifest).unwrap(), 0);
            assert_eq!(cache.absorb(&pics, &rec, &[], &pdf).unwrap(), 0);
        }
        cache.write_manifest(&pics, &manifest).unwrap();
        assert_eq!(cache.absorb(&pics, &rec, &[], &pdf).unwrap(), 1);
        assert_eq!(cache.write_manifest(&pics, &manifest).unwrap(), 1);
        // a changed picture: no hit; after KEEP_PASSES unwanted passes the entry and its PDF go
        let changed = vec![PictureRef {
            hash: 43,
            ..pics[0].clone()
        }];
        for _ in 0..(KEEP_PASSES + 1) {
            assert_eq!(cache.write_manifest(&changed, &manifest).unwrap(), 0);
            cache.absorb(&changed, &BTreeMap::new(), &[], &pdf).unwrap();
        }
        assert_eq!(cache.entries(), 0);
        assert!(std::fs::read_dir(dir.join("cache"))
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".pdf")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
