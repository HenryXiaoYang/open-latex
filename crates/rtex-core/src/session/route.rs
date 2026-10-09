//! Routing an edited span: whether it can be compiled live, on which context (its own
//! captured one or one borrowed from a neighbour), with which picture cache entries.

use super::*;

impl Session {
    /// Decide the fast path for one span: the allow-list on its source, the capture facts of
    /// its unit (or a borrowed context when the layout does not know it yet), the budget and the
    /// context staleness. Returns the request to send, or the reasons it goes to the background.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn route_span(
        &self,
        rel_path: &str,
        files: &BTreeMap<String, FileBuf>,
        fb: &FileBuf,
        span: &Span,
        rev: Revision,
        edit_id: u64,
        layout: &LayoutStore,
        policy: &Policy,
    ) -> (Option<FastRequest>, Vec<String>) {
        let text = &fb.text[span.range.clone()];
        let (first_line, _) = fb.line_range(span);
        let mut reasons: Vec<String> = Vec::new();
        if matches!(span.kind, SpanKind::Preamble | SpanKind::Trailer) {
            reasons.push(format!("{:?} span", span.kind));
        }
        let probe_mode = self.shared.cfg.eligibility == EligibilityMode::Probe;
        let (shape, src_reasons) = classify_source_with(text, policy, false);
        let mut needs_probe = false;
        let mut vocab: Vec<Reason> = Vec::new();
        for r in src_reasons {
            if probe_mode && crate::eligibility::is_vocabulary_reason(&r) {
                // not a verdict: the probe compile decides
                needs_probe = true;
                vocab.push(r);
            } else {
                reasons.push(reason_str(&r));
            }
        }
        // read the live record once, into locals: no lock guard lives in a scrutinee (one
        // there stays held through the whole block)
        let (leak, internal_error, over_budget) = {
            let live = self.shared.live.lock();
            (
                live.get(span.id).and_then(|u| u.leak.clone()),
                live.internal_error_for(span.id, layout.layout_version),
                live.get(span.id).and_then(|u| u.over_budget),
            )
        };
        if let Some(why) = leak {
            reasons.push(format!("unverified: {why}"));
        }
        if let Some(why) = internal_error {
            reasons.push(format!(
                "EngineFailed: internal error, retried after the next layout ({})",
                first_line_of(&why)
            ));
        }
        let mut seq = 0i64;
        let mut ctx: Option<serde_json::Value> = None;
        let mut expected_rows = 0i64;
        let mut derived_from: Option<(ParaId, ParaId, bool)> = None;
        let mut probe = false;
        let (pics, pics_n, pics_cached) =
            self.unit_pics(rel_path, files, fb, span, rev, layout.layout_version);
        // the unit's only vocabulary beyond the allow-list is picture environments, and the
        // cache holds every one of them: their bodies are not run, so there is nothing for a
        // probe to prove (a probe compiles the snapshot's text, whose pictures the entries of
        // the current text need not match: it would draw them, a pgfplots axis for seconds)
        let pictures_only = !vocab.is_empty()
            && vocab.iter().all(|r| {
                matches!(r, Reason::DisallowedEnvironment(e)
                    if crate::piccache::PICTURE_ENVS.contains(&e.as_str()))
            });
        let cache_vouched = pictures_only && pics_n > 0 && pics_cached == pics_n;
        let mut pics_required = false;
        match layout.unit(span.id) {
            Some(eu) => {
                if needs_probe {
                    let verdict = self
                        .shared
                        .live
                        .lock()
                        .probe_for(span.id, layout.layout_version);
                    match verdict {
                        Some(ProbeVerdict::Verified) => {}
                        Some(ProbeVerdict::Mismatch(why)) | Some(ProbeVerdict::Leak(why)) => {
                            reasons.push(format!("unverified: {why}"))
                        }
                        None if cache_vouched => pics_required = true,
                        None => {
                            if layout.snapshot_text(span.id).is_some() {
                                probe = true;
                            } else {
                                reasons.push("unverified: no snapshot to compare with".into());
                            }
                        }
                    }
                }
                let c = &eu.captured;
                for r in
                    check_engine_unit(&c.kind, &c.everypar, eu.has_context(), eu.rows(), &eu.flags)
                {
                    reasons.push(reason_str(&r));
                }
                let shape_ok = match (&shape, c.kind.as_str()) {
                    (UnitShape::Par, "par") => true,
                    (UnitShape::Env(n), "env") => Some(n.as_str()) == c.name.as_deref(),
                    (UnitShape::Heading(n), "heading") => Some(n.as_str()) == c.name.as_deref(),
                    _ => false,
                };
                if !shape_ok {
                    reasons.push(reason_str(&Reason::KindMismatch));
                }
                if let Some((lv, ms)) = over_budget {
                    if lv == QUARANTINED {
                        reasons.push("EngineFailed: the live engine hung or crashed on this paragraph; it stays on the full compile until the preamble changes".into());
                    } else if lv == layout.layout_version {
                        reasons.push(reason_str(&Reason::OverBudget(ms)));
                    }
                }
                seq = eu.uid;
                expected_rows = eu.rows();
            }
            None => {
                // a unit the layout does not know yet (a split's second half, a paragraph or a
                // block environment typed fresh) borrows a context: a paragraph, or a block
                // environment that is not a float (a float's box state comes from the capture
                // only). It cannot be probed (no layout rows to compare with), so vocabulary
                // beyond the allow-list keeps it on the background path, with one exception:
                // picture environments the cache holds, every one of them, whose bodies the
                // engine does not run (the result delivery drops the compile when a picture
                // was drawn after all).
                let derivable = matches!(span.kind, SpanKind::Body | SpanKind::Env)
                    && match &shape {
                        UnitShape::Par => true,
                        UnitShape::Env(n) => !crate::eligibility::FLOAT_ENVS.contains(&n.as_str()),
                        UnitShape::Heading(_) => false,
                    }
                    && (!needs_probe || cache_vouched);
                pics_required = needs_probe && derivable;
                if !derivable && needs_probe {
                    reasons.push("unverified: no layout unit to compare with".into());
                }
                if reasons.is_empty() && derivable {
                    match self.derive_context(fb, span, layout, &shape) {
                        Some((parent, anchor, after, json)) => {
                            // negative context ids never collide with unit ids
                            seq = -(span.id.0 as i64);
                            ctx = Some(json);
                            derived_from = Some((parent, anchor, after));
                        }
                        None => reasons.push("NoContext".into()),
                    }
                } else {
                    reasons.push("NoContext".into());
                }
            }
        }
        if !reasons.is_empty() {
            return (None, reasons);
        }
        // context staleness: a background-only change in this file before this span, after the
        // snapshot this context came from; a borrowed context is stale by definition
        let stale = derived_from.is_some() || {
            let bg = self.shared.bg_change.lock();
            bg.get(rel_path)
                .map(|v| {
                    v.iter()
                        .any(|(r, line)| *r > layout.snapshot_revision && *line < first_line)
                })
                .unwrap_or(false)
        };
        if stale && !self.shared.cfg.fast_on_stale_context {
            reasons.push("ContextStale".into());
            return (None, reasons);
        }
        if let Some(d) = derived_from {
            self.shared.live.lock().unit(span.id).derived = Some(d);
        }
        let req = FastRequest {
            warmup: false,
            par_id: span.id,
            edit_id,
            span_hash: span.hash,
            source: crate::document::fast_source(
                fb,
                span,
                self.shared.inputted.lock().contains(rel_path),
            ),
            pics,
            pics_n,
            seq,
            ctx,
            versions: Versions {
                source_revision: rev,
                context_revision: layout.context_revision,
                engine_generation: self.shared.engine_generation.load(Ordering::SeqCst),
                layout_version: layout.layout_version,
            },
            context_stale: stale,
            expected_rows,
            probe,
            pics_required,
        };
        (Some(req), reasons)
    }

    /// Picture cache entries for the picture environments of a span, in document order (the
    /// live engine replaces a matching picture with the cached region instead of drawing it:
    /// the compile of a paragraph followed by a plot costs the text, not the plot). Returns the
    /// entries the cache holds, each with the 1-based `line` of its `\begin` within the unit's
    /// source (the server matches a picture by the line it begins on, never by position), the
    /// number of pictures scanned in the span and the number of entries. Entries are looked up
    /// by the same hash the background pass uses; a picture without one is left out (drawn).
    /// Returns an empty string and zeros when no entry applies.
    pub(super) fn unit_pics(
        &self,
        rel_path: &str,
        files: &BTreeMap<String, FileBuf>,
        fb: &FileBuf,
        span: &Span,
        rev: Revision,
        layout_version: u64,
    ) -> (String, usize, usize) {
        use crate::piccache::{preamble_hash, scan_pictures, PicCache, PICTURE_ENVS};
        let text = &fb.text[span.range.clone()];
        if !self.shared.cfg.picture_cache
            || !PICTURE_ENVS
                .iter()
                .any(|e| text.contains(&format!("\\begin{{{e}}}")))
        {
            return (String::new(), 0, 0);
        }
        let pics = {
            let mut scan = self.shared.pic_scan.lock();
            match scan.as_ref().filter(|(r, _)| *r == rev) {
                Some((_, pics)) => pics.clone(),
                None => {
                    let texts: BTreeMap<String, String> = files
                        .iter()
                        .map(|(n, f)| (n.clone(), f.text.clone()))
                        .collect();
                    let main = &self.shared.cfg.main_file;
                    let pics = Arc::new(scan_pictures(&texts, main, preamble_hash(&texts, main)));
                    *scan = Some((rev, pics.clone()));
                    pics
                }
            }
        };
        let (first, last) = fb.line_range(span);
        let file = rel_path.trim_start_matches("./");
        let mut idx = self.shared.pic_index.lock();
        if idx
            .as_ref()
            .map(|(v, _)| *v != layout_version)
            .unwrap_or(true)
        {
            *idx = Some((
                layout_version,
                PicCache::open(&self.shared.cfg.build_dir.join("bg").join("pic-cache")),
            ));
        }
        let cache = &idx.as_ref().unwrap().1;
        let mut entries = Vec::new();
        let (mut scanned, mut cached) = (0usize, 0usize);
        for p in pics.iter() {
            let Some((f, line)) = p.key.rsplit_once(':') else {
                continue;
            };
            let Ok(line) = line.parse::<i64>() else {
                continue;
            };
            if f != file || line < first || line > last {
                continue;
            }
            scanned += 1;
            if let Some(mut e) = cache.entry_json(p) {
                e["line"] = serde_json::Value::from(line - first + 1);
                entries.push(e);
                cached += 1;
            }
        }
        if cached == 0 {
            return (String::new(), 0, 0);
        }
        (
            serde_json::to_string(&entries).unwrap_or_default(),
            scanned,
            cached,
        )
    }

    /// Context for a paragraph the layout does not know yet (created by a split or a merge, or
    /// typed fresh): a plain body paragraph borrows the parameters, fonts and counters of the
    /// nearest paragraph unit before it (after it when there is none) with the paragraph-start
    /// state of a paragraph that follows a paragraph, or a heading when a heading span precedes
    /// it. The rows are placed after the nearest span before it that has rows at all: a layout
    /// unit of any kind (a heading, an environment, a paragraph) or a span itself live on a
    /// borrowed context (the other half of a split, the previous new paragraph), so the new
    /// unit follows what precedes it instead of landing on a heading or on the other half. The
    /// next layout replaces the borrowed context with a captured one. Returns (unit the context
    /// came from, span the rows are placed against, whether they follow it, context).
    pub(super) fn derive_context(
        &self,
        fb: &FileBuf,
        span: &Span,
        layout: &LayoutStore,
        shape: &UnitShape,
    ) -> Option<(ParaId, ParaId, bool, serde_json::Value)> {
        let idx = fb.spans.iter().position(|sp| sp.id == span.id)?;
        let usable = |sp: &Span| -> Option<&EngineUnit> {
            if sp.kind != SpanKind::Body {
                return None;
            }
            let eu = layout.unit(sp.id)?;
            // a unit that ran past its span's last line (the blank line after it at most)
            // swallowed what follows (`\noindent` on a line of its own before a picture): the
            // span after it is already inside this unit, not a new one to place after it
            let within = match (layout.snapshot_span(sp.id), eu.captured.end_line) {
                (Some(ss), Some(end)) => end <= ss.last_line + 1,
                _ => true,
            };
            let ok = within
                && eu.kind() == "par"
                && eu.rows() > 0
                && eu
                    .first_para
                    .as_ref()
                    .map(|p| p.begin.is_some())
                    .unwrap_or(false)
                && everypar_allowed(&eu.captured.everypar);
            ok.then_some(eu)
        };
        // the context: the nearest paragraph unit before (after when there is none)
        let ctx_before = fb.spans[..idx]
            .iter()
            .rev()
            .find_map(|sp| usable(sp).map(|eu| (sp.id, eu)));
        let (parent, eu, ctx_after) = match ctx_before {
            Some((id, eu)) => (id, eu, false),
            None => fb.spans[idx + 1..]
                .iter()
                .find_map(|sp| usable(sp).map(|eu| (sp.id, eu, true)))?,
        };
        // the anchor: the nearest span before with rows, whatever its kind, or one live on a
        // borrowed context (a split's halves are routed in document order within one edit: the
        // second chains onto the first before its result exists; the engine delivers them in
        // that order, and the delivery falls back to the context's parent otherwise)
        let anchor = {
            let live = self.shared.live.lock();
            fb.spans[..idx].iter().rev().find_map(|sp| {
                if matches!(sp.kind, SpanKind::Preamble | SpanKind::Trailer) {
                    return None;
                }
                if layout.unit(sp.id).is_some_and(|u| u.rows() > 0) || live.is_derived(sp.id) {
                    Some(sp.id)
                } else {
                    None
                }
            })
        };
        let (anchor, after) = match anchor {
            Some(a) => (a, true),
            None if ctx_after => (parent, false),
            None => (parent, true),
        };
        let mut json = eu.context_json();
        let after_heading = idx > 0 && fb.spans[idx - 1].kind == SpanKind::Heading;
        json["everypar"] = serde_json::Value::String(if after_heading {
            AFTER_HEADING_EVERYPAR.to_string()
        } else {
            String::new()
        });
        json["nobreak"] = serde_json::Value::Bool(after_heading);
        json["afterindent"] = serde_json::Value::Bool(false);
        json["noskipsec"] = serde_json::Value::Bool(false);
        // a block environment starts in vertical mode with the same parameters; the server
        // tells environments from paragraphs by kind (floats, which are not borrowed, by name)
        match shape {
            UnitShape::Env(name) => {
                json["kind"] = serde_json::Value::String("env".into());
                json["name"] = serde_json::Value::String(name.clone());
            }
            _ => {
                json["kind"] = serde_json::Value::String("par".into());
                json["name"] = serde_json::Value::Null;
            }
        }
        Some((parent, anchor, after, json))
    }
}
