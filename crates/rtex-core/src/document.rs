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
    /// Extra block environments (theorem-like) that end a span like the built-in ones.
    pub extra_block_envs: Vec<String>,
}

/// Block environments after whose `\end` a span ends (the capture closes the unit there); the
/// session adds theorem-like environments from the preamble through `FileBuf::extra_block_envs`.
pub const BLOCK_ENVS: &[&str] = &[
    "itemize",
    "enumerate",
    "description",
    "quote",
    "quotation",
    "verse",
    "center",
    "flushleft",
    "flushright",
    "abstract",
    "figure",
    "figure*",
    "table",
    "table*",
    "tabbing",
    // amsthm, verbatim material, setspace
    "proof",
    "verbatim",
    "verbatim*",
    "lstlisting",
    "Verbatim",
    "alltt",
    "spacing",
    // manual bibliography: its \section* heading and \bibitem list are one unit
    "thebibliography",
];
const HEADING_CMDS: &[&str] = &[
    "\\chapter",
    "\\section",
    "\\subsection",
    "\\subsubsection",
    "\\part",
    "\\paragraph",
    "\\subparagraph",
    "\\tableofcontents",
];

fn brace_balance(line: &str) -> i32 {
    let b = line.as_bytes();
    let mut bal = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'{' => bal += 1,
            b'}' => bal -= 1,
            _ => {}
        }
        i += 1;
    }
    bal
}

pub fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// `\input{…}`, `\include{…}` and `\subfile{…}` commands in `text` (outside comments), as
/// (byte range of the command, project-relative target with a `.tex` extension). Absolute
/// paths, `..`, and names built from macros are skipped.
fn find_inputs(text: &str) -> Vec<(Range<usize>, String, &'static str)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                i += text[i..].find('\n').map(|k| k + 1).unwrap_or(b.len() - i);
            }
            b'\\' => {
                let start = i;
                i += 1;
                let name_end = i + text[i..]
                    .bytes()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .count();
                if name_end == i {
                    // escaped character (\%, \\ …): skip it so the next byte is not a comment start
                    i = (i + 1).min(b.len());
                    continue;
                }
                let name = &text[i..name_end];
                i = name_end;
                if !matches!(name, "input" | "include" | "subfile") {
                    continue;
                }
                let mut j = name_end;
                while j < b.len() && b[j] == b' ' {
                    j += 1;
                }
                let (arg, end) = if j < b.len() && b[j] == b'{' {
                    let Some(close) = text[j..].find('}') else {
                        break;
                    };
                    (&text[j + 1..j + close], j + close + 1)
                } else if name == "input" && j < b.len() {
                    // plain TeX form: \input file
                    let k = j + text[j..]
                        .bytes()
                        .take_while(|c| {
                            !c.is_ascii_whitespace() && *c != b'\\' && *c != b'{' && *c != b'}'
                        })
                        .count();
                    (&text[j..k], k)
                } else {
                    continue;
                };
                i = end.max(i);
                if let Some(t) = normalize_input_target(arg) {
                    let kind = match name {
                        "input" => "input",
                        "include" => "include",
                        _ => "subfile",
                    };
                    out.push((start..end, t, kind));
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn normalize_input_target(arg: &str) -> Option<String> {
    let t = arg.trim().trim_matches('"');
    let t = t.strip_prefix("./").unwrap_or(t);
    if t.is_empty()
        || t.starts_with('/')
        || t.contains("..")
        || t.contains('\\')
        || t.contains('#')
        || t.contains(',')
    {
        return None;
    }
    let last = t.rsplit('/').next().unwrap_or(t);
    Some(if last.contains('.') {
        t.to_string()
    } else {
        format!("{t}.tex")
    })
}

/// Files `text` pulls in with `\input`/`\include`/`\subfile`, normalised to project-relative
/// paths with a `.tex` extension, in order of appearance.
pub fn input_targets(text: &str) -> Vec<String> {
    find_inputs(text).into_iter().map(|(_, t, _)| t).collect()
}

/// Files read with `\input` (not `\include`) by any of `texts`: LaTeX reads the rest of the
/// `\input` line after the file, so a paragraph that ends at such a file's end keeps one
/// interword space before `\parfillskip` (see `fast_source`).
pub fn inputted_files(
    texts: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeSet<String> {
    texts
        .values()
        .flat_map(|t| find_inputs(t))
        .filter(|(_, _, kind)| *kind == "input" || *kind == "subfile")
        .map(|(_, t, _)| t)
        .collect()
}

/// The source the fast path typesets for a span: its text without the trailing newline, plus
/// `\n{}` when the span ends a file read with `\input` without a final blank line. At that
/// point TeX produces a second end-of-line space (the parent's `\input` line), and `\par`
/// removes only the last glue, so the paragraph keeps one trailing space; `{}` on a new line
/// reproduces that space token exactly (same space factor), and nothing else.
pub fn fast_source(fb: &FileBuf, span: &Span, file_is_inputted: bool) -> String {
    let text = fb.text[span.range.clone()].trim_end_matches('\n');
    let mut src = text.to_string();
    if file_is_inputted && span.kind != SpanKind::Trailer && !text.trim().is_empty() {
        let rest = &fb.text[span.range.end..];
        let own = &fb.text[span.range.clone()];
        let own_tail = &own[own.trim_end().len()..];
        let ends_file = rest.trim().is_empty()
            && own_tail.matches('\n').count() + rest.matches('\n').count() <= 1;
        if ends_file {
            src.push_str("\n{}");
        }
    }
    src
}

/// `main` plus, transitively, every file it inputs that exists under `root` (missing files are
/// left to the compiler to report), keyed by project-relative path.
pub fn load_project_files(
    root: &std::path::Path,
    main: &str,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    use anyhow::Context;
    let mut files = std::collections::BTreeMap::new();
    let main_text =
        std::fs::read_to_string(root.join(main)).with_context(|| format!("reading {main}"))?;
    let mut queue = input_targets(&main_text);
    files.insert(main.to_string(), main_text);
    while let Some(rel) = queue.pop() {
        if files.contains_key(&rel) || files.len() > 256 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        queue.extend(input_targets(&text));
        files.insert(rel, text);
    }
    Ok(files)
}

/// Files reachable from `text` through `\input`/`\include` chains, restricted to `files`.
pub fn transitive_inputs(
    text: &str,
    files: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeSet<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = input_targets(text);
    while let Some(rel) = queue.pop() {
        if !files.contains_key(&rel) || !seen.insert(rel.clone()) {
            continue;
        }
        queue.extend(input_targets(&files[&rel]));
    }
    seen
}

/// `text` with every `\input{f}`/`\include{f}` whose target is in `files` replaced by that
/// file's text (recursively, bounded depth): the preamble the fast server loads, so it sees the
/// host's buffers of preamble files rather than what is on disk.
pub fn expand_inputs(text: &str, files: &std::collections::BTreeMap<String, String>) -> String {
    fn go(text: &str, files: &std::collections::BTreeMap<String, String>, depth: u32) -> String {
        if depth > 8 {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (range, target, _) in find_inputs(text) {
            if let Some(t) = files.get(&target) {
                out.push_str(&text[last..range.start]);
                out.push_str(&go(t, files, depth + 1));
                out.push('\n');
                last = range.end;
            }
        }
        out.push_str(&text[last..]);
        out
    }
    go(text, files, 0)
}

/// Setup statements found in the document body (see `eligibility::setup_statements`), in
/// document order: the main file's body, then the other files. Chunks are separated by blank
/// lines, like the segmenter's paragraph spans.
pub fn body_setup_statements(
    texts: &std::collections::BTreeMap<String, String>,
    main: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut scan = |text: &str| {
        for chunk in text.split("\n\n") {
            let stripped = crate::eligibility::strip_comments(chunk);
            if let Some(st) = crate::eligibility::setup_statements(&stripped) {
                out.push(st);
            }
        }
    };
    if let Some(t) = texts.get(main) {
        let body = crate::split_preamble(t).map(|(_, b)| b).unwrap_or("");
        let body = body
            .strip_prefix("\\begin{document}")
            .unwrap_or(body)
            .split("\\end{document}")
            .next()
            .unwrap_or("");
        scan(body);
    }
    for (name, t) in texts {
        if name != main {
            scan(t);
        }
    }
    out
}

/// The file buffers of a project (offline tools): spans are found across files by id.
#[derive(Default)]
pub struct FileSet {
    pub files: std::collections::BTreeMap<String, FileBuf>,
    /// Files read with `\input` (see `fast_source`).
    pub inputted: std::collections::BTreeSet<String>,
}

impl FileSet {
    pub fn span_text(&self, id: ParaId) -> Option<&str> {
        self.files.values().find_map(|fb| fb.span_text(id))
    }
    /// What the fast path compiles for span `id` (see `fast_source`).
    pub fn fast_source(&self, id: ParaId) -> Option<String> {
        self.files.iter().find_map(|(name, fb)| {
            fb.span(id)
                .map(|sp| fast_source(fb, sp, self.inputted.contains(name)))
        })
    }
    pub fn get(&self, rel: &str) -> Option<&FileBuf> {
        self.files.get(rel)
    }
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
fn segment(text: &str, extra_block_envs: &[String]) -> Vec<(Range<usize>, SpanKind)> {
    let mut out: Vec<(Range<usize>, SpanKind)> = Vec::new();
    let begin_doc = text.find("\\begin{document}");
    let end_doc = text.find("\\end{document}");
    let body_start = match begin_doc {
        Some(i) => {
            let e = i + "\\begin{document}".len();
            // preamble includes the \begin{document} line
            let e = text[e..]
                .find('\n')
                .map(|k| e + k + 1)
                .unwrap_or(text.len());
            out.push((0..e, SpanKind::Preamble));
            e
        }
        None => 0,
    };
    let body_end = end_doc.filter(|e| *e >= body_start).unwrap_or(text.len());
    let _ = segment_body(text, body_start, body_end, &mut out, extra_block_envs);
    if body_end < text.len() {
        out.push((body_end..text.len(), SpanKind::Trailer));
    }
    out
}

/// Segment `text[body_start..body_end]` (body material only) appending absolute ranges to `out`.
/// Returns false when the slice ends inside an unclosed environment (the caller must then
/// re-segment the whole file, since the environment swallows everything that follows).
fn segment_body(
    text: &str,
    body_start: usize,
    body_end: usize,
    out: &mut Vec<(Range<usize>, SpanKind)>,
    extra_block_envs: &[String],
) -> bool {
    let is_block =
        |name: &str| BLOCK_ENVS.contains(&name) || extra_block_envs.iter().any(|e| e == name);
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
    // the current span ends before the next non-blank line: after a heading's braces closed or
    // after a block environment closed (the capture closes the unit at those points)
    let mut split_pending = false;
    let mut heading_balance: i32 = 0;
    for (lstart, line) in &lines {
        let trimmed = line.trim();
        let stripped = strip_comment(trimmed);
        let is_blank = stripped.trim().is_empty() && !trimmed.starts_with('%');
        // environment tracking (only at line starts, which covers the common layout)
        let begins: Vec<String> = if stripped.contains("\\begin{") {
            find_all_envs(stripped, "\\begin{")
        } else {
            Vec::new()
        };
        let ends: Vec<String> = if stripped.contains("\\end{") {
            find_all_envs(stripped, "\\end{")
        } else {
            Vec::new()
        };
        let heading = HEADING_CMDS.iter().any(|h| stripped.starts_with(h));
        if env_stack.is_empty() {
            if is_blank && trimmed.is_empty() {
                if let Some(s) = cur_start.take() {
                    flush(out, s, *lstart, cur_kind);
                }
                split_pending = false;
                continue;
            }
            if cur_start.is_none() {
                cur_start = Some(*lstart);
                cur_kind = if heading {
                    SpanKind::Heading
                } else {
                    SpanKind::Body
                };
                heading_balance = 0;
                split_pending = false;
            } else if (heading && cur_kind == SpanKind::Body) || split_pending {
                // a heading command starts a new unit even without a blank line, and a unit ends
                // after a heading or a block environment
                flush(out, cur_start.unwrap(), *lstart, cur_kind);
                cur_start = Some(*lstart);
                cur_kind = if heading {
                    SpanKind::Heading
                } else {
                    SpanKind::Body
                };
                heading_balance = 0;
                split_pending = false;
            }
            if !begins.is_empty() {
                cur_kind = SpanKind::Env;
            }
            if cur_kind == SpanKind::Heading {
                heading_balance += brace_balance(stripped);
                if heading_balance <= 0 && begins.is_empty() {
                    split_pending = true;
                }
            }
        }
        for b in begins {
            env_stack.push(b);
        }
        for e in ends {
            if let Some(idx) = env_stack.iter().rposition(|x| *x == e) {
                env_stack.truncate(idx);
                if env_stack.is_empty() && is_block(&e) {
                    split_pending = true;
                }
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
        Self::with_block_envs(text, ids, rev, Vec::new())
    }

    pub fn with_block_envs(
        text: &str,
        ids: &mut IdAllocator,
        rev: Revision,
        extra_block_envs: Vec<String>,
    ) -> FileBuf {
        let mut fb = FileBuf {
            text: text.to_string(),
            line_starts: compute_line_starts(text),
            spans: Vec::new(),
            extra_block_envs,
        };
        fb.resegment(ids, rev);
        fb
    }

    /// Re-segment the whole buffer (new ids for every span).
    pub fn resegment(&mut self, ids: &mut IdAllocator, rev: Revision) {
        let units = segment(&self.text, &self.extra_block_envs);
        self.spans = units
            .into_iter()
            .map(|(range, kind)| {
                let hash = hash_str(&self.text[range.clone()]);
                Span {
                    id: ids.next(),
                    range,
                    kind,
                    hash,
                    last_revision: rev,
                }
            })
            .collect();
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
        (
            self.line_of(span.range.start) as i64,
            self.line_of(end) as i64,
        )
    }
    pub fn span_at(&self, byte: usize) -> Option<&Span> {
        self.spans
            .iter()
            .find(|s| s.range.start <= byte && byte <= s.range.end)
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
        let mut inserted: Vec<usize> = edit
            .text
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| start + i + 1)
            .collect();
        let tail: Vec<usize> = self.line_starts[last_removed..]
            .iter()
            .map(|&p| (p as i64 + delta) as usize)
            .collect();
        self.line_starts.truncate(first_after);
        self.line_starts.append(&mut inserted);
        self.line_starts.extend(tail);
        debug_assert_eq!(self.line_starts, compute_line_starts(&self.text));
        // Windowed re-segmentation: body spans are delimited by blank lines (environments are kept
        // whole), so re-segmenting from the start of the span before the edit to the end of the
        // span after it reproduces exactly what a full pass would produce there. Edits touching
        // the preamble, the trailer or no span at all fall back to a full pass.
        // spans touched by the edit, or (for an edit in a gap between spans) its neighbours
        let touches = |sp: &Span| sp.range.start <= end && start <= sp.range.end;
        let first_touched = old_spans
            .iter()
            .position(touches)
            .or_else(|| old_spans.iter().rposition(|sp| sp.range.end <= start));
        let last_touched = old_spans
            .iter()
            .rposition(touches)
            .or_else(|| old_spans.iter().position(|sp| sp.range.start >= end));
        // The window is bounded by the neighbouring spans; when a neighbour is the preamble or
        // the trailer the window starts (ends) at the body boundary instead, which is fixed.
        let window: Option<(usize, usize)> = match (first_touched, last_touched) {
            (Some(a), Some(b)) if a <= b && a > 0 && b + 1 < old_spans.len() => {
                let (lo, hi) = (a - 1, b + 1);
                let lo_ok = matches!(
                    old_spans[lo].kind,
                    SpanKind::Body | SpanKind::Heading | SpanKind::Env | SpanKind::Preamble
                );
                let hi_ok = matches!(
                    old_spans[hi].kind,
                    SpanKind::Body | SpanKind::Heading | SpanKind::Env | SpanKind::Trailer
                );
                let mid_ok = old_spans[a..=b].iter().all(|sp| {
                    matches!(sp.kind, SpanKind::Body | SpanKind::Heading | SpanKind::Env)
                });
                if lo_ok
                    && hi_ok
                    && mid_ok
                    && !edit.text.contains("\\begin{document}")
                    && !edit.text.contains("\\end{document}")
                {
                    Some((lo, hi))
                } else {
                    None
                }
            }
            _ => None,
        };
        let new_units: Vec<(Range<usize>, SpanKind)> = if let Some((lo, hi)) = window {
            let lo_is_preamble = old_spans[lo].kind == SpanKind::Preamble;
            let hi_is_trailer = old_spans[hi].kind == SpanKind::Trailer;
            let win_start = if lo_is_preamble {
                old_spans[lo].range.end
            } else {
                old_spans[lo].range.start
            };
            let win_end = (if hi_is_trailer {
                old_spans[hi].range.start
            } else {
                old_spans[hi].range.end
            } as i64
                + delta) as usize;
            let keep_prefix = if lo_is_preamble { lo + 1 } else { lo };
            let mut units: Vec<(Range<usize>, SpanKind)> = old_spans[..keep_prefix]
                .iter()
                .map(|sp| (sp.range.clone(), sp.kind))
                .collect();
            if segment_body(
                &self.text,
                win_start,
                win_end,
                &mut units,
                &self.extra_block_envs,
            ) {
                let tail_from = if hi_is_trailer { hi } else { hi + 1 };
                for sp in &old_spans[tail_from..] {
                    units.push((
                        (sp.range.start as i64 + delta) as usize
                            ..(sp.range.end as i64 + delta) as usize,
                        sp.kind,
                    ));
                }
                units
            } else {
                segment(&self.text, &self.extra_block_envs)
            }
        } else {
            segment(&self.text, &self.extra_block_envs)
        };
        // old spans before the edit whose range came out unchanged are unchanged (a span that
        // merely borders the edit, like the preamble when typing at the top of the first
        // paragraph, is unchanged too when its range is)
        let mut prefix = 0;
        while prefix < old_spans.len()
            && prefix < new_units.len()
            && old_spans[prefix].range.end <= start
            && old_spans[prefix].range == new_units[prefix].0
        {
            prefix += 1;
        }
        // old spans entirely after the edit, matched from the end with the byte delta applied
        let mut suffix = 0;
        while suffix < old_spans.len() - prefix && suffix < new_units.len() - prefix {
            let o = &old_spans[old_spans.len() - 1 - suffix];
            let n = &new_units[new_units.len() - 1 - suffix];
            let shifted =
                (o.range.start as i64 + delta) as usize..(o.range.end as i64 + delta) as usize;
            if o.range.start >= end && n.0 == shifted {
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
            spans.push(Span {
                id: o.id,
                range,
                kind,
                hash,
                last_revision: rev,
            });
        } else {
            // A boundary change (split, merge, a paragraph typed fresh). The first new span keeps
            // the id of the first old span when it starts at the same byte with the same kind:
            // its engine context and its first-row placement stay valid (same predecessor, same
            // start). Every other new span gets a fresh id and borrows a context (session).
            let reused = match (old_mid.first(), new_mid.first()) {
                (Some(o), Some((range, kind)))
                    if o.range.start == range.start
                        && o.kind == *kind
                        && matches!(kind, SpanKind::Body | SpanKind::Heading | SpanKind::Env) =>
                {
                    Some(o.id)
                }
                _ => None,
            };
            for o in old_mid {
                if Some(o.id) == reused {
                    continue;
                }
                outcome.removed.push(o.id);
                outcome.preamble_changed |= o.kind == SpanKind::Preamble;
            }
            for (k, (range, kind)) in new_mid.iter().enumerate() {
                let hash = hash_str(&self.text[range.clone()]);
                let id = match (k, reused) {
                    (0, Some(id)) => {
                        outcome.touched.push(id);
                        id
                    }
                    _ => {
                        let id = ids.next();
                        outcome.added.push(id);
                        id
                    }
                };
                outcome.preamble_changed |= *kind == SpanKind::Preamble;
                spans.push(Span {
                    id,
                    range: range.clone(),
                    kind: *kind,
                    hash,
                    last_revision: rev,
                });
            }
        }
        for k in 0..suffix {
            let o = &old_spans[old_spans.len() - suffix + k];
            let (range, kind) = new_units[new_units.len() - suffix + k].clone();
            spans.push(Span {
                id: o.id,
                range,
                kind,
                hash: o.hash,
                last_revision: o.last_revision,
            });
        }
        self.spans = spans;
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_scanning_and_expansion() {
        let text = "\\input{chapters/one}\n% \\input{nope}\n\\include{chapters/two.tex}\n\\input three\n\\input{../x}\n\\input{\\jobname}\n";
        assert_eq!(
            input_targets(text),
            vec!["chapters/one.tex", "chapters/two.tex", "three.tex"]
        );
        let mut files = std::collections::BTreeMap::new();
        files.insert("a.tex".to_string(), "A\\input{b}".to_string());
        files.insert("b.tex".to_string(), "B".to_string());
        assert_eq!(
            expand_inputs("x \\input{a} y \\input{c}", &files),
            "x AB\n\n y \\input{c}"
        );
        assert_eq!(
            transitive_inputs("\\input{a}", &files)
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["a.tex", "b.tex"]
        );
        let mut ids = IdAllocator(0);
        let fb = FileBuf::new("First.\n\nLast one.\n", &mut ids, 1);
        let last = fb.spans.last().unwrap();
        assert_eq!(fast_source(&fb, last, false), "Last one.");
        assert_eq!(fast_source(&fb, last, true), "Last one.\n{}");
        assert_eq!(fast_source(&fb, &fb.spans[0], true), "First.");
        let fb2 = FileBuf::new("Only.\n\n", &mut ids, 1);
        assert_eq!(fast_source(&fb2, &fb2.spans[0], true), "Only.");
    }
    const DOC: &str = "\\documentclass{book}\n\\usepackage{microtype}\n\\begin{document}\n\\chapter{One}\n\nFirst paragraph\nspanning two lines.\n\nSecond paragraph.\n\n\\begin{itemize}\n\\item a\n\n\\item b\n\\end{itemize}\n\nThird.\n\\end{document}\n";

    #[test]
    fn segments_kinds() {
        let mut ids = IdAllocator(0);
        let fb = FileBuf::new(DOC, &mut ids, 1);
        let kinds: Vec<SpanKind> = fb.spans.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SpanKind::Preamble,
                SpanKind::Heading,
                SpanKind::Body,
                SpanKind::Body,
                SpanKind::Env,
                SpanKind::Body,
                SpanKind::Trailer
            ]
        );
        assert_eq!(
            fb.span_text(fb.spans[2].id).unwrap(),
            "First paragraph\nspanning two lines.\n"
        );
        assert_eq!(fb.line_range(&fb.spans[2]), (6, 7));
    }

    #[test]
    fn edit_inside_paragraph_keeps_ids() {
        let mut ids = IdAllocator(0);
        let mut fb = FileBuf::new(DOC, &mut ids, 1);
        let before: Vec<ParaId> = fb.spans.iter().map(|s| s.id).collect();
        let pos = fb.text.find("spanning").unwrap();
        let out = fb.apply(
            &Edit {
                start_byte: pos,
                end_byte: pos,
                text: "now ".into(),
            },
            &mut ids,
            2,
        );
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
        let before: Vec<ParaId> = fb.spans.iter().map(|s| s.id).collect();
        let out = fb.apply(
            &Edit {
                start_byte: pos,
                end_byte: pos,
                text: "\n\n".into(),
            },
            &mut ids,
            2,
        );
        assert_eq!(fb.spans.len(), n + 1);
        // the first half keeps the paragraph's id, the second half is new
        assert_eq!(out.touched, vec![before[2]]);
        assert!(out.removed.is_empty());
        assert_eq!(out.added.len(), 1);
        assert_eq!(fb.spans[2].id, before[2]);
        assert_eq!(fb.spans[3].id, out.added[0]);
        // merge back by deleting the inserted blank line: the first id survives, the second goes
        let out2 = fb.apply(
            &Edit {
                start_byte: pos,
                end_byte: pos + 2,
                text: String::new(),
            },
            &mut ids,
            3,
        );
        assert_eq!(fb.spans.len(), n);
        assert_eq!(out2.touched, vec![before[2]]);
        assert_eq!(out2.removed, vec![out.added[0]]);
        assert!(out2.added.is_empty());
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
            body.push_str(&format!(
                "Paragraph {i} with some words\nand a second line.\n\n"
            ));
        }
        let doc =
            format!("\\documentclass{{book}}\n\\begin{{document}}\n{body}\\end{{document}}\n");
        let mut ids = IdAllocator(0);
        let mut fb = FileBuf::new(&doc, &mut ids, 1);
        let n = fb.spans.len();
        // many kinds of edits: insert, delete across a boundary, split, merge, inside env
        let probes = [" x", "\n\n", "", "\\emph{y}"];
        let mut t_total = std::time::Duration::ZERO;
        let mut times: Vec<std::time::Duration> = Vec::new();
        // probe positions inside paragraph text (never inside \begin/\end lines, which would turn
        // the rest of the document into one unclosed environment and dominate the timing)
        let anchors: Vec<usize> = fb
            .text
            .match_indices("with some words")
            .map(|(i, _)| i + 5)
            .collect();
        for k in 0..200 {
            let pos = anchors[(k * 7919 + 13) % anchors.len()] + (k % 3);
            let text = probes[k % probes.len()].to_string();
            let del = if k % 5 == 0 { 3 } else { 0 };
            let t0 = std::time::Instant::now();
            fb.apply(
                &Edit {
                    start_byte: pos,
                    end_byte: pos + del,
                    text,
                },
                &mut ids,
                2 + k as u64,
            );
            let dt = t0.elapsed();
            t_total += dt;
            times.push(dt);
            assert_eq!(
                fb.line_starts,
                compute_line_starts(&fb.text),
                "line starts after edit {k}"
            );
            let full = segment(&fb.text, &fb.extra_block_envs);
            let got: Vec<(Range<usize>, SpanKind)> =
                fb.spans.iter().map(|s| (s.range.clone(), s.kind)).collect();
            if got != full {
                let i = got
                    .iter()
                    .zip(full.iter())
                    .position(|(a, b)| a != b)
                    .unwrap_or(got.len().min(full.len()));
                let lo = i.saturating_sub(2);
                panic!("edit {k} at {pos} (del {del}, text {:?}): first mismatch at span {i}\n got: {:?}\nfull: {:?}\ntext around: {:?}",
                    probes[k % probes.len()], &got[lo..(i + 3).min(got.len())], &full[lo..(i + 3).min(full.len())],
                    &fb.text[pos.saturating_sub(60)..(pos + 60).min(fb.text.len())]);
            }
        }
        times.sort();
        eprintln!("200 edits on a {}-byte / {}-span document: median {:?}, mean {:?} per edit (the mean includes edits after a probe broke an \\end{{itemize}}, which makes the rest of the document one span)", fb.text.len(), n, times[100], t_total / 200);
        // the timing bound is a release-build property (debug builds are ~10× slower and also run
        // the debug_assert that recomputes every line start); correctness is checked above either way
        if !cfg!(debug_assertions) {
            assert!(
                times[100] < std::time::Duration::from_micros(250),
                "median per-edit cost {:?}",
                times[100]
            );
        }
    }

    #[test]
    fn heading_and_environment_splits() {
        let doc = "\\documentclass{book}\n\\begin{document}\n\\section{Title\nover two lines}\nFirst paragraph after the heading.\nSecond line.\n\nIntro:\n\\begin{itemize}\n\\item a\n\\end{itemize}\nText after the list.\n\nMath \\begin{equation} x \\end{equation} stays\ntogether.\n\\end{document}\n";
        let mut ids = IdAllocator(0);
        let fb = FileBuf::new(doc, &mut ids, 1);
        let texts: Vec<(&str, SpanKind)> = fb
            .spans
            .iter()
            .map(|s| (fb.text[s.range.clone()].trim_end(), s.kind))
            .collect();
        assert_eq!(
            texts[1],
            ("\\section{Title\nover two lines}", SpanKind::Heading)
        );
        assert_eq!(
            texts[2],
            (
                "First paragraph after the heading.\nSecond line.",
                SpanKind::Body
            )
        );
        assert_eq!(
            texts[3],
            (
                "Intro:\n\\begin{itemize}\n\\item a\n\\end{itemize}",
                SpanKind::Env
            )
        );
        assert_eq!(texts[4], ("Text after the list.", SpanKind::Body));
        assert_eq!(
            texts[5],
            (
                "Math \\begin{equation} x \\end{equation} stays\ntogether.",
                SpanKind::Env
            )
        );
        assert_eq!(fb.spans.len(), 7);
    }

    #[test]
    fn preamble_edit_flagged() {
        let mut ids = IdAllocator(0);
        let mut fb = FileBuf::new(DOC, &mut ids, 1);
        let pos = fb.text.find("microtype").unwrap();
        let out = fb.apply(
            &Edit {
                start_byte: pos,
                end_byte: pos + 9,
                text: "xcolor".into(),
            },
            &mut ids,
            2,
        );
        assert!(out.preamble_changed);
    }
}
