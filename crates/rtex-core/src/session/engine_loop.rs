//! The engine thread: owns the fast server, sends queued requests, collects results (direct
//! or queued), runs probes and warm-ups, and turns results into paragraph updates or demotions.

use super::*;

// ---------------------------------------------------------------------------------------------
// Engine thread: owns the fast server; coalesces pending requests (latest per paragraph).
// ---------------------------------------------------------------------------------------------
pub(super) fn engine_thread(s: Arc<Shared>) {
    let mut server: Option<FastServer> = None;
    let mut server_generation: u64 = u64::MAX;
    let mut labels_sent: u64 = 0;
    let reset_link = |s: &Shared| {
        let mut l = s.link.lock();
        l.writer = None;
        l.inflight = None;
        l.contexts_sent.clear();
    };
    loop {
        if s.shutdown.load(Ordering::SeqCst) {
            reset_link(&s);
            if let Some(mut srv) = server.take() {
                let _ = srv.shutdown();
            }
            return;
        }
        let wanted_gen = s.engine_generation.load(Ordering::SeqCst);
        // 1. a compile is in flight (sent by us or directly by the host): read its result
        let inflight_gen = s
            .link
            .lock()
            .inflight
            .as_ref()
            .map(|f| f.req.versions.engine_generation);
        if let Some(gen) = inflight_gen {
            let srv_ok = server.is_some() && server_generation == gen && gen == wanted_gen;
            if !srv_ok {
                s.link.lock().inflight = None;
                continue;
            }
            let srv = server.as_mut().unwrap();
            let result = srv.recv();
            srv.timeout = compile_timeout(&s.cfg);
            let Some(fl) = s.link.lock().inflight.take() else {
                continue;
            };
            match result {
                Ok(Response::Result(cr)) => {
                    let rt = crate::engine::RoundTrip {
                        total: fl.t0.elapsed(),
                        t_tex: Duration::from_micros(cr.t_tex_us as u64),
                        t_traverse: Duration::from_micros(cr.t_traverse_us as u64),
                        t_pack: Duration::from_micros(cr.t_pack_us as u64),
                    };
                    handle_result(&s, fl.req, cr, rt, fl.t0, wanted_gen);
                }
                Ok(Response::Fatal {
                    reason,
                    errors,
                    before,
                    after,
                    ..
                }) => {
                    engine_failed(
                        &s,
                        &fl.req,
                        anyhow!(
                            "engine fatal: {reason} {errors:?} before=[{before}] after=[{after}]"
                        ),
                        wanted_gen,
                        server.as_ref(),
                    );
                    server = None;
                    reset_link(&s);
                }
                Ok(other) => {
                    engine_failed(
                        &s,
                        &fl.req,
                        anyhow!("unexpected response {other:?}"),
                        wanted_gen,
                        server.as_ref(),
                    );
                    server = None;
                    reset_link(&s);
                }
                Err(e) => {
                    engine_failed(&s, &fl.req, e, wanted_gen, server.as_ref());
                    server = None;
                    reset_link(&s);
                }
            }
            continue;
        }
        // 2. next queued request (the smallest unit id)
        let req = {
            let mut p = s.pending.lock();
            let key = p.keys().next().copied();
            key.and_then(|k| p.remove(&k))
        };
        if req.is_none()
            && server.is_some()
            && server_generation == wanted_gen
            && server.as_mut().unwrap().is_alive()
        {
            let _ = s.pending_signal.1.recv_timeout(Duration::from_millis(200));
            continue;
        }
        if let Some(r) = &req {
            if r.versions.engine_generation != wanted_gen {
                continue; // superseded by a preamble change
            }
        }
        // 3. (re)start the server when the generation changed or it died. A preamble edit bumps
        //    the generation per keystroke; wait until it has been quiet for the debounce time so
        //    a burst of preamble keystrokes costs one restart, not one per keystroke.
        if server.is_none()
            || server_generation != wanted_gen
            || !server.as_mut().unwrap().is_alive()
        {
            if server.is_some() && server_generation != wanted_gen {
                let mut g = wanted_gen;
                loop {
                    let _ = s.pending_signal.1.recv_timeout(s.cfg.debounce);
                    let now = s.engine_generation.load(Ordering::SeqCst);
                    if now == g || s.shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    g = now;
                }
                if g != wanted_gen {
                    continue; // re-evaluate with the settled generation
                }
            }
            reset_link(&s);
            if let Some(mut old) = server.take() {
                old.kill();
            }
            let preamble = effective_preamble(&texts_of(&s.files.lock()), &s.cfg.main_file);
            s.events
                .send(Event::EngineState {
                    engine_generation: wanted_gen,
                    state: "Starting".into(),
                    reason: None,
                })
                .ok();
            let aux = {
                let layout = s.layout.lock();
                let jobname = Path::new(&s.cfg.main_file)
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or("main")
                    .to_string();
                layout
                    .capture_dir
                    .as_ref()
                    .map(|d| d.join(format!("{jobname}.aux")))
            };
            match FastServer::spawn_with(
                &s.tl,
                &s.cfg.project_root,
                &s.cfg.build_dir.join("serve"),
                &preamble,
                wanted_gen,
                aux.as_deref(),
                s.cfg.debug_dir.is_some(),
            ) {
                Ok(mut srv) => {
                    srv.timeout = compile_timeout(&s.cfg).max(Duration::from_secs(30)); // first compile loads fonts
                    {
                        let mut l = s.link.lock();
                        l.writer = srv.stdin_clone().ok();
                        l.generation = wanted_gen;
                        l.contexts_sent.clear();
                        l.inflight = None;
                    }
                    server = Some(srv);
                    server_generation = wanted_gen;
                    labels_sent = 0;
                    s.events
                        .send(Event::EngineState {
                            engine_generation: wanted_gen,
                            state: "Ready".into(),
                            reason: None,
                        })
                        .ok();
                }
                Err(e) => {
                    let reason = debug_bundle_startup(&s, wanted_gen, &format!("{e:#}"));
                    s.events
                        .send(Event::EngineState {
                            engine_generation: wanted_gen,
                            state: "Failed".into(),
                            reason: Some(reason),
                        })
                        .ok();
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }
        let srv = server.as_mut().unwrap();
        // 3b. labels (\newlabel/\bibcite of the last pass) when they changed; done while idle
        // when possible, and before a compile otherwise
        let (labels, labels_hash) = {
            let layout = s.layout.lock();
            (layout.labels.clone(), layout.labels_hash)
        };
        if labels_hash != labels_sent && !labels.is_empty() {
            let mut link = s.link.lock();
            if link.inflight.is_none() {
                match srv.set_labels(&labels) {
                    Ok(()) => labels_sent = labels_hash,
                    Err(e) => {
                        drop(link);
                        s.events
                            .send(Event::EngineState {
                                engine_generation: wanted_gen,
                                state: "Restarting".into(),
                                reason: Some(format!("labels: {e}")),
                            })
                            .ok();
                        server = None;
                        reset_link(&s);
                        continue;
                    }
                }
                link.labels_sent = labels_sent;
            }
        }
        let Some(mut req) = req else { continue };
        // 4. send the context when the server does not hold it, then the compile frame
        let mut link = s.link.lock();
        if link.context_rev_sent != req.versions.context_revision {
            link.contexts_sent.clear();
            link.context_rev_sent = req.versions.context_revision;
        }
        if !link.contexts_sent.contains(&req.seq) {
            let ctx = match req.ctx.take() {
                Some(c) => Some(c),
                None => {
                    // built lazily from the current layout; the unit may have a new id there
                    let layout = s.layout.lock();
                    layout.unit(req.par_id).map(|eu| {
                        req.seq = eu.uid;
                        eu.context_json()
                    })
                }
            };
            let Some(ctx) = ctx else {
                drop(link);
                s.events
                    .send(Event::BackgroundScheduled {
                        par_id: Some(req.par_id),
                        reasons: vec!["context no longer available".into()],
                        edit_id: req.edit_id,
                    })
                    .ok();
                continue;
            };
            if link.contexts_sent.contains(&req.seq) {
                // the lazy lookup mapped to a context already installed
            } else if let Err(e) = srv.set_context(req.seq, &ctx) {
                drop(link);
                s.events
                    .send(Event::EngineState {
                        engine_generation: wanted_gen,
                        state: "Restarting".into(),
                        reason: Some(format!("set_context: {e}")),
                    })
                    .ok();
                server = None;
                reset_link(&s);
                continue;
            } else {
                link.contexts_sent.insert(req.seq);
            }
        }
        // 4b. probe mode: prove the unit first — its snapshot text (from the layout current
        // now) compiled and compared with that layout's rows. The compile runs without the
        // link lock (the host's apply_edit must not wait on it); `probing` keeps the direct
        // dispatch path off the server meanwhile.
        if req.probe {
            let (text, lv) = {
                let layout = s.layout.lock();
                (
                    layout.snapshot_text(req.par_id).map(|t| t.to_string()),
                    layout.layout_version,
                )
            };
            let cached = s
                .probe
                .lock()
                .get(&req.par_id)
                .filter(|(v, _)| *v == lv)
                .map(|(_, v)| v.clone());
            let verdict = match cached {
                Some(v) => v,
                None => {
                    let Some(text) = text else {
                        drop(link);
                        demote_span(&s, &req, "no snapshot to compare with");
                        continue;
                    };
                    link.probing = true;
                    drop(link);
                    let t_probe = Instant::now();
                    let res = srv.compile_with_pics(req.seq, &text, &req.pics);
                    let mut relock = s.link.lock();
                    relock.probing = false;
                    relock.probes += 1;
                    relock.probe_us += t_probe.elapsed().as_micros() as u64;
                    link = relock;
                    let v = match res {
                        Ok((cr, _))
                            if req.pics_n > 0
                                && cr.pics_seen.is_some_and(|n| n != req.pics_n as i64) =>
                        {
                            // the cache entries were scanned from the current text, the probe
                            // compiles the snapshot's: when they do not pair up the probe is
                            // repeated without them (the pictures are drawn)
                            let mut req = req;
                            req.pics.clear();
                            req.pics_n = 0;
                            drop(link);
                            s.pending.lock().insert(req.par_id, req);
                            s.pending_signal.0.send(()).ok();
                            continue;
                        }
                        Ok((cr, _)) if cr.internal.is_some() => ProbeVerdict::Mismatch(format!(
                            "internal error ({})",
                            first_line_of(cr.internal.as_deref().unwrap_or_default())
                        )),
                        Ok((cr, _)) => {
                            if !cr.leaks.is_empty() {
                                ProbeVerdict::Leak(format!(
                                    "the unit redefines \\{}",
                                    cr.leaks.join(", \\")
                                ))
                            } else if cr.status == "error" {
                                ProbeVerdict::Mismatch(format!(
                                    "probe compile failed: {}",
                                    cr.errors
                                        .first()
                                        .and_then(|e| e.message.clone())
                                        .unwrap_or_default()
                                ))
                            } else {
                                let layout = s.layout.lock();
                                if layout.layout_version != lv {
                                    // the layout moved while the probe ran: judge against the
                                    // new one (the request goes back to the queue)
                                    drop(layout);
                                    drop(link);
                                    s.pending.lock().insert(req.par_id, req);
                                    s.pending_signal.0.send(()).ok();
                                    continue;
                                }
                                match &cr.dl {
                                    Some(dl) => match layout.probe_check(req.par_id, dl) {
                                        Ok(()) => ProbeVerdict::Verified,
                                        Err(why) => ProbeVerdict::Mismatch(why),
                                    },
                                    None => ProbeVerdict::Mismatch("probe produced no box".into()),
                                }
                            }
                        }
                        Err(e) => {
                            drop(link);
                            engine_failed(&s, &req, e, wanted_gen, server.as_ref());
                            server = None;
                            reset_link(&s);
                            continue;
                        }
                    };
                    s.probe.lock().insert(req.par_id, (lv, v.clone()));
                    v
                }
            };
            match verdict {
                ProbeVerdict::Verified => {}
                ProbeVerdict::Mismatch(why) => {
                    drop(link);
                    demote_span(&s, &req, &why);
                    continue;
                }
                ProbeVerdict::Leak(why) => {
                    drop(link);
                    s.leaky.lock().insert(req.par_id, why.clone());
                    demote_span(&s, &req, &why);
                    // the server's state is no longer the document's: start over
                    s.engine_generation.fetch_add(1, Ordering::SeqCst);
                    server = None;
                    reset_link(&s);
                    continue;
                }
            }
        }
        let req_id = link.next_req;
        link.next_req += 1;
        let t0 = Instant::now();
        match srv.send_compile(req_id, req.seq, &req.source, &req.pics) {
            Ok(()) => {
                link.inflight = Some(InFlight { req, t0 });
            }
            Err(e) => {
                drop(link);
                engine_failed(&s, &req, e, wanted_gen, server.as_ref());
                server = None;
                reset_link(&s);
            }
        }
    }
}

/// A fast request the probe (or a leak) took away from the live path after `apply_edit`
/// reported it as fast: undo the live bookkeeping (no overlay claims a result at this
/// revision; the change counts as a background change for the spans after it), tell the host,
/// and schedule the pass that will typeset it.
pub(super) fn demote_span(s: &Shared, req: &FastRequest, why: &str) {
    s.overlays.lock().remove(&req.par_id);
    {
        let files = s.files.lock();
        if let Some((name, line)) = files.iter().find_map(|(name, fb)| {
            fb.span(req.par_id)
                .map(|sp| (name.clone(), fb.line_range(sp).0))
        }) {
            s.bg_change
                .lock()
                .entry(name)
                .or_default()
                .push((req.versions.source_revision, line));
        }
    }
    if !req.warmup {
        s.events
            .send(Event::BackgroundScheduled {
                par_id: Some(req.par_id),
                reasons: vec![format!("unverified: {why}")],
                edit_id: req.edit_id,
            })
            .ok();
    }
    s.bg_signal.0.send(BgCmd::Pass).ok();
}

pub(super) fn engine_failed(
    s: &Shared,
    req: &FastRequest,
    e: anyhow::Error,
    wanted_gen: u64,
    srv: Option<&FastServer>,
) {
    let mut message = e.to_string();
    if let Some(dir) = debug_bundle(s, req, &message, wanted_gen, srv) {
        message = format!("{message} (debug bundle: {})", dir.display());
    }
    s.events
        .send(Event::EngineState {
            engine_generation: wanted_gen,
            state: "Restarting".into(),
            reason: Some(message.clone()),
        })
        .ok();
    s.events
        .send(Event::Diagnostics {
            source: format!("fast:{:?}", req.par_id),
            items: vec![Diagnostic {
                severity: "error".into(),
                file: None,
                line: None,
                message,
                context: None,
            }],
        })
        .ok();
    s.engine_generation.fetch_add(1, Ordering::SeqCst);
    // the paragraph goes to the background path, and stays there until the preamble changes:
    // retrying a unit that hung or crashed the engine would kill the server on every keystroke
    s.bg_signal.0.send(BgCmd::Pass).ok();
    if !req.warmup {
        s.slow_units.lock().insert(req.par_id, (QUARANTINED, 0));
        s.events
            .send(Event::BackgroundScheduled {
                par_id: Some(req.par_id),
                reasons: vec!["engine restarted".into()],
                edit_id: req.edit_id,
            })
            .ok();
    }
}

/// `slow_units` layout-version marker for a unit quarantined after an engine failure.
pub(super) const QUARANTINED: u64 = u64::MAX;

/// The per-compile watchdog. With macro tracing on (`$RTEX_TRACE_MACROS` with a debug
/// directory) every macro expansion is written to the TeX log, which makes a heavy compile (a
/// pgfplots axis drawn) many times slower: the watchdog is lengthened rather than letting the
/// debug setting itself kill the engine.
pub(super) fn compile_timeout(cfg: &SessionConfig) -> Duration {
    if cfg.debug_dir.is_some() && std::env::var_os("RTEX_TRACE_MACROS").is_some_and(|v| v != "0") {
        cfg.compile_timeout * 10
    } else {
        cfg.compile_timeout
    }
}

pub(super) fn handle_result(
    s: &Shared,
    req: FastRequest,
    cr: crate::engine::CompileResult,
    rt: crate::engine::RoundTrip,
    t0: Instant,
    wanted_gen: u64,
) {
    if let Some(msg) = cr.internal.as_deref() {
        // the server hit a Lua error of its own on this unit and answered with it (it used to
        // stay silent until the watchdog killed the engine): the unit waits for the pass until
        // the next layout; the engine is fine
        if req.warmup {
            // nothing to demote, and no pass to schedule: a pass would install a layout, which
            // warms the engine again, which would fail again (a loop of passes)
            log::warn!("warm-up compile: internal error in the server: {msg}");
            return;
        }
        let lv = s.layout.lock().layout_version;
        let first = s
            .internal_errors
            .lock()
            .insert(req.par_id, (lv, msg.to_string()))
            .is_none_or(|(prev, _)| prev != lv);
        log::warn!("{:?}: internal error in the server: {msg}", req.par_id);
        // one bundle per unit and layout, not one per keystroke
        if first {
            if let Some(dir) =
                debug_bundle(s, &req, &format!("internal error: {msg}"), wanted_gen, None)
            {
                log::warn!("debug bundle: {}", dir.display());
            }
        }
        demote_span(s, &req, &format!("internal error ({})", first_line_of(msg)));
        return;
    }
    if !cr.leaks.is_empty() {
        // before any early return: the compile changed the meaning of a control sequence it
        // mentions, so the server is no longer the document's state whatever else happened.
        // Demote the span until the preamble changes and restart the engine.
        let why = format!("the unit redefines \\{}", cr.leaks.join(", \\"));
        s.leaky.lock().insert(req.par_id, why.clone());
        demote_span(s, &req, &why);
        s.engine_generation.fetch_add(1, Ordering::SeqCst);
        s.pending_signal.0.send(()).ok();
        return;
    }
    if s.cfg.debug_dir.is_some() {
        debug_request_line(
            s,
            &format!(
                "par {} ctx {} status {} rows {} tex_us {} total_us {} pics {}/{} errors {}{}",
                req.par_id.0,
                req.seq,
                cr.status,
                cr.dl.as_ref().map(|d| d.lines.len()).unwrap_or(0),
                rt.t_tex.as_micros(),
                t0.elapsed().as_micros(),
                cr.pics_used.unwrap_or(0),
                cr.pics_seen.unwrap_or(0),
                cr.errors.len(),
                if req.warmup { " (warm-up)" } else { "" }
            ),
        );
    }
    if req.warmup {
        return;
    }
    // a unit let through unprobed (a borrowed context, or a probe skipped) because every
    // picture in it comes from the cache: when the engine drew one after all (a state
    // mismatch, a picture the scan did not see), the result is unverified and the unit waits
    // for the pass
    if req.pics_required && cr.status != "error" && cr.pics_used != Some(req.pics_n as i64) {
        demote_span(
            s,
            &req,
            "a picture drawn outside the cache on a borrowed context",
        );
        return;
    }
    // the engine saw a different number of picture environments than the source scan (a
    // picture made by a macro, a nested one): its cache entries may have gone to the wrong
    // pictures, so the result is dropped and the compile repeated without them
    if req.pics_n > 0 && cr.pics_seen.is_some_and(|n| n != req.pics_n as i64) {
        log::warn!(
            "{:?}: {} pictures scanned, engine saw {:?}; compiling without the cache",
            req.par_id,
            req.pics_n,
            cr.pics_seen
        );
        let mut again = req;
        again.pics.clear();
        again.pics_n = 0;
        let mut pending = s.pending.lock();
        pending.entry(again.par_id).or_insert(again);
        drop(pending);
        s.pending_signal.0.send(()).ok();
        return;
    }
    // discard rule: the span changed meanwhile, or the generation moved on
    let current_hash = s
        .files
        .lock()
        .values()
        .find_map(|fb| fb.span(req.par_id).map(|sp| sp.hash));
    if current_hash != Some(req.span_hash)
        || s.engine_generation.load(Ordering::SeqCst) != wanted_gen
    {
        return;
    }
    let diagnostics: Vec<Diagnostic> = cr
        .errors
        .iter()
        .map(|e| Diagnostic {
            severity: "error".into(),
            file: None,
            line: e.line.map(|l| l - 1), // line 1 is the replay head
            message: e.message.clone().unwrap_or_default(),
            context: e.context.clone(),
        })
        .collect();
    let timing = Timing {
        total_us: t0.elapsed().as_micros() as u64,
        tex_us: rt.t_tex.as_micros() as u64,
        traverse_us: rt.t_traverse.as_micros() as u64,
        pack_us: rt.t_pack.as_micros() as u64,
    };
    if cr.status == "error" || cr.dl.is_none() {
        s.events
            .send(Event::ParagraphUpdate {
                par_id: req.par_id,
                edit_id: req.edit_id,
                versions: req.versions,
                status: "error".into(),
                reasons: vec![],
                fragments: vec![],
                pagination_stale: false,
                context_stale: req.context_stale,
                dl: cr.dl.unwrap_or_default(),
                diagnostics,
                timing,
            })
            .ok();
        return;
    }
    let mut dl = cr.dl.unwrap();
    if !cr.images.is_empty() {
        dl.images = cr.images.clone();
    }
    let rows: Vec<(i64, i64)> = dl.lines.iter().map(|l| (l.x, l.y)).collect();
    let (fragments, mut stale) = {
        let layout = s.layout.lock();
        match layout.fragments(req.par_id, &rows) {
            Some(x) => x,
            None => {
                // a borrowed context: placed relative to the anchor's rows (a layout unit's
                // placements, or the live placement of a unit on a borrowed context itself)
                let derived = s.derived.lock().get(&req.par_id).copied();
                let frags = derived
                    .and_then(|(parent, anchor, after)| {
                        let placed = s.live_place.lock().get(&anchor).copied();
                        match placed.filter(|_| layout.unit(anchor).is_none()) {
                            Some((page, x, last)) => {
                                let bs = layout.unit(parent)?.baselineskip().max(1);
                                Some(LayoutStore::fragments_at(page, x, last + bs, &rows))
                            }
                            None => {
                                // a layout unit, or a chained unit not delivered (yet)
                                let a = if layout.unit(anchor).is_some() {
                                    anchor
                                } else {
                                    parent
                                };
                                let live = s.live_rows.lock().get(&a).copied();
                                layout.fragments_relative(a, live, after, &rows)
                            }
                        }
                    })
                    .unwrap_or_default();
                (frags, true)
            }
        }
    };
    // a placement that lands off its page is a bad guess (a borrowed anchor on another page,
    // a unit grown past the page bottom, rows extrapolated above the top): drop it so the unit
    // waits for the pass instead of drawing its rows, pictures and all, at the page edge or
    // past it (two graphs, a plot pinned to the page top)
    if !fragments.is_empty() && !s.layout.lock().fragments_on_page(&fragments) {
        demote_span(s, &req, "the placement would fall off its page");
        return;
    }
    // a box the host cannot place (no placement for the unit, no neighbour to place it
    // against) is not a live result: the unit waits for the pass
    if fragments.is_empty() && !dl.lines.is_empty() {
        demote_span(s, &req, "no placement for the unit's rows");
        return;
    }
    if s.cfg.debug_dir.is_some() {
        let f = fragments.first();
        debug_request_line(
            s,
            &format!(
                "par {} placed: rows {} fragments {} page {:?} x {:?} y {:?}..{:?} approximate {:?}",
                req.par_id.0,
                dl.lines.len(),
                fragments.len(),
                f.map(|f| f.page),
                f.and_then(|f| f.xs.first().copied()),
                f.and_then(|f| f.baselines.first().copied()),
                fragments.last().and_then(|f| f.baselines.last().copied()),
                f.map(|f| f.approximate),
            ),
        );
    }
    s.live_rows.lock().insert(req.par_id, dl.lines.len() as i64);
    if let Some(f) = fragments.last() {
        if let (Some(x), Some(last)) = (f.xs.first(), f.baselines.last()) {
            s.live_place.lock().insert(req.par_id, (f.page, *x, *last));
        }
    }
    if req.expected_rows != dl.lines.len() as i64 {
        stale = true;
    }
    let mut reasons: Vec<String> = dl.flags_map().keys().cloned().collect();
    if dl.inserts > 0 {
        // footnote/margin text is placed by the page builder: refreshed by the next layout
        reasons.push("inserts".into());
    }
    // counters: a unit that now advances other counters, or to other values, than the layout
    // saw (an added equation, item, footnote or \stepcounter) renumbers what follows: pass
    let counters_changed = {
        let expected = s
            .layout
            .lock()
            .unit(req.par_id)
            .map(|u| u.advanced())
            .unwrap_or_default();
        let mut seen = s.counters_seen.lock();
        let prev = seen.get(&req.par_id).cloned().unwrap_or(expected);
        seen.insert(req.par_id, cr.counters.clone());
        prev != cr.counters
    };
    if stale || counters_changed || !reasons.is_empty() {
        s.bg_signal.0.send(BgCmd::Pass).ok();
    }
    let total_us = timing.total_us;
    s.events
        .send(Event::ParagraphUpdate {
            par_id: req.par_id,
            edit_id: req.edit_id,
            versions: req.versions,
            status: if reasons.is_empty() {
                "ok".into()
            } else {
                "ok_degraded".into()
            },
            reasons,
            fragments,
            pagination_stale: stale,
            context_stale: req.context_stale,
            dl,
            diagnostics,
            timing,
        })
        .ok();
    // fast budget: a unit whose compiles are too slow leaves the fast path until the next
    // layout. The first slow compiles of a unit are forgiven (font loading, cold caches); three
    // in a row mark the unit.
    const STRIKES: u32 = 3;
    if total_us > s.cfg.fast_budget.as_micros() as u64 {
        if s.bg_running.load(Ordering::SeqCst) {
            // a layout pass is using the CPU: a slow round trip now says nothing about the unit
            return;
        }
        let mut cand = s.slow_candidates.lock();
        let n = cand.entry(req.par_id).or_insert(0);
        *n += 1;
        if *n >= STRIKES {
            cand.remove(&req.par_id);
            s.slow_units
                .lock()
                .insert(req.par_id, (req.versions.layout_version, total_us / 1000));
            s.events
                .send(Event::BackgroundScheduled {
                    par_id: Some(req.par_id),
                    reasons: vec![reason_str(&Reason::OverBudget(total_us / 1000))],
                    edit_id: req.edit_id,
                })
                .ok();
        }
    } else {
        s.slow_candidates.lock().remove(&req.par_id);
    }
}
