//! Source buffers, paragraph segmentation with stable ids, and revision bookkeeping.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// Monotonic per-session revision counter (bumped by every edit on any file).
pub type Revision = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ParaId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanKind {
    Preamble,
    Body,
    Env,
    Heading,
    /// After `\end{document}`
    Trailer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub id: ParaId,
    pub range: Range<usize>,
    pub kind: SpanKind,
    pub hash: u64,
    /// Revision of the last edit that touched this span.
    pub last_revision: Revision,
}

#[derive(Debug, Clone)]
pub struct Edit {
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct FileBuf {
    pub text: String,
    pub line_starts: Vec<usize>,
    pub spans: Vec<Span>,
}

pub fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn compute_line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// Boundaries of paragraph-ish units in `text`, as byte ranges with kinds (ids not assigned).
fn segment(text: &str) -> Vec<(Range<usize>, SpanKind)> {
    let mut out: Vec<(Range<usize>, SpanKind)> = Vec::new();
    let begin_doc = text.find("\\begin{document}");
    let end_doc = text.find("\\end{document}");
    let body_start = match begin_doc {
        Some(i) => {
            let e = i + "\\begin{document}".len();
            // preamble includes the \begin{document} line
            let e = text[e..].find('\n').map(|k| e + k + 1).unwrap_or(text.len());
            out.push((0..e, SpanKind::Preamble));
            e
        }
        None => 0,
    };
    let body_end = end_doc.filter(|e| *e >= body_start).unwrap_or(text.len());
    let _ = segment_body(text, body_start, body_end, &mut out);
    if body_end < text.len() {
        out.push((body_end..text.len(), SpanKind::Trailer));
    }
    out
}

/// Segment `text[body_start..body_end]` (body material only) appending absolute ranges to `out`.
/// Returns false when the slice ends inside an unclosed environment (the caller must then
/// re-segment the whole file, since the environment swallows everything that follows).
fn segment_body(text: &str, body_start: usize, body_end: usize, out: &mut Vec<(Range<usize>, SpanKind)>) -> bool {
    let body = &text[body_start..body_end];
    let lines: Vec<(usize, &str)> = {
        let mut v = Vec::new();
        let mut p = 0;
        for l in body.split_inclusive('\n') {
            v.push((p, l));
            p += l.len();
        }
        v
    };
    let flush = |out: &mut Vec<(Range<usize>, SpanKind)>, s: usize, e: usize, k: SpanKind| {
        if e > s && !body[s..e].trim().is_empty() {
            out.push((body_start + s..body_start + e, k));
        }
    };
    let mut cur_start: Option<usize> = None;
    let mut env_stack: Vec<String> = Vec::new();
    let mut cur_kind = SpanKind::Body;
    for (lstart, line) in &lines {
        let trimmed = line.trim();
        let stripped = strip_comment(trimmed);
        let is_blank = stripped.trim().is_empty() && !trimmed.starts_with('%');
        // environment tracking (only at line starts, which covers the common layout)
        let begins: Vec<String> = if stripped.contains("\\begin{") { find_all_envs(stripped, "\\begin{") } else { Vec::new() };
        let ends: Vec<String> = if stripped.contains("\\end{") { find_all_envs(stripped, "\\end{") } else { Vec::new() };
        let heading = stripped.starts_with("\\chapter") || stripped.starts_with("\\section") || stripped.starts_with("\\subsection") || stripped.starts_with("\\subsubsection") || stripped.starts_with("\\part") || stripped.starts_with("\\paragraph") || stripped.starts_with("\\tableofcontents");
        if env_stack.is_empty() {
            if is_blank && trimmed.is_empty() {
                if let Some(s) = cur_start.take() {
                    flush(out, s, *lstart, cur_kind);
                }
                continue;
            }
            if cur_start.is_none() {
                cur_start = Some(*lstart);
                cur_kind = if heading { SpanKind::Heading } else { SpanKind::Body };
            } else if heading && cur_kind == SpanKind::Body {
                // a heading command starts a new unit even without a blank line
                flush(out, cur_start.unwrap(), *lstart, cur_kind);
                cur_start = Some(*lstart);
                cur_kind = SpanKind::Heading;
            }
            if !begins.is_empty() {
                cur_kind = SpanKind::Env;
            }
        }
        for b in begins {
            env_stack.push(b);
        }
        for e in ends {
            if let Some(idx) = env_stack.iter().rposition(|x| *x == e) {
                env_stack.truncate(idx);
            }
        }
    }
    if let Some(s) = cur_start {
        flush(out, s, body.len(), cur_kind);
    }
    env_stack.is_empty()
}

fn strip_comment(line: &str) -> &str {
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

fn find_all_envs(line: &str, prefix: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut idx = 0;
    while let Some(p) = line[idx..].find(prefix) {
        let s = idx + p + prefix.len();
        if let Some(e) = line[s..].find('}') {
            v.push(line[s..s + e].to_string());
            idx = s + e;
        } else {
            break;
        }
    }
    v
}

pub struct IdAllocator(pub u64);
impl IdAllocator {
    pub fn next(&mut self) -> ParaId {
        self.0 += 1;
        ParaId(self.0)
    }
}

/// Outcome of applying an edit to a file.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EditOutcome {
    pub touched: Vec<ParaId>,
    pub added: Vec<ParaId>,
    pub removed: Vec<ParaId>,
    pub preamble_changed: bool,
}

impl FileBuf {
    pub fn new(text: &str, ids: &mut IdAllocator, rev: Revision) -> FileBuf {
        let mut fb = FileBuf { text: text.to_string(), line_starts: compute_line_starts(text), spans: Vec::new() };
        fb.spans = segment(text).into_iter().map(|(range, kind)| {
            let hash = hash_str(&fb.text[range.clone()]);
            Span { id: ids.next(), range, kind, hash, last_revision: rev }
        }).collect();
        fb
    }

    pub fn line_of(&self, byte: usize) -> usize {
        match self.line_starts.binary_search(&byte) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }
    /// 1-based inclusive line range of a span.
    pub fn line_range(&self, span: &Span) -> (i64, i64) {
        let end = span.range.end.saturating_sub(1).max(span.range.start);
        (self.line_of(span.range.start) as i64, self.line_of(end) as i64)
    }
    pub fn span_at(&self, byte: usize) -> Option<&Span> {
        self.spans.iter().find(|s| s.range.start <= byte && byte <= s.range.end)
    }
    pub fn span(&self, id: ParaId) -> Option<&Span> {
        self.spans.iter().find(|s| s.id == id)
    }
    pub fn span_text(&self, id: ParaId) -> Option<&str> {
        self.span(id).map(|s| &self.text[s.range.clone()])
    }

    /// Apply a byte-range replacement, re-segment, and keep ids stable where the content did not
    /// change (before the edit by position, after it by content hash); the edited unit keeps its
    /// id when the edit maps one old unit to one new unit.
    pub fn apply(&mut self, edit: &Edit, ids: &mut IdAllocator, rev: Revision) -> EditOutcome {
        let start = edit.start_byte.min(self.text.len());
        let end = edit.end_byte.clamp(start, self.text.len());
        let old_spans = std::mem::take(&mut self.spans);
        let delta = edit.text.len() as i64 - (end - start) as i64;
        // in-place splice: one memmove of the tail instead of a full copy
        self.text.replace_range(start..end, &edit.text);
        // incremental line-start table: drop entries inside the replaced range, shift the rest,
        // insert the newlines of the inserted text
        let first_after = self.line_starts.partition_point(|&p| p <= start);
        let last_removed = self.line_starts.partition_point(|&p| p <= end);
        let mut inserted: Vec<usize> = edit.text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| start + i + 1).collect();
        let tail: Vec<usize> = self.line_starts[last_removed..].iter().map(|&p| (p as i64 + delta) as usize).collect();
        self.line_starts.truncate(first_after);
        self.line_starts.append(&mut inserted);
        self.line_starts.extend(tail);
        debug_assert_eq!(self.line_starts, compute_line_starts(&self.text));
        // Windowed re-segmentation: body spans are delimited by blank lines (environments are kept
        // whole), so re-segmenting from the start of the span before the edit to the end of the
        // span after it reproduces exactly what a full pass would produce there. Edits touching
        // the preamble, the trailer or no span at all fall back to a full pass.
        let touches = |sp: &Span| sp.range.start <= end && start <= sp.range.end;
        let first_touched = old_spans.iter().position(touches);
        let last_touched = old_spans.iter().rposition(touches);
        let window_ok = match (first_touched, last_touched) {
            (Some(a), Some(b)) => {
                let lo = a.saturating_sub(1);
                let hi = (b + 1).min(old_spans.len() - 1);
                old_spans[lo..=hi].iter().all(|sp| matches!(sp.kind, SpanKind::Body | SpanKind::Heading | SpanKind::Env))
                    && lo > 0 && hi + 1 < old_spans.len()
                    && !edit.text.contains("\\begin{document}") && !edit.text.contains("\\end{document}")
            }
            _ => false,
        };
        let new_units: Vec<(Range<usize>, SpanKind)> = if window_ok {
            let a = first_touched.unwrap().saturating_sub(1);
            let b = (last_touched.unwrap() + 1).min(old_spans.len() - 1);
            let win_start = old_spans[a].range.start;
            let win_end = (old_spans[b].range.end as i64 + delta) as usize;
            let mut units: Vec<(Range<usize>, SpanKind)> = old_spans[..a].iter().map(|sp| (sp.range.clone(), sp.kind)).collect();
            if segment_body(&self.text, win_start, win_end, &mut units) {
                for sp in &old_spans[b + 1..] {
                    units.push(((sp.range.start as i64 + delta) as usize..(sp.range.end as i64 + delta) as usize, sp.kind));
                }
                units
            } else {
                segment(&self.text)
            }
        } else {
            segment(&self.text)
        };
        // old spans entirely before the edit (and not touching it) are unchanged
        let mut prefix = 0;
        while prefix < old_spans.len() && prefix < new_units.len() && old_spans[prefix].range.end < start && old_spans[prefix].range == new_units[prefix].0 {
            prefix += 1;
        }
        // old spans entirely after the edit, matched from the end with the byte delta applied
        let mut suffix = 0;
        while suffix < old_spans.len() - prefix && suffix < new_units.len() - prefix {
            let o = &old_spans[old_spans.len() - 1 - suffix];
            let n = &new_units[new_units.len() - 1 - suffix];
            let shifted = (o.range.start as i64 + delta) as usize..(o.range.end as i64 + delta) as usize;
            if o.range.start > end && n.0 == shifted {
                suffix += 1;
            } else {
                break;
            }
        }
        let mut outcome = EditOutcome::default();
        let mut spans = Vec::with_capacity(new_units.len());
        for i in 0..prefix {
            spans.push(old_spans[i].clone());
        }
        let old_mid = &old_spans[prefix..old_spans.len() - suffix];
        let new_mid = &new_units[prefix..new_units.len() - suffix];
        if old_mid.len() == 1 && new_mid.len() == 1 {
            let o = &old_mid[0];
            let (range, kind) = new_mid[0].clone();
            let hash = hash_str(&self.text[range.clone()]);
            outcome.preamble_changed |= o.kind == SpanKind::Preamble || kind == SpanKind::Preamble;
            outcome.touched.push(o.id);
            spans.push(Span { id: o.id, range, kind, hash, last_revision: rev });
        } else {
            for o in old_mid {
                outcome.removed.push(o.id);
                outcome.preamble_changed |= o.kind == SpanKind::Preamble;
            }
            for (range, kind) in new_mid {
                let hash = hash_str(&self.text[range.clone()]);
                let id = ids.next();
                outcome.added.push(id);
                outcome.preamble_changed |= *kind == SpanKind::Preamble;
                spans.push(Span { id, range: range.clone(), kind: *kind, hash, last_revision: rev });
            }
        }
        for k in 0..suffix {
            let o = &old_spans[old_spans.len() - suffix + k];
            let (range, kind) = new_units[new_units.len() - suffix + k].clone();
            spans.push(Span { id: o.id, range, kind, hash: o.hash, last_revision: o.last_revision });
        }
        self.spans = spans;
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const DOC: &str = "\\documentclass{book}\n\\usepackage{microtype}\n\\begin{document}\n\\chapter{One}\n\nFirst paragraph\nspanning two lines.\n\nSecond paragraph.\n\n\\begin{itemize}\n\\item a\n\n\\item b\n\\end{itemize}\n\nThird.\n\\end{document}\n";

    #[test]
    fn segments_kinds() {
        let mut ids = IdAllocator(0);
        let fb = FileBuf::new(DOC, &mut ids, 1);
        let kinds: Vec<SpanKind> = fb.spans.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![SpanKind::Preamble, SpanKind::Heading, SpanKind::Body, SpanKind::Body, SpanKind::Env, SpanKind::Body, SpanKind::Trailer]);
        assert_eq!(fb.span_text(fb.spans[2].id).unwrap(), "First paragraph\nspanning two lines.\n");
        assert_eq!(fb.line_range(&fb.spans[2]), (6, 7));
    }

    #[test]
    fn edit_inside_paragraph_keeps_ids() {
        let mut ids = IdAllocator(0);
        let mut fb = FileBuf::new(DOC, &mut ids, 1);
        let before: Vec<ParaId> = fb.spans.iter().map(|s| s.id).collect();
        let pos = fb.text.find("spanning").unwrap();
        let out = fb.apply(&Edit { start_byte: pos, end_byte: pos, text: "now ".into() }, &mut ids, 2);
        let after: Vec<ParaId> = fb.spans.iter().map(|s| s.id).collect();
        assert_eq!(before, after);
        assert_eq!(out.touched, vec![before[2]]);
        assert!(out.added.is_empty() && out.removed.is_empty() && !out.preamble_changed);
        assert_eq!(fb.spans[2].last_revision, 2);
        assert_eq!(fb.spans[3].last_revision, 1);
        assert!(fb.span_text(before[2]).unwrap().contains("now spanning"));
    }

    #[test]
    fn split_and_merge() {
        let mut ids = IdAllocator(0);
        let mut fb = FileBuf::new(DOC, &mut ids, 1);
        let n = fb.spans.len();
        let pos = fb.text.find("spanning").unwrap();
        let out = fb.apply(&Edit { start_byte: pos, end_byte: pos, text: "\n\n".into() }, &mut ids, 2);
        assert_eq!(fb.spans.len(), n + 1);
        assert_eq!(out.removed.len(), 1);
        assert_eq!(out.added.len(), 2);
        // merge back by deleting the inserted blank line
        let out2 = fb.apply(&Edit { start_byte: pos, end_byte: pos + 2, text: String::new() }, &mut ids, 3);
        assert_eq!(fb.spans.len(), n);
        assert_eq!(out2.removed.len(), 2);
        assert_eq!(out2.added.len(), 1);
    }

    #[test]
    fn windowed_resegmentation_matches_full_pass() {
        let mut body = String::new();
        for i in 0..3000 {
            if i % 7 == 0 {
                body.push_str(&format!("\\section{{S{i}}}\n\n"));
            }
            if i % 11 == 0 {
                body.push_str("\\begin{itemize}\n\\item a\n\n\\item b\n\\end{itemize}\n\n");
            }
            body.push_str(&format!("Paragraph {i} with some words\nand a second line.\n\n"));
        }
        let doc = format!("\\documentclass{{book}}\n\\begin{{document}}\n{body}\\end{{document}}\n");
        let mut ids = IdAllocator(0);
        let mut fb = FileBuf::new(&doc, &mut ids, 1);
        let n = fb.spans.len();
        // many kinds of edits: insert, delete across a boundary, split, merge, inside env
        let probes = [" x", "\n\n", "", "\\emph{y}"];
        let mut t_total = std::time::Duration::ZERO;
        for k in 0..200 {
            let pos = (k * 7919 + 1234) % (fb.text.len() - 40) + 30;
            let text = probes[k % probes.len()].to_string();
            let del = if k % 5 == 0 { 3 } else { 0 };
            let t0 = std::time::Instant::now();
            fb.apply(&Edit { start_byte: pos, end_byte: pos + del, text }, &mut ids, 2 + k as u64);
            t_total += t0.elapsed();
            assert_eq!(fb.line_starts, compute_line_starts(&fb.text), "line starts after edit {k}");
            let full = segment(&fb.text);
            let got: Vec<(Range<usize>, SpanKind)> = fb.spans.iter().map(|s| (s.range.clone(), s.kind)).collect();
            if got != full {
                let i = got.iter().zip(full.iter()).position(|(a, b)| a != b).unwrap_or(got.len().min(full.len()));
                let lo = i.saturating_sub(2);
                panic!("edit {k} at {pos} (del {del}, text {:?}): first mismatch at span {i}\n got: {:?}\nfull: {:?}\ntext around: {:?}",
                    probes[k % probes.len()], &got[lo..(i + 3).min(got.len())], &full[lo..(i + 3).min(full.len())],
                    &fb.text[pos.saturating_sub(60)..(pos + 60).min(fb.text.len())]);
            }
        }
        eprintln!("200 edits on a {}-byte / {}-span document: {:?} per edit", fb.text.len(), n, t_total / 200);
    }

    #[test]
    fn preamble_edit_flagged() {
        let mut ids = IdAllocator(0);
        let mut fb = FileBuf::new(DOC, &mut ids, 1);
        let pos = fb.text.find("microtype").unwrap();
        let out = fb.apply(&Edit { start_byte: pos, end_byte: pos + 9, text: "xcolor".into() }, &mut ids, 2);
        assert!(out.preamble_changed);
    }
}
