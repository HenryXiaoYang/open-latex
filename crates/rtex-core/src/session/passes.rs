//! Background passes: the scheduler thread, standby engines, per-pass directories, the pass
//! loop with the picture cache, and delivering a pass as a layout. PDF export.

use super::*;

// ---------------------------------------------------------------------------------------------
// Background thread: debounced full passes with capture, layout installation, exports.
// ---------------------------------------------------------------------------------------------
pub(super) fn background_thread(s: Arc<Shared>) {
    prepare_standby(&s);
    loop {
        let cmd = match s.bg_signal.1.recv() {
            Ok(c) => c,
            Err(_) => break,
        };
        match cmd {
            BgCmd::Quit => break,
            BgCmd::Export(job, out) => run_export(&s, job, out),
            BgCmd::Pass => {
                if s.bg_paused.load(Ordering::SeqCst) {
                    s.bg_pending_while_paused.store(true, Ordering::SeqCst);
                    continue;
                }
                // the first pass runs at once; later ones are debounced (drain Pass commands until quiet)
                let debounce = if s.layout.lock().layout_version == 0 {
                    Duration::from_millis(1)
                } else {
                    s.cfg.debounce
                };
                loop {
                    match s.bg_signal.1.recv_timeout(debounce) {
                        Ok(BgCmd::Pass) => continue,
                        Ok(BgCmd::Quit) => return,
                        Ok(BgCmd::Export(job, out)) => {
                            run_export(&s, job, out);
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                if s.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                run_background_pass(&s);
                prepare_standby(&s);
            }
        }
    }
    if let Some(w) = s.standby.lock().take() {
        w.kill();
    }
}

pub(super) fn standby_dir(s: &Shared, n: usize) -> PathBuf {
    s.cfg.build_dir.join(format!("src-body-{n}"))
}

/// Output directory of the pass that runs from `src-body-{n}`. Two alternate: a standby
/// (lualatex up to the end of the preamble) opens its log, and with some preambles its PDF,
/// as soon as it starts, so it must never share a directory with the pass that is running.
pub(super) fn pass_dir(s: &Shared, n: usize) -> PathBuf {
    s.cfg.build_dir.join("bg").join(format!("pass-{n}"))
}

/// The directory of the last finished pass (seeded from disk on the first call: the newer
/// of the two pass directories, or the pre-pass-directory layout `build/bg` itself).
pub(super) fn last_pass_dir(s: &Shared) -> Option<PathBuf> {
    let mut slot = s.last_pass_dir.lock();
    if slot.is_none() {
        let jobname = jobname_of(&s.cfg.main_file);
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        for d in [pass_dir(s, 0), pass_dir(s, 1), s.cfg.build_dir.join("bg")] {
            if let Ok(m) = std::fs::metadata(d.join(format!("{jobname}.aux"))) {
                if let Ok(t) = m.modified() {
                    if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                        best = Some((t, d));
                    }
                }
            }
        }
        *slot = best.map(|(_, d)| d);
    }
    slot.clone()
}

/// The slot (0 or 1) a new standby takes: the one that does not hold the last pass.
pub(super) fn standby_slot(s: &Shared) -> usize {
    match last_pass_dir(s) {
        Some(d) if d.ends_with("pass-0") => 1,
        _ => 0,
    }
}

pub(super) fn jobname_of(main: &str) -> String {
    Path::new(main)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("main")
        .to_string()
}

/// Start a standby engine for the next background pass (while the user types, it loads the
/// current preamble). Replaces a standby whose preamble is outdated; keeps a matching one.
pub(super) fn prepare_standby(s: &Shared) {
    if !s.cfg.warm_background || s.shutdown.load(Ordering::SeqCst) {
        return;
    }
    let (texts, _, _) = snapshot(s);
    let Some(pre_hash) = standby_preamble_hash(&texts, &s.cfg.main_file) else {
        return;
    };
    let mut slot = s.standby.lock();
    if let Some(w) = slot.as_mut() {
        if w.preamble_hash == pre_hash && w.is_alive() {
            return;
        }
    }
    if let Some(w) = slot.take() {
        w.kill();
    }
    let unit_envs = s.policy.lock().unit_envs_env();
    let n = standby_slot(s);
    match WarmEngine::spawn(
        &s.tl,
        &s.cfg.project_root,
        &texts,
        &s.cfg.main_file,
        &standby_dir(s, n),
        &pass_dir(s, n),
        true,
        &unit_envs,
    ) {
        Ok(w) => *slot = Some(w),
        Err(e) => {
            s.events
                .send(Event::Diagnostics {
                    source: "background".into(),
                    items: vec![Diagnostic {
                        severity: "warning".into(),
                        file: None,
                        line: None,
                        message: format!("standby engine: {e}"),
                        context: None,
                    }],
                })
                .ok();
        }
    }
}

pub(super) fn texts_of(files: &BTreeMap<String, FileBuf>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(n, fb)| (n.clone(), fb.text.clone()))
        .collect()
}

/// The preamble the fast server loads: the main file's preamble with `\input`ted files inlined
/// from the buffers, plus the document's body setup statements (`crate::server_preamble`).
pub(super) fn effective_preamble(texts: &BTreeMap<String, String>, main: &str) -> String {
    crate::server_preamble(texts, main)
}

/// The preamble as the standby engine hashes it (`write_body_snapshot`): inputs inlined, no
/// body setup (the standby compiles the whole body itself).
pub(super) fn standby_preamble_hash(texts: &BTreeMap<String, String>, main: &str) -> Option<u64> {
    texts
        .get(main)
        .and_then(|t| crate::split_preamble(t))
        .map(|(p, _)| crate::document::hash_str(&crate::document::expand_inputs(p, texts)))
}

pub(super) fn preamble_input_set(
    texts: &BTreeMap<String, String>,
    main: &str,
) -> std::collections::BTreeSet<String> {
    texts
        .get(main)
        .and_then(|t| crate::split_preamble(t))
        .map(|(p, _)| crate::document::transitive_inputs(p, texts))
        .unwrap_or_default()
}

pub(super) fn snapshot(s: &Shared) -> (BTreeMap<String, String>, Vec<SnapshotSpan>, Revision) {
    let files = s.files.lock();
    let rev = s.source_revision.load(Ordering::SeqCst);
    let policy = s.policy.lock();
    let inputted = s.inputted.lock();
    let mut texts = BTreeMap::new();
    let mut spans = Vec::new();
    for (name, fb) in files.iter() {
        texts.insert(name.clone(), fb.text.clone());
        spans.extend(crate::layout::snapshot_spans_of(
            fb,
            name,
            &policy,
            inputted.contains(name),
        ));
    }
    (texts, spans, rev)
}

pub(super) fn run_background_pass(s: &Shared) {
    s.bg_running.store(true, Ordering::SeqCst);
    run_background_pass_inner(s);
    s.bg_running.store(false, Ordering::SeqCst);
}

/// A pass of a multi-pass run that took long enough to be worth showing before the run is
/// stable (TikZ-heavy documents: 45 s per pass, three passes to converge).
pub(super) const PROVISIONAL_LAYOUT_AFTER: Duration = Duration::from_secs(2);

pub(super) fn run_background_pass_inner(s: &Shared) {
    let t0 = Instant::now();
    let (texts, spans, rev) = snapshot(s);
    let snap_dir = snapshot_dir(&s.cfg.build_dir);
    // a pass that cannot run is still a layout result: hosts see compile = Failed instead of a
    // pass that never ends
    let failed = |msg: String| {
        s.events
            .send(Event::Diagnostics {
                source: "background".into(),
                items: vec![Diagnostic {
                    severity: "error".into(),
                    file: None,
                    line: None,
                    message: msg.clone(),
                    context: None,
                }],
            })
            .ok();
        *s.convergence.lock() = Some(Convergence::PassLimitReached {
            passes: 0,
            reasons: vec![msg.clone()],
        });
        s.events
            .send(Event::LayoutUpdate {
                versions: Versions {
                    source_revision: rev,
                    ..Default::default()
                },
                compile: CompileStatus::Failed,
                convergence: Convergence::PassLimitReached {
                    passes: 0,
                    reasons: vec![msg],
                },
                passes: 0,
                pages_changed: vec![],
                pages_total: 0,
                placements: vec![],
                eligible_paragraphs: vec![],
                pdf_fallback: None,
                wall_ms: t0.elapsed().as_millis() as u64,
            })
            .ok();
    };
    if let Err(e) = write_snapshot(&s.cfg.project_root, &texts, &snap_dir) {
        failed(format!("snapshot: {e}"));
        return;
    }
    let out_dir = s.cfg.build_dir.join("bg");
    let unit_envs = s.policy.lock().unit_envs_env();
    // picture cache: pictures whose source and surroundings are unchanged come from an earlier
    // pass's PDF; the manifest is refreshed before every pass of the run (a pass's own
    // drawings serve the next one)
    let _ = std::fs::create_dir_all(&out_dir);
    let pics: Vec<crate::piccache::PictureRef> = if s.cfg.picture_cache {
        let pre_hash = crate::piccache::preamble_hash(&texts, &s.cfg.main_file);
        crate::piccache::scan_pictures(&texts, &s.cfg.main_file, pre_hash)
    } else {
        Vec::new()
    };
    let pic_cache =
        std::sync::Mutex::new(crate::piccache::PicCache::open(&out_dir.join("pic-cache")));
    // written into the directory of the pass about to run (the capture reads it from there)
    let refresh_manifest = |dir: &Path| {
        let manifest = dir.join("pic-manifest.json");
        if pics.is_empty() {
            let _ = std::fs::remove_file(&manifest);
            return;
        }
        if let Err(e) = pic_cache.lock().unwrap().write_manifest(&pics, &manifest) {
            log::warn!("picture cache manifest: {e:#}");
        }
    };
    let absorb = |cap: &crate::capture::CaptureResult| {
        if pics.is_empty() {
            return;
        }
        if let Err(e) = pic_cache.lock().unwrap().absorb(
            &pics,
            &cap.json.recorded_pics(),
            &cap.json.pic_mismatch,
            &cap.pdf,
        ) {
            log::warn!("picture cache: {e:#}");
        }
    };
    // the aux family every pass of this run starts from
    let aux_dir = last_pass_dir(s).unwrap_or_else(|| pass_dir(s, 0));
    // before a pass runs in its directory: the previous pass's aux family and the manifest
    let prepare_dir = |dir: &Path| {
        if let Some(from) = last_pass_dir(s) {
            if let Err(e) = crate::background::copy_aux_family(&from, dir) {
                log::warn!("aux family: {e:#}");
            }
        }
        refresh_manifest(dir);
    };
    let finished = |cap: &crate::capture::CaptureResult| {
        *s.last_pass_dir.lock() = Some(cap.out_dir.clone());
    };
    let spans_for_provisional = spans.clone();
    let mut pass_started = Instant::now();
    let mut on_pass = |cap: &crate::capture::CaptureResult, pass: u32| {
        // the host sees the provisional layout before the cache absorbs the pass's pictures
        // (a PDF round trip)
        if pass_started.elapsed() >= PROVISIONAL_LAYOUT_AFTER {
            deliver_layout(
                s,
                t0,
                cap,
                pass,
                false,
                spans_for_provisional.clone(),
                rev,
                true,
            );
        }
        absorb(cap);
        pass_started = Instant::now();
    };
    let result = if s.cfg.warm_background {
        // Every pass of the loop runs in a standby engine: the one prepared while the user
        // typed, then the one started when the previous pass was released (its preamble loads
        // while the body is typeset). Two snapshot directories alternate so a loading standby
        // never rewrites the files a running one reads.
        let pre_hash = texts
            .get(&s.cfg.main_file)
            .and_then(|t| crate::split_preamble(t))
            .map(|(p, _)| crate::document::hash_str(p));
        let mut runner = |_pass: u32| -> Result<crate::capture::CaptureResult> {
            let ready = {
                let mut slot = s.standby.lock();
                match slot.take() {
                    Some(mut w) => {
                        if Some(w.preamble_hash) == pre_hash && w.is_alive() {
                            Some(w)
                        } else {
                            w.kill();
                            None
                        }
                    }
                    None => None,
                }
            };
            let w = match ready {
                Some(w) => w,
                None => {
                    let slot = standby_slot(s);
                    WarmEngine::spawn(
                        &s.tl,
                        &s.cfg.project_root,
                        &texts,
                        &s.cfg.main_file,
                        &standby_dir(s, slot),
                        &pass_dir(s, slot),
                        true,
                        &unit_envs,
                    )?
                }
            };
            // this pass starts from the last one's aux family, then the next standby starts
            // in the other slot (whose previous results are consumed by now)
            prepare_dir(w.out_dir());
            let other = if w.src_dir.ends_with("src-body-0") {
                1
            } else {
                0
            };
            if let Ok(next) = WarmEngine::spawn(
                &s.tl,
                &s.cfg.project_root,
                &texts,
                &s.cfg.main_file,
                &standby_dir(s, other),
                &pass_dir(s, other),
                true,
                &unit_envs,
            ) {
                *s.standby.lock() = Some(next);
            }
            let cap = w.run(&s.cfg.project_root, &texts, &s.cfg.main_file)?;
            finished(&cap);
            Ok(cap)
        };
        run_pass_with_runner(
            &s.tl,
            &standby_dir(s, 0),
            &s.cfg.main_file,
            &aux_dir,
            s.cfg.max_passes,
            s.cfg.bib_tool,
            &mut runner,
            &mut on_pass,
        )
    } else {
        // plain passes: a fresh lualatex per pass in pass-0 (sequential, so one directory)
        let dir = pass_dir(s, 0);
        let mut runner = |_pass: u32| -> Result<crate::capture::CaptureResult> {
            prepare_dir(&dir);
            let cap = crate::capture::run_capture_with(
                &s.tl,
                &snap_dir,
                &s.cfg.main_file,
                &dir,
                true,
                &unit_envs,
            )?;
            finished(&cap);
            Ok(cap)
        };
        run_pass_with_runner(
            &s.tl,
            &snap_dir,
            &s.cfg.main_file,
            &aux_dir,
            s.cfg.max_passes,
            s.cfg.bib_tool,
            &mut runner,
            &mut on_pass,
        )
    };
    let outcome = match result {
        Ok(o) => o,
        Err(e) => {
            failed(format!("background pass: {e:#}"));
            return;
        }
    };
    absorb(&outcome.capture);
    let outcome_cap = outcome.capture;
    deliver_layout(
        s,
        t0,
        &outcome_cap,
        outcome.passes,
        outcome.aux_stable,
        spans,
        rev,
        false,
    );
}

pub(super) fn layout_failed(s: &Shared, t0: Instant, rev: Revision, msg: String) {
    s.events
        .send(Event::Diagnostics {
            source: "background".into(),
            items: vec![Diagnostic {
                severity: "error".into(),
                file: None,
                line: None,
                message: msg.clone(),
                context: None,
            }],
        })
        .ok();
    *s.convergence.lock() = Some(Convergence::PassLimitReached {
        passes: 0,
        reasons: vec![msg.clone()],
    });
    s.events
        .send(Event::LayoutUpdate {
            versions: Versions {
                source_revision: rev,
                ..Default::default()
            },
            compile: CompileStatus::Failed,
            convergence: Convergence::PassLimitReached {
                passes: 0,
                reasons: vec![msg],
            },
            passes: 0,
            pages_changed: vec![],
            pages_total: 0,
            placements: vec![],
            eligible_paragraphs: vec![],
            pdf_fallback: None,
            wall_ms: t0.elapsed().as_millis() as u64,
        })
        .ok();
}

/// Install a pass's capture as the current layout and tell the host. `provisional`: a pass of a
/// run that is not stable yet (another pass follows); the layout is usable, its convergence is
/// `Converging`.
#[allow(clippy::too_many_arguments)]
pub(super) fn deliver_layout(
    s: &Shared,
    t0: Instant,
    cap: &crate::capture::CaptureResult,
    passes: u32,
    aux_stable: bool,
    spans: Vec<SnapshotSpan>,
    rev: Revision,
    provisional: bool,
) {
    let mut diagnostics = parse_log(&cap.log);
    for d in &mut diagnostics {
        // the standby reads the preamble from rtex-preamble.tex (same line numbers)
        if d.file
            .as_deref()
            .map(|f| f.ends_with("rtex-preamble.tex"))
            .unwrap_or(false)
        {
            d.file = Some(s.cfg.main_file.clone());
        }
    }
    let errors = diagnostics.iter().filter(|d| d.severity == "error").count();
    let compile = if cap.json.pages == 0 {
        CompileStatus::Failed
    } else if errors > 0 {
        CompileStatus::CompiledWithErrors { count: errors }
    } else {
        CompileStatus::Ok
    };
    if compile == CompileStatus::Failed {
        s.events
            .send(Event::Diagnostics {
                source: "background".into(),
                items: diagnostics,
            })
            .ok();
        s.events
            .send(Event::LayoutUpdate {
                versions: Versions {
                    source_revision: rev,
                    ..Default::default()
                },
                compile,
                convergence: Convergence::PassLimitReached {
                    passes: passes,
                    reasons: vec!["no pages".into()],
                },
                passes: passes,
                pages_changed: vec![],
                pages_total: 0,
                placements: vec![],
                eligible_paragraphs: vec![],
                pdf_fallback: None,
                wall_ms: t0.elapsed().as_millis() as u64,
            })
            .ok();
        return;
    }
    let (changed, versions, placements, eligible, eligible_strict, pdf) = {
        // lock order everywhere: files, then layout, then policy (apply_edit holds the first two
        // together)
        let files = s.files.lock();
        let mut layout = s.layout.lock();
        let changed = match layout.install(&cap, spans, rev) {
            Ok(c) => {
                // verdicts are keyed by layout version; drop the old ones while the lock is held
                s.probe.lock().clear();
                // the PDF hosts render degraded pages from: a copy per layout, since the next
                // pass (started right after this one, a provisional layout's in particular)
                // rewrites the pass PDF while the host reads it
                let v = layout.layout_version;
                let bg = s.cfg.build_dir.join("bg");
                let stable = bg.join(format!("layout-{v}.pdf"));
                match std::fs::copy(&cap.pdf, &stable) {
                    Ok(_) => {
                        layout.pdf = Some(stable.clone());
                        // the two previous copies stay: a host may still be reading the layout
                        // before the one it is replacing
                        if v >= 3 {
                            let _ = std::fs::remove_file(bg.join(format!("layout-{}.pdf", v - 3)));
                        }
                        // build/bg/<jobname>.pdf and .log: the latest layout, for hosts that
                        // name these files themselves (a link to the copy; nothing writes
                        // them in place)
                        let main_pdf = bg.join(format!("{}.pdf", cap.jobname));
                        let _ = std::fs::remove_file(&main_pdf);
                        if std::fs::hard_link(&stable, &main_pdf).is_err() {
                            let _ = std::fs::copy(&stable, &main_pdf);
                        }
                        let _ = std::fs::copy(&cap.log, bg.join(format!("{}.log", cap.jobname)));
                    }
                    Err(e) => log::warn!("layout PDF copy: {e}"),
                }
                c
            }
            Err(e) => {
                drop(layout);
                drop(files);
                layout_failed(s, t0, rev, format!("install layout: {e}"));
                return;
            }
        };
        let versions = Versions {
            source_revision: s.source_revision.load(Ordering::SeqCst),
            context_revision: layout.context_revision,
            engine_generation: s.engine_generation.load(Ordering::SeqCst),
            layout_version: layout.layout_version,
        };
        let mut placements = Vec::new();
        let mut eligible = Vec::new();
        // units whose vocabulary the allow-list fully knows (safe to compile unprobed)
        let mut eligible_strict: Vec<ParaId> = Vec::new();
        let policy = s.policy.lock().clone();
        for (id, idx) in &layout.by_span {
            let eu = &layout.units[*idx];
            let c = &eu.captured;
            let rows: Vec<(i64, i64)> = c.placements.iter().map(|p| (p.x, p.y)).collect();
            let kind = match c.kind.as_str() {
                "par" => "par".to_string(),
                k => format!("{k}:{}", c.name.clone().unwrap_or_default()),
            };
            if let Some((frags, _)) = layout.fragments(*id, &rows) {
                placements.push(ParagraphPlacement {
                    par_id: *id,
                    fragments: frags,
                    lines: eu.rows(),
                    kind,
                });
            }
            let has_ctx = eu.has_context();
            // eligible = capture facts clean AND the span's source passes the allow-list with the
            // shape the capture saw (what apply_edit will decide for a one-character edit)
            let source_ok = layout
                .snapshot_spans
                .iter()
                .find(|sp| sp.id == *id)
                .map(|sp| !sp.background_only)
                .unwrap_or(false)
                && files
                    .values()
                    .find_map(|fb| fb.span_text(*id))
                    .map(|text| {
                        let (shape, _) = classify_source(text, &policy);
                        match (&shape, c.kind.as_str()) {
                            (UnitShape::Par, "par") => true,
                            (UnitShape::Env(n), "env") => Some(n.as_str()) == c.name.as_deref(),
                            (UnitShape::Heading(n), "heading") => {
                                Some(n.as_str()) == c.name.as_deref()
                            }
                            _ => false,
                        }
                    })
                    .unwrap_or(false);
            if source_ok
                && check_engine_unit(&c.kind, &c.everypar, has_ctx, eu.rows(), &eu.flags).is_empty()
            {
                eligible.push(*id);
                let strict_ok = files
                    .values()
                    .find_map(|fb| fb.span_text(*id))
                    .map(|text| classify_source_with(text, &policy, false).1.is_empty())
                    .unwrap_or(false);
                if strict_ok {
                    eligible_strict.push(*id);
                }
            }
        }
        drop(files);
        placements.sort_by_key(|p| p.par_id);
        eligible.sort();
        eligible_strict.sort();
        (
            changed,
            versions,
            placements,
            eligible,
            eligible_strict,
            layout.pdf.clone(),
        )
    };
    // warm the engine: compile the first allow-listed paragraph once so fonts are loaded before
    // the first real keystroke (never an unprobed unit: it could leak state)
    if let Some(id) = eligible_strict.first().copied() {
        let files = s.files.lock();
        let layout = s.layout.lock();
        if let (Some(eu), Some(text)) = (
            layout.unit(id),
            files.values().find_map(|fb| fb.span_text(id)),
        ) {
            let mut p = s.pending.lock();
            if !p.contains_key(&id) {
                // exercise the font variants and math a body paragraph commonly needs so their
                // font instances are loaded before the first real keystroke
                let warm = format!("{} \\emph{{warm}} \\textbf{{warm}} \\textit{{warm}} \\textsc{{warm}} {{\\small warm}} $x^2_i + \\alpha \\sum \\frac{{1}}{{2}} \\mathbf{{v}}$", text.trim_end_matches('\n'));
                p.insert(
                    id,
                    FastRequest {
                        warmup: true,
                        par_id: id,
                        edit_id: 0,
                        span_hash: 0,
                        source: warm,
                        pics: String::new(),
                        pics_n: 0,
                        seq: eu.uid,
                        ctx: None,
                        versions: Versions {
                            source_revision: rev,
                            context_revision: layout.context_revision,
                            engine_generation: s.engine_generation.load(Ordering::SeqCst),
                            layout_version: layout.layout_version,
                        },
                        context_stale: false,
                        expected_rows: 0,
                        probe: false,
                        pics_required: false,
                    },
                );
                s.pending_signal.0.send(()).ok();
            }
        }
    }
    // commit overlays older than the snapshot; borrowed contexts and live row counts are
    // superseded by the new placements
    s.overlays.lock().retain(|_, r| *r > rev);
    s.derived.lock().clear();
    s.live_rows.lock().clear();
    s.live_place.lock().clear();
    s.counters_seen.lock().clear();
    let current = s.source_revision.load(Ordering::SeqCst);
    let mut reasons = Vec::new();
    if !aux_stable {
        reasons.push("aux family still changing".into());
    }
    if errors > 0 {
        reasons.push(format!("{errors} compile errors"));
    }
    let convergence = if current > rev {
        Convergence::Stale {
            pending_since: rev + 1,
        }
    } else if provisional {
        Convergence::Converging {
            pass: passes,
            reasons: vec!["another pass is running".into()],
        }
    } else if aux_stable && errors == 0 {
        Convergence::Converged
    } else if !aux_stable && passes >= s.cfg.max_passes {
        Convergence::PassLimitReached {
            passes: passes,
            reasons: reasons.clone(),
        }
    } else {
        Convergence::Converging {
            pass: passes,
            reasons: reasons.clone(),
        }
    };
    *s.convergence.lock() = Some(convergence.clone());
    let pages_changed: Vec<PageUpdate> = {
        let layout = s.layout.lock();
        changed
            .iter()
            .filter_map(|n| {
                layout.pages.get(n).map(|dl| PageUpdate {
                    page: *n,
                    exact: dl.is_exact(),
                    hash: layout.page_hashes[n],
                    dl: dl.clone(),
                })
            })
            .collect()
    };
    let pages_total = cap.json.pages;
    if !diagnostics.is_empty() {
        s.events
            .send(Event::Diagnostics {
                source: "background".into(),
                items: diagnostics,
            })
            .ok();
    }
    // the fallback is named whenever the layout has a degraded page, changed in this
    // layout or not: a host that keeps one PDF per layout needs the current file
    let any_degraded = {
        let layout = s.layout.lock();
        layout.pages.values().any(|dl| !dl.is_exact())
    };
    s.events
        .send(Event::LayoutUpdate {
            versions,
            compile,
            convergence: convergence.clone(),
            passes: passes,
            pages_changed,
            pages_total,
            placements,
            eligible_paragraphs: eligible,
            pdf_fallback: if any_degraded { pdf } else { None },
            wall_ms: t0.elapsed().as_millis() as u64,
        })
        .ok();
    if matches!(convergence, Convergence::Stale { .. }) {
        s.bg_signal.0.send(BgCmd::Pass).ok();
    }
}

pub(super) fn run_export(s: &Shared, job: u64, out: PathBuf) {
    let (texts, _spans, _rev) = snapshot(s);
    let snap_dir = s.cfg.build_dir.join("export-src");
    if let Err(e) = write_snapshot(&s.cfg.project_root, &texts, &snap_dir) {
        s.events
            .send(Event::PdfExported {
                job_id: job,
                path: None,
                status: CompileStatus::Failed,
                converged: false,
                passes: 0,
            })
            .ok();
        let _ = e;
        return;
    }
    let out_dir = s.cfg.build_dir.join("export");
    match run_pass_with(
        &s.tl,
        &snap_dir,
        &s.cfg.main_file,
        &out_dir,
        s.cfg.max_passes,
        s.cfg.bib_tool,
        false,
        "",
    ) {
        Ok(o) => {
            let diags = parse_log(&o.capture.log);
            let errors = diags.iter().filter(|d| d.severity == "error").count();
            let status = if !o.capture.pdf.exists() {
                CompileStatus::Failed
            } else if errors > 0 {
                CompileStatus::CompiledWithErrors { count: errors }
            } else {
                CompileStatus::Ok
            };
            let path = if o.capture.pdf.exists() {
                if let Some(parent) = out.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::copy(&o.capture.pdf, &out)
                    .ok()
                    .map(|_| out.clone())
            } else {
                None
            };
            s.events
                .send(Event::PdfExported {
                    job_id: job,
                    path,
                    status,
                    converged: o.aux_stable && errors == 0,
                    passes: o.passes,
                })
                .ok();
        }
        Err(_) => {
            s.events
                .send(Event::PdfExported {
                    job_id: job,
                    path: None,
                    status: CompileStatus::Failed,
                    converged: false,
                    passes: 0,
                })
                .ok();
        }
    }
}
