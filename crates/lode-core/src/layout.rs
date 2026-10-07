//! Versioned layout store: engine units (contexts + row placements) from the latest background
//! pass, their mapping to host spans, page display lists, labels, and fragment construction.

use crate::capture::{CapturedParagraph, CapturedUnit, CaptureResult, Placement};
use crate::document::{ParaId, Revision};
use lode_dl::{DisplayList, Sp};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct Fragment {
    pub page: i64,
    /// 1-based inclusive range of the unit's rows on this page.
    pub first_line: i64,
    pub last_line: i64,
    /// x of the first row of the fragment (page coordinates).
    pub x: Sp,
    /// Per-row x positions (page coordinates), parallel to `baselines`.
    pub xs: Vec<Sp>,
    pub baselines: Vec<Sp>,
    /// Rows beyond the cached placements were extrapolated from the fast box's own geometry.
    pub approximate: bool,
}

/// What the host recorded about a span when a pass was started.
#[derive(Debug, Clone)]
pub struct SnapshotSpan {
    pub id: ParaId,
    pub file: String,
    pub first_line: i64,
    pub last_line: i64,
    pub last_revision: Revision,
    pub background_only: bool,
}

/// A capture unit with the derived data the session needs.
#[derive(Debug, Clone)]
pub struct EngineUnit {
    pub uid: i64,
    pub captured: CapturedUnit,
    /// The unit's first paragraph (source of the line-breaking parameters for "par" units).
    pub first_para: Option<CapturedParagraph>,
    /// Node flags merged over the unit's paragraphs.
    pub flags: BTreeMap<String, i64>,
    /// (groupcode, nest) of every member paragraph.
    pub members: Vec<(String, i64)>,
    pub span: Option<ParaId>,
}

impl EngineUnit {
    pub fn kind(&self) -> &str {
        &self.captured.kind
    }
    pub fn name(&self) -> Option<&str> {
        self.captured.name.as_deref()
    }
    pub fn rows(&self) -> i64 {
        self.captured.placements.len() as i64
    }
    pub fn baselineskip(&self) -> Sp {
        let g = self.first_para.as_ref().map(|p| &p.glues).unwrap_or(&self.captured.glues);
        g.get("baselineskip").and_then(|v| v.first()).map(|v| *v as i64).unwrap_or(0)
    }
    /// The context object the fast server replays before typesetting this unit.
    pub fn context_json(&self) -> serde_json::Value {
        let c = &self.captured;
        let (ints, dims, glues, parshape) = match &self.first_para {
            Some(p) if c.kind == "par" => (&p.ints, &p.dims, &p.glues, &p.parshape),
            _ => (&c.ints, &c.dims, &c.glues, &c.parshape),
        };
        let (nfss, color) = match self.first_para.as_ref().and_then(|p| p.begin.as_ref()) {
            Some(b) if c.kind == "par" => (b.nfss.clone(), b.color.clone()),
            _ => (c.nfss.clone(), c.color.clone()),
        };
        serde_json::json!({
            "kind": c.kind, "name": c.name,
            "ints": ints, "dims": dims, "glues": glues, "parshape": parshape,
            "everypar": c.everypar, "nobreak": c.nobreak, "afterindent": c.afterindent, "noskipsec": c.noskipsec,
            "counters": c.abs_counters,
            "begin": { "nfss": nfss, "color": color },
        })
    }
}

/// Snapshot spans of a file buffer (what the session records when a pass starts).
pub fn snapshot_spans_of(fb: &crate::document::FileBuf, file: &str, policy: &crate::eligibility::Policy) -> Vec<SnapshotSpan> {
    use crate::document::SpanKind;
    fb.spans.iter().map(|sp| {
        let (a, b) = fb.line_range(sp);
        let text = &fb.text[sp.range.clone()];
        let background_only = matches!(sp.kind, SpanKind::Preamble | SpanKind::Trailer) || !crate::eligibility::classify_source(text, policy).1.is_empty();
        SnapshotSpan { id: sp.id, file: file.to_string(), first_line: a, last_line: b, last_revision: sp.last_revision, background_only }
    }).collect()
}

#[derive(Debug, Default)]
pub struct LayoutStore {
    pub layout_version: u64,
    pub context_revision: u64,
    /// `source_revision` of the snapshot the latest layout was compiled from.
    pub snapshot_revision: Revision,
    pub units: Vec<EngineUnit>,
    pub by_span: HashMap<ParaId, usize>,
    pub pages: BTreeMap<i64, DisplayList>,
    pub page_hashes: BTreeMap<i64, u64>,
    pub snapshot_spans: Vec<SnapshotSpan>,
    pub capture_dir: Option<std::path::PathBuf>,
    pub pdf: Option<std::path::PathBuf>,
    /// `\newlabel`/`\bibcite` definitions of the pass (name, value) for the server's `\ref`/`\cite`.
    pub labels: Arc<Vec<(String, String)>>,
    pub labels_hash: u64,
}

impl LayoutStore {
    /// Install a finished capture pass. Maps units to spans by their first source line inside
    /// the snapshot's span line ranges (exactly one unit per span for a fast-eligible mapping).
    pub fn install(&mut self, cap: &CaptureResult, snapshot: Vec<SnapshotSpan>, snapshot_revision: Revision) -> anyhow::Result<Vec<i64>> {
        self.layout_version += 1;
        self.context_revision += 1;
        self.snapshot_revision = snapshot_revision;
        self.units.clear();
        self.by_span.clear();
        let paras: HashMap<i64, &CapturedParagraph> = cap.json.paragraphs.iter().map(|p| (p.seq, p)).collect();
        let mut per_span_count: HashMap<ParaId, usize> = HashMap::new();
        for u in &cap.json.units {
            let file = u.file.clone().unwrap_or_else(|| "./main.tex".into());
            let file = file.trim_start_matches("./").to_string();
            let mut span = None;
            for s in &snapshot {
                if s.file == file && u.begin_line >= s.first_line && u.begin_line <= s.last_line {
                    span = Some(s.id);
                    break;
                }
            }
            if let Some(id) = span {
                *per_span_count.entry(id).or_default() += 1;
            }
            let mut flags: BTreeMap<String, i64> = BTreeMap::new();
            let mut members = Vec::new();
            for seq in &u.seqs {
                if let Some(p) = paras.get(seq) {
                    for (k, v) in &p.flags {
                        *flags.entry(k.clone()).or_default() += v;
                    }
                    members.push((p.groupcode.clone(), p.nest));
                }
            }
            // the unit's body paragraph: the first member at the unit's own nesting level with a
            // para/begin record (footnote text and \parbox contents are line-broken earlier, at
            // deeper levels)
            let first_para = u.seqs.iter().filter_map(|s| paras.get(s)).find(|p| p.nest == 1 && p.begin.is_some())
                .or_else(|| u.seqs.iter().filter_map(|s| paras.get(s)).find(|p| p.begin.is_some()))
                .or_else(|| u.seqs.first().and_then(|s| paras.get(s)))
                .map(|p| (*p).clone());
            self.units.push(EngineUnit { uid: u.uid, captured: u.clone(), first_para, flags, members, span });
        }
        for (i, eu) in self.units.iter_mut().enumerate() {
            if let Some(id) = eu.span {
                if per_span_count.get(&id).copied().unwrap_or(0) == 1 {
                    self.by_span.insert(id, i);
                } else {
                    eu.span = None; // ambiguous: several units for one span
                }
            }
        }
        let mut changed = Vec::new();
        let mut new_pages = BTreeMap::new();
        let mut new_hashes = BTreeMap::new();
        for n in 1..=cap.json.pages {
            let dl = cap.page(n)?;
            let h = crate::document::hash_str(&serde_json::to_string(&dl)?);
            if self.page_hashes.get(&n) != Some(&h) {
                changed.push(n);
            }
            new_hashes.insert(n, h);
            new_pages.insert(n, dl);
        }
        for old in self.page_hashes.keys() {
            if !new_hashes.contains_key(old) {
                changed.push(*old);
            }
        }
        self.pages = new_pages;
        self.page_hashes = new_hashes;
        self.snapshot_spans = snapshot;
        self.capture_dir = Some(cap.out_dir.clone());
        self.pdf = Some(cap.pdf.clone());
        let labels = crate::background::read_aux_labels(&cap.out_dir.join(format!("{}.aux", cap.jobname)));
        self.labels_hash = crate::document::hash_str(&format!("{labels:?}"));
        self.labels = Arc::new(labels);
        Ok(changed)
    }

    pub fn unit(&self, id: ParaId) -> Option<&EngineUnit> {
        self.by_span.get(&id).map(|i| &self.units[*i])
    }

    /// Units mapped to spans, in document order: (unit, span id).
    pub fn mapped_units(&self) -> Vec<(&EngineUnit, ParaId)> {
        let mut v: Vec<(&EngineUnit, ParaId)> = self.units.iter().filter_map(|u| u.span.map(|s| (u, s))).collect();
        v.sort_by_key(|(u, _)| u.uid);
        v
    }

    /// Build a store from a capture of `project/main` without a session (verification tools,
    /// tests): the main file is segmented like the session would, and the capture installed.
    pub fn offline(cap: &CaptureResult, project: &std::path::Path, main: &str, policy: &crate::eligibility::Policy) -> anyhow::Result<(LayoutStore, crate::document::FileBuf)> {
        let text = std::fs::read_to_string(project.join(main))?;
        let mut ids = crate::document::IdAllocator(0);
        let fb = crate::document::FileBuf::with_block_envs(&text, &mut ids, 1, policy.theorem_envs.iter().cloned().collect());
        let spans = snapshot_spans_of(&fb, main, policy);
        let mut store = LayoutStore::default();
        store.install(cap, spans, 1)?;
        Ok((store, fb))
    }

    /// Page positions for the rows of a fast result. `rows` are the (x, baseline) of each row in
    /// the fast box's own coordinates. Within a page the rows keep the fast box's geometry,
    /// anchored at the page's first placed row; rows beyond the cached placements continue on the
    /// last page (`approximate`). Returns None when the unit has no placement; the bool says
    /// whether the pagination is stale (row count differs from the layout).
    pub fn fragments(&self, id: ParaId, rows: &[(Sp, Sp)]) -> Option<(Vec<Fragment>, bool)> {
        let eu = self.unit(id)?;
        let placements: &Vec<Placement> = &eu.captured.placements;
        if placements.is_empty() {
            return None;
        }
        let mut stale = rows.len() != placements.len();
        let mut frags = Vec::new();
        let mut i = 0usize; // placement index == fast row index while both exist
        while i < placements.len() && i < rows.len() {
            let page = placements[i].page;
            let anchor = &placements[i];
            let (ax, ay) = rows[i];
            let first = i;
            let mut xs = Vec::new();
            let mut baselines = Vec::new();
            while i < placements.len() && placements[i].page == page && i < rows.len() {
                let (rx, ry) = rows[i];
                xs.push(anchor.x + (rx - ax));
                baselines.push(anchor.y + (ry - ay));
                i += 1;
            }
            let last_page_group = i >= placements.len();
            let mut approximate = false;
            if last_page_group {
                while i < rows.len() {
                    let (rx, ry) = rows[i];
                    xs.push(anchor.x + (rx - ax));
                    baselines.push(anchor.y + (ry - ay));
                    i += 1;
                    approximate = true;
                    stale = true;
                }
            }
            frags.push(Fragment { page, first_line: first as i64 + 1, last_line: i as i64, x: xs[0], xs, baselines, approximate });
        }
        Some((frags, stale))
    }
}
