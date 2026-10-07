//! Versioned layout store: engine paragraphs (contexts + placements) from the latest background
//! pass, their mapping to host spans, page display lists, and fragment construction.

use crate::capture::{CapturedParagraph, CaptureResult, Placement};
use crate::document::{ParaId, Revision};
use lode_dl::{DisplayList, Sp};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize)]
pub struct Fragment {
    pub page: i64,
    /// 1-based inclusive range of the paragraph's lines on this page.
    pub first_line: i64,
    pub last_line: i64,
    pub x: Sp,
    pub baselines: Vec<Sp>,
    /// Lines beyond the cached placement were extrapolated by `\baselineskip`.
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

#[derive(Debug, Clone)]
pub struct EngineParagraph {
    pub seq: i64,
    pub captured: CapturedParagraph,
    pub span: Option<ParaId>,
}

#[derive(Debug, Default)]
pub struct LayoutStore {
    pub layout_version: u64,
    pub context_revision: u64,
    /// `source_revision` of the snapshot the latest layout was compiled from.
    pub snapshot_revision: Revision,
    pub paragraphs: Vec<EngineParagraph>,
    pub by_span: HashMap<ParaId, usize>,
    pub pages: BTreeMap<i64, DisplayList>,
    pub page_hashes: BTreeMap<i64, u64>,
    pub snapshot_spans: Vec<SnapshotSpan>,
    pub capture_dir: Option<std::path::PathBuf>,
    pub pdf: Option<std::path::PathBuf>,
}

impl LayoutStore {
    /// Install a finished capture pass. Maps engine paragraphs to spans by overlapping line ranges
    /// in the snapshot (exactly one engine paragraph per span for a fast-eligible mapping).
    pub fn install(&mut self, cap: &CaptureResult, snapshot: Vec<SnapshotSpan>, snapshot_revision: Revision) -> anyhow::Result<Vec<i64>> {
        self.layout_version += 1;
        self.context_revision += 1;
        self.snapshot_revision = snapshot_revision;
        self.paragraphs.clear();
        self.by_span.clear();
        let mut per_span_count: HashMap<ParaId, usize> = HashMap::new();
        for p in &cap.json.paragraphs {
            let mut span = None;
            if let (Some(start), true) = (p.start_line(), p.is_top_level()) {
                let file = p.file.clone().unwrap_or_else(|| "./main.tex".into());
                let file = file.trim_start_matches("./").to_string();
                for s in &snapshot {
                    if s.file == file && start >= s.first_line && start <= s.last_line {
                        span = Some(s.id);
                        break;
                    }
                }
            }
            if let Some(id) = span {
                *per_span_count.entry(id).or_default() += 1;
            }
            self.paragraphs.push(EngineParagraph { seq: p.seq, captured: p.clone(), span });
        }
        for (i, ep) in self.paragraphs.iter_mut().enumerate() {
            if let Some(id) = ep.span {
                if per_span_count.get(&id).copied().unwrap_or(0) == 1 {
                    self.by_span.insert(id, i);
                } else {
                    ep.span = None; // ambiguous: several engine paragraphs for one span
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
        Ok(changed)
    }

    pub fn engine_paragraph(&self, id: ParaId) -> Option<&EngineParagraph> {
        self.by_span.get(&id).map(|i| &self.paragraphs[*i])
    }

    /// Fragments for `id` given the number of lines (and their baseline distances) the fast
    /// result produced. Returns None when the paragraph has no placement.
    pub fn fragments(&self, id: ParaId, fast_baselines: &[Sp], baselineskip: Sp) -> Option<(Vec<Fragment>, bool)> {
        let ep = self.engine_paragraph(id)?;
        let placements: &Vec<Placement> = ep.captured.placements.as_ref()?;
        if placements.is_empty() {
            return None;
        }
        let mut by_page: BTreeMap<i64, Vec<&Placement>> = BTreeMap::new();
        for p in placements {
            by_page.entry(p.page).or_default().push(p);
        }
        let mut frags = Vec::new();
        let mut stale = fast_baselines.len() != placements.len();
        let mut consumed = 0usize;
        let pages: Vec<i64> = by_page.keys().copied().collect();
        for (k, page) in pages.iter().enumerate() {
            let pl = &by_page[page];
            let is_last = k + 1 == pages.len();
            let mut baselines: Vec<Sp> = Vec::new();
            let x = pl[0].x;
            let mut approximate = false;
            for p in pl.iter() {
                if consumed < fast_baselines.len() {
                    baselines.push(p.y);
                    consumed += 1;
                }
            }
            if is_last {
                // extra fast lines: extrapolate by baselineskip
                let mut last_y = baselines.last().copied().or_else(|| pl.last().map(|p| p.y)).unwrap_or(0);
                while consumed < fast_baselines.len() {
                    last_y += baselineskip;
                    baselines.push(last_y);
                    consumed += 1;
                    approximate = true;
                    stale = true;
                }
            }
            if baselines.is_empty() {
                continue;
            }
            let first_line = (consumed - baselines.len()) as i64 + 1;
            let last_line = consumed as i64;
            frags.push(Fragment { page: *page, first_line, last_line, x, baselines, approximate });
        }
        Some((frags, stale))
    }
}
