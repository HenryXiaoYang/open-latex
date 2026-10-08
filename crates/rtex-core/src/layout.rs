//! Versioned layout store: engine units (contexts + row placements) from the latest background
//! pass, their mapping to host spans, page display lists, labels, and fragment construction.

use crate::capture::{CaptureResult, CapturedParagraph, CapturedUnit, Placement};
use crate::document::{ParaId, Revision};
use rtex_dl::{DisplayList, Item, Line, Sp};
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
    /// What the fast path compiles for the span as the pass typeset it (`document::fast_source`
    /// of the snapshot: trailing newline removed, `\input` end-of-file space reproduced); the
    /// probe compile replays exactly this.
    pub text: String,
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
    /// Capture unit ids this unit is made of: one, or several consecutive paragraph units that
    /// share a source span (`{\Large Title\par}` followed by a line of text), typeset as one box.
    pub uids: Vec<i64>,
}

impl EngineUnit {
    pub fn kind(&self) -> &str {
        &self.captured.kind
    }
    /// Counters the unit advanced in the last pass (name → value at its end).
    pub fn advanced(&self) -> BTreeMap<String, i64> {
        self.captured.advanced.clone().unwrap_or_default()
    }
    pub fn name(&self) -> Option<&str> {
        self.captured.name.as_deref()
    }
    pub fn rows(&self) -> i64 {
        self.captured.placements.len() as i64
    }
    pub fn baselineskip(&self) -> Sp {
        let g = self
            .first_para
            .as_ref()
            .map(|p| &p.glues)
            .unwrap_or(&self.captured.glues);
        g.get("baselineskip")
            .and_then(|v| v.first())
            .map(|v| *v as i64)
            .unwrap_or(0)
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
pub fn snapshot_spans_of(
    fb: &crate::document::FileBuf,
    file: &str,
    policy: &crate::eligibility::Policy,
    file_is_inputted: bool,
) -> Vec<SnapshotSpan> {
    use crate::document::SpanKind;
    fb.spans
        .iter()
        .map(|sp| {
            let (a, b) = fb.line_range(sp);
            let text = &fb.text[sp.range.clone()];
            let background_only = matches!(sp.kind, SpanKind::Preamble | SpanKind::Trailer)
                || !crate::eligibility::classify_source(text, policy)
                    .1
                    .is_empty();
            SnapshotSpan {
                id: sp.id,
                file: file.to_string(),
                first_line: a,
                last_line: b,
                last_revision: sp.last_revision,
                background_only,
                text: crate::document::fast_source(fb, sp, file_is_inputted),
            }
        })
        .collect()
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
    /// Span id → index into `snapshot_spans`.
    snapshot_index: HashMap<ParaId, usize>,
    pub capture_dir: Option<std::path::PathBuf>,
    pub pdf: Option<std::path::PathBuf>,
    /// `\newlabel`/`\bibcite` definitions of the pass (name, value) for the server's `\ref`/`\cite`.
    pub labels: Arc<Vec<(String, String)>>,
    pub labels_hash: u64,
}

impl LayoutStore {
    /// Install a finished capture pass. Maps units to spans by their first source line inside
    /// the snapshot's span line ranges (exactly one unit per span for a fast-eligible mapping).
    pub fn install(
        &mut self,
        cap: &CaptureResult,
        snapshot: Vec<SnapshotSpan>,
        snapshot_revision: Revision,
    ) -> anyhow::Result<Vec<i64>> {
        self.layout_version += 1;
        self.context_revision += 1;
        self.snapshot_revision = snapshot_revision;
        self.units.clear();
        self.by_span.clear();
        let paras: HashMap<i64, &CapturedParagraph> =
            cap.json.paragraphs.iter().map(|p| (p.seq, p)).collect();
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
            if span.is_none() {
                // a unit that begins on a blank line between two spans: it belongs to the next
                // span when it extends into it (the engine reported the paragraph a line early,
                // seen after \newpage in standby passes), otherwise to the previous one (a macro
                // at the end of its last line read ahead before producing the unit: hyperref's
                // \maketitle wrapper)
                let next = if u.kind == "par" {
                    snapshot
                        .iter()
                        .find(|s| {
                            s.file == file
                                && s.first_line > u.begin_line
                                && u.end_line.unwrap_or(u.begin_line) >= s.first_line
                        })
                        .map(|s| s.id)
                } else {
                    None
                };
                span = next.or_else(|| {
                    snapshot
                        .iter()
                        .find(|s| s.file == file && s.last_line + 1 == u.begin_line)
                        .map(|s| s.id)
                });
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
            let first_para = u
                .seqs
                .iter()
                .filter_map(|s| paras.get(s))
                .find(|p| p.nest == 1 && p.begin.is_some())
                .or_else(|| {
                    u.seqs
                        .iter()
                        .filter_map(|s| paras.get(s))
                        .find(|p| p.begin.is_some())
                })
                .or_else(|| u.seqs.first().and_then(|s| paras.get(s)))
                .map(|p| (*p).clone());
            self.units.push(EngineUnit {
                uid: u.uid,
                captured: u.clone(),
                first_para,
                flags,
                members,
                span,
                uids: vec![u.uid],
            });
        }
        // Several consecutive paragraph units in one span (an explicit \par, a title line followed
        // by a text line) form one composite unit: the fast path typesets the span as one box, and
        // its rows are the members' rows in order. The first member provides the context.
        let mut idx_by_span: HashMap<ParaId, Vec<usize>> = HashMap::new();
        for (i, eu) in self.units.iter().enumerate() {
            if let Some(id) = eu.span {
                idx_by_span.entry(id).or_default().push(i);
            }
        }
        let mut merged_away: Vec<usize> = Vec::new();
        for (id, idxs) in idx_by_span.iter() {
            if idxs.len() < 2
                || !idxs.iter().all(|i| {
                    self.units[*i].captured.kind == "par" && self.units[*i].captured.nest == 1
                })
            {
                continue;
            }
            let mut idxs = idxs.clone();
            idxs.sort();
            let (first, rest) = idxs.split_first().unwrap();
            let mut placements = Vec::new();
            let mut seqs = Vec::new();
            let mut members = Vec::new();
            let mut flags: BTreeMap<String, i64> = BTreeMap::new();
            let mut end_line = None;
            let mut uids = Vec::new();
            let mut advanced: BTreeMap<String, i64> = BTreeMap::new();
            for i in rest {
                let u = &self.units[*i];
                if let Some(a) = &u.captured.advanced {
                    advanced.extend(a.iter().map(|(k, v)| (k.clone(), *v)));
                }
                placements.extend(u.captured.placements.iter().cloned());
                seqs.extend(u.captured.seqs.iter().cloned());
                members.extend(u.members.iter().cloned());
                for (k, v) in &u.flags {
                    *flags.entry(k.clone()).or_default() += v;
                }
                end_line = u.captured.end_line.or(end_line);
                uids.push(u.uid);
                merged_away.push(*i);
            }
            let f = &mut self.units[*first];
            f.captured.placements.extend(placements);
            for (k, p) in f.captured.placements.iter_mut().enumerate() {
                p.row = k as i64 + 1;
            }
            f.captured.rows = f.captured.placements.len() as i64;
            f.captured.seqs.extend(seqs);
            f.members.extend(members);
            for (k, v) in flags {
                *f.flags.entry(k).or_default() += v;
            }
            if end_line.is_some() {
                f.captured.end_line = end_line;
            }
            if !advanced.is_empty() {
                let mut all = f.captured.advanced.take().unwrap_or_default();
                all.extend(advanced);
                f.captured.advanced = Some(all);
            }
            f.uids.extend(uids);
            per_span_count.insert(*id, 1);
        }
        for i in merged_away {
            self.units[i].span = None;
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
        self.snapshot_index = snapshot
            .iter()
            .enumerate()
            .map(|(i, sp)| (sp.id, i))
            .collect();
        self.snapshot_spans = snapshot;
        self.capture_dir = Some(cap.out_dir.clone());
        self.pdf = Some(cap.pdf.clone());
        let labels =
            crate::background::read_aux_labels(&cap.out_dir.join(format!("{}.aux", cap.jobname)));
        self.labels_hash = crate::document::hash_str(&format!("{labels:?}"));
        self.labels = Arc::new(labels);
        Ok(changed)
    }

    pub fn unit(&self, id: ParaId) -> Option<&EngineUnit> {
        self.by_span.get(&id).map(|i| &self.units[*i])
    }

    /// The text of span `id` as the installed pass typeset it.
    pub fn snapshot_text(&self, id: ParaId) -> Option<&str> {
        self.snapshot_index
            .get(&id)
            .map(|i| self.snapshot_spans[*i].text.as_str())
    }

    /// Probe check: does `fast`, the fast compile of the span's snapshot text, reproduce the
    /// rows the pass shipped for its unit? `Ok` when every row has the same box, glue set and
    /// glyphs (font, char, position, width, expansion); `Err(why)` names the first difference.
    pub fn probe_check(&self, id: ParaId, fast: &DisplayList) -> Result<(), String> {
        let Some(eu) = self.unit(id) else {
            return Err("no layout unit to compare with".into());
        };
        let mut pnos: Vec<i64> = eu.captured.placements.iter().map(|pl| pl.page).collect();
        pnos.sort();
        pnos.dedup();
        let pages: Vec<(i64, &DisplayList)> = pnos
            .iter()
            .filter_map(|p| self.pages.get(p).map(|d| (*p, d)))
            .collect();
        if pages.len() != pnos.len() {
            return Err("page display lists not available".into());
        }
        let (same, total, notes) = compare_unit_rows(fast, &pages, &eu.uids);
        if let Some(n) = notes.first() {
            return Err(n.clone());
        }
        if same != total {
            return Err(format!("{} of {} glyphs differ", total - same, total));
        }
        if total == 0 {
            // an empty box against no rows proves nothing
            return Err("nothing to compare: the unit has no glyph rows".into());
        }
        Ok(())
    }

    /// Units mapped to spans, in document order: (unit, span id).
    pub fn mapped_units(&self) -> Vec<(&EngineUnit, ParaId)> {
        let mut v: Vec<(&EngineUnit, ParaId)> = self
            .units
            .iter()
            .filter_map(|u| u.span.map(|s| (u, s)))
            .collect();
        v.sort_by_key(|(u, _)| u.uid);
        v
    }

    /// Build a store from a capture of `project/main` without a session (verification tools,
    /// tests): the main file and the files it inputs are segmented like the session would, and
    /// the capture installed.
    pub fn offline(
        cap: &CaptureResult,
        project: &std::path::Path,
        main: &str,
        policy: &crate::eligibility::Policy,
    ) -> anyhow::Result<(LayoutStore, crate::document::FileSet)> {
        let texts = crate::document::load_project_files(project, main)?;
        let mut ids = crate::document::IdAllocator(0);
        let mut set = crate::document::FileSet::default();
        set.inputted = crate::document::inputted_files(&texts);
        let mut spans = Vec::new();
        // the main file first so its ids come first (tools pick "the middle paragraph" by id)
        for (name, text) in std::iter::once((main, texts[main].as_str())).chain(
            texts
                .iter()
                .filter(|(n, _)| n.as_str() != main)
                .map(|(n, t)| (n.as_str(), t.as_str())),
        ) {
            let fb = crate::document::FileBuf::with_block_envs(
                text,
                &mut ids,
                1,
                policy.theorem_envs.iter().cloned().collect(),
            );
            spans.extend(snapshot_spans_of(
                &fb,
                name,
                policy,
                set.inputted.contains(name),
            ));
            set.files.insert(name.to_string(), fb);
        }
        let mut store = LayoutStore::default();
        store.install(cap, spans, 1)?;
        Ok((store, set))
    }

    /// Page positions for the rows of a unit the layout has no placement for, relative to a
    /// neighbouring unit that has one. `after`: the rows follow the parent's rows, of which the
    /// parent currently shows `parent_rows` (its last fast result; its placement count when
    /// unknown) — exact for consecutive paragraphs with the same baselineskip and no parskip;
    /// otherwise the rows end one baselineskip above the parent's first row. Always approximate.
    pub fn fragments_relative(
        &self,
        parent: ParaId,
        parent_rows: Option<i64>,
        after: bool,
        rows: &[(Sp, Sp)],
    ) -> Option<Vec<Fragment>> {
        let eu = self.unit(parent)?;
        let pl = &eu.captured.placements;
        if pl.is_empty() || rows.is_empty() {
            return None;
        }
        let bs = eu.baselineskip().max(1);
        let (fx, fy) = rows[0];
        let (page, ax, ay) = if after {
            let idx = parent_rows.unwrap_or(pl.len() as i64).max(0) as usize;
            if idx < pl.len() {
                (pl[idx].page, pl[idx].x, pl[idx].y)
            } else {
                let last = pl.last().unwrap();
                (
                    last.page,
                    last.x,
                    last.y + (idx as i64 - (pl.len() as i64 - 1)) * bs,
                )
            }
        } else {
            let (_, ly) = rows[rows.len() - 1];
            (pl[0].page, pl[0].x, pl[0].y - bs - (ly - fy))
        };
        let xs: Vec<Sp> = rows.iter().map(|(rx, _)| ax + (rx - fx)).collect();
        let baselines: Vec<Sp> = rows.iter().map(|(_, ry)| ay + (ry - fy)).collect();
        Some(vec![Fragment {
            page,
            first_line: 1,
            last_line: rows.len() as i64,
            x: xs[0],
            xs,
            baselines,
            approximate: true,
        }])
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
            frags.push(Fragment {
                page,
                first_line: first as i64 + 1,
                last_line: i as i64,
                x: xs[0],
                xs,
                baselines,
                approximate,
            });
        }
        Some((frags, stale))
    }
}

/// Compare a fast-path unit display list with the pass's rows for the same unit, which may be
/// spread over several pages. Every row gets its own (x, y) offset from its placement, so the
/// comparison is of each row's content and geometry, not of the vertical arrangement (which the
/// page builder owns). Returns (identical glyphs, compared glyphs, notes on differences).
pub fn compare_unit_rows(
    fast: &DisplayList,
    pages: &[(i64, &DisplayList)],
    uids: &[i64],
) -> (usize, usize, Vec<String>) {
    // rows of every member unit (a composite unit is several capture units), in member order
    let mut keyed: Vec<((usize, i64), i64, &Line)> = Vec::new();
    for (k, uid) in uids.iter().enumerate() {
        for (pno, page) in pages {
            for l in page.rows_of(*uid) {
                keyed.push(((k, l.row), *pno, l));
            }
        }
    }
    keyed.sort_by_key(|(key, _, _)| *key);
    let ref_rows: Vec<(i64, &Line)> = keyed.into_iter().map(|(_, pno, l)| (pno, l)).collect();
    let mut notes = Vec::new();
    if ref_rows.len() != fast.lines.len() {
        notes.push(format!(
            "row count: fast {} vs capture {}",
            fast.lines.len(),
            ref_rows.len()
        ));
    }
    if ref_rows.is_empty() || fast.lines.is_empty() {
        return (0, 0, notes);
    }
    let (mut same, mut total) = (0, 0);
    for (k, (fl, (pno, rl))) in fast.lines.iter().zip(ref_rows.iter()).enumerate() {
        let page: &DisplayList = pages
            .iter()
            .find(|(p, _)| p == pno)
            .map(|(_, d)| *d)
            .unwrap();
        let (ox, oy) = (rl.x - fl.x, rl.y - fl.y);
        if fl.w != rl.w || fl.h != rl.h || fl.d != rl.d || (fl.gs - rl.gs).abs() > 1e-12 {
            notes.push(format!(
                "row {} box/glue differs: fast ({},{},{} gs {}) capture ({},{},{} gs {})",
                k + 1,
                fl.w,
                fl.h,
                fl.d,
                fl.gs,
                rl.w,
                rl.h,
                rl.d,
                rl.gs
            ));
        }
        let fg: Vec<&Item> = fl
            .items
            .iter()
            .filter(|i| matches!(i, Item::Glyph { .. }))
            .collect();
        let rg: Vec<&Item> = rl
            .items
            .iter()
            .filter(|i| matches!(i, Item::Glyph { .. }))
            .collect();
        if fg.len() != rg.len() {
            notes.push(format!(
                "row {} glyph count {} vs {}",
                k + 1,
                fg.len(),
                rg.len()
            ));
        }
        for (a, b) in fg.iter().zip(rg.iter()) {
            total += 1;
            if let (
                Item::Glyph {
                    font: fa,
                    char: ca,
                    index: ia,
                    x: xa,
                    y: ya,
                    width: wa,
                    expansion: ea,
                },
                Item::Glyph {
                    font: fb,
                    char: cb,
                    index: ib,
                    x: xb,
                    y: yb,
                    width: wb,
                    expansion: eb,
                },
            ) = (a, b)
            {
                let font_ok = match (fast.font(*fa), page.font(*fb)) {
                    (Some(da), Some(db)) => da.key() == db.key(),
                    _ => fa == fb,
                };
                if font_ok
                    && ca == cb
                    && ia == ib
                    && xa + ox == *xb
                    && ya + oy == *yb
                    && wa == wb
                    && ea == eb
                {
                    same += 1;
                } else if notes.len() < 6 {
                    notes.push(format!(
                        "row {} glyph differs: fast {:?} capture {:?}",
                        k + 1,
                        a,
                        b
                    ));
                }
            }
        }
        for (kind, pred) in [
            (
                "rule",
                (|i: &&Item| matches!(i, Item::Rule { .. })) as fn(&&Item) -> bool,
            ),
            ("image", |i: &&Item| matches!(i, Item::Image { .. })),
        ] {
            let fr = fl.items.iter().filter(pred).count();
            let rr = rl.items.iter().filter(pred).count();
            if fr != rr {
                notes.push(format!("row {} {kind} count {} vs {}", k + 1, fr, rr));
            }
        }
    }
    (same, total, notes)
}
