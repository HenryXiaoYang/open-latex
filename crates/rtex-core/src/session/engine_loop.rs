//! The engine thread: owns the fast server, sends queued requests, collects results (direct
//! or queued), runs probes and warm-ups, and turns results into paragraph updates or demotions.

use super::*;

// ---------------------------------------------------------------------------------------------
// Engine thread: owns the fast server; coalesces pending requests (latest per paragraph).
// ---------------------------------------------------------------------------------------------
pub(super) fn engine_thread(s: Arc<Shared>) {
    EngineLoop {
        s,
        server: None,
        server_generation: u64::MAX,
        labels_sent: 0,
        server_bbl: None,
        standby: None,
        bbl_checked: u64::MAX,
        swapped_from: None,
    }
    .run();
}

/// The engine thread's own state: the server it owns and what it has sent to that server.
struct EngineLoop {
    s: Arc<Shared>,
    server: Option<FastServer>,
    server_generation: u64,
    labels_sent: u64,
    /// Hash of the `.bbl` the running server read at `\begin{document}` (None: it had none).
    /// biblatex reads its data once, so a newer one needs a new server (`refresh_bibliography`).
    server_bbl: Option<u64>,
    /// A server starting with a newer `.bbl` while the running one keeps serving.
    standby: Option<Standby>,
    /// The layout version whose `.bbl` was last compared with the server's.
    bbl_checked: u64,
    /// (replaced, current) generations of the last standby swap: requests queued for the
    /// replaced server are still valid (same preamble) and are carried over, as long as the
    /// generation is still the one the swap installed.
    swapped_from: Option<(u64, u64)>,
}

/// A server for the latest bibliography (`refresh_bibliography`): starting on a helper thread,
/// then ready and waiting for the running server to be idle.
struct Standby {
    generation: u64,
    state: StandbyState,
}

enum StandbyState {
    Starting(std::thread::JoinHandle<Result<FastServer>>),
    Ready(FastServer),
}

/// Hash of the `.bbl` next to `aux` (None: there is none).
fn bbl_hash(aux: Option<&Path>) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(aux?.with_extension("bbl")).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

/// Hash of the `.bbl` a started server read: the copy it made at start, not the pass's file,
/// which a later pass in the same directory may already have rewritten.
fn server_bbl_hash(srv: &FastServer) -> Option<u64> {
    bbl_hash(Some(
        &srv.work_dir
            .join(format!("rtex-serve-g{}.aux", srv.generation)),
    ))
}

/// The aux file of the latest layout's pass (a server starts from it and its `.bbl`), with
/// that layout's version, read together.
fn layout_aux(s: &Shared) -> (Option<PathBuf>, u64) {
    let layout = s.layout.lock();
    let jobname = Path::new(&s.cfg.main_file)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("main")
        .to_string();
    (
        layout
            .capture_dir
            .as_ref()
            .map(|d| d.join(format!("{jobname}.aux"))),
        layout.layout_version,
    )
}

/// The link fields that describe the current server, cleared when it goes away.
fn reset_link(s: &Shared) {
    let mut l = s.link.lock();
    l.writer = None;
    l.inflight = None;
    l.contexts_sent.clear();
}

/// What the probe of a unit decided for this iteration.
enum ProbeOutcome {
    /// Proven (now or earlier for this layout): send the compile.
    Send(FastRequest),
    /// Handled (demoted, requeued, or the engine failed): next iteration.
    Done,
}

impl EngineLoop {
    /// One iteration per request or event: 1. collect a result in flight; 2. take the next
    /// queued request; 3. (re)start the server; 3b. send labels; 4. send the context; 4b. probe
    /// (probe mode); 5. send the compile. A step that settles the iteration returns false (or
    /// nothing to go on with) and the loop starts over.
    fn run(mut self) {
        loop {
            if self.s.shutdown.load(Ordering::SeqCst) {
                reset_link(&self.s);
                if let Some(mut srv) = self.server.take() {
                    let _ = srv.shutdown();
                }
                return;
            }
            let wanted_gen = self.s.engine_generation.load(Ordering::SeqCst);
            if self.collect_inflight(wanted_gen) {
                continue;
            }
            if self.refresh_bibliography(wanted_gen) {
                continue;
            }
            let Some(req) = self.next_request(wanted_gen) else {
                continue;
            };
            if !self.ensure_server(wanted_gen) || !self.sync_labels(wanted_gen) {
                continue;
            }
            let Some(req) = req else { continue };
            self.serve(req, wanted_gen);
        }
    }

    /// Drop the server (its process is killed) and the link's view of it.
    fn drop_server(&mut self) {
        self.server = None;
        reset_link(&self.s);
    }

    /// Step 1: A compile is in flight (sent by us or directly by the host): read its result. True
    /// when there was one (the iteration is settled).
    fn collect_inflight(&mut self, wanted_gen: u64) -> bool {
        let s = self.s.clone();
        let inflight_gen = s
            .link
            .lock()
            .inflight
            .as_ref()
            .map(|f| f.req.versions.engine_generation);
        let Some(gen) = inflight_gen else {
            return false;
        };
        let srv_ok = self.server.is_some() && self.server_generation == gen && gen == wanted_gen;
        if !srv_ok {
            s.link.lock().inflight = None;
            return true;
        }
        let srv = self.server.as_mut().unwrap();
        let result = srv.recv();
        srv.timeout = compile_timeout(&s.cfg);
        let Some(fl) = s.link.lock().inflight.take() else {
            return true;
        };
        let failure = match result {
            Ok(Response::Result(cr)) => {
                let rt = crate::engine::RoundTrip {
                    total: fl.t0.elapsed(),
                    t_tex: Duration::from_micros(cr.t_tex_us as u64),
                    t_traverse: Duration::from_micros(cr.t_traverse_us as u64),
                    t_pack: Duration::from_micros(cr.t_pack_us as u64),
                };
                handle_result(&s, fl.req, *cr, rt, fl.t0, wanted_gen);
                return true;
            }
            Ok(Response::Fatal {
                reason,
                errors,
                before,
                after,
                ..
            }) => anyhow!("engine fatal: {reason} {errors:?} before=[{before}] after=[{after}]"),
            Ok(other) => anyhow!("unexpected response {other:?}"),
            Err(e) => e,
        };
        engine_failed(&s, &fl.req, failure, wanted_gen, self.server.as_ref());
        self.drop_server();
        true
    }

    /// Step 2: The next queued request (the smallest unit id). None: the iteration is settled
    /// (nothing to do and the server is fine: waited for a signal; or the request was
    /// superseded). Some(None): nothing queued but the server needs attention.
    fn next_request(&mut self, wanted_gen: u64) -> Option<Option<FastRequest>> {
        let s = &self.s;
        let req = {
            let mut p = s.pending.lock();
            let key = p.keys().next().copied();
            key.and_then(|k| p.remove(&k))
        };
        if req.is_none()
            && self.server.is_some()
            && self.server_generation == wanted_gen
            && self.server.as_mut().unwrap().is_alive()
        {
            let _ = s.pending_signal.1.recv_timeout(Duration::from_millis(200));
            return None;
        }
        let mut req = req;
        if let Some(r) = &mut req {
            if self.swapped_from == Some((r.versions.engine_generation, wanted_gen)) {
                // queued for the server a standby replaced: same preamble, still valid
                r.versions.engine_generation = wanted_gen;
            }
            if r.versions.engine_generation != wanted_gen {
                return None; // superseded by a preamble change
            }
        }
        Some(req)
    }

    /// Step 1b: biblatex reads its `.bbl` once, at `\begin{document}`: a server started before
    /// the pass that wrote the bibliography (a session opened without an earlier build), or
    /// before the bibliography changed, prints citations as unresolved keys, and the probe
    /// keeps every citing unit on the pass. When the latest layout's `.bbl` differs from the
    /// server's, a server is started with it on a helper thread while the running one keeps
    /// serving, and replaces it once ready and idle. True: the iteration is settled (swapped).
    fn refresh_bibliography(&mut self, wanted_gen: u64) -> bool {
        let s = self.s.clone();
        if let Some(mut sb) = self.standby.take() {
            if let StandbyState::Starting(handle) = sb.state {
                if !handle.is_finished() {
                    sb.state = StandbyState::Starting(handle);
                    self.standby = Some(sb);
                    return false;
                }
                match handle.join() {
                    Ok(Ok(srv)) => sb.state = StandbyState::Ready(srv),
                    Ok(Err(e)) => {
                        log::warn!("standby server for the new bibliography: {e:#}");
                        return false;
                    }
                    Err(_) => return false,
                }
            }
            let StandbyState::Ready(mut srv) = sb.state else {
                unreachable!()
            };
            // only onto the server it was started to replace (a preamble change meanwhile
            // restarts the engine anyway)
            if self.server_generation != wanted_gen || sb.generation != wanted_gen + 1 {
                srv.kill();
                return false;
            }
            // and only while nothing is in flight or queued (try again next iteration); the
            // check and the swap of the link happen under one lock, so a host thread cannot
            // write a compile to the old server in between (it sees the new generation and
            // queues the request, which `next_request` carries over)
            srv.timeout = compile_timeout(&s.cfg).max(Duration::from_secs(30)); // first compile loads fonts
            {
                let mut l = s.link.lock();
                let idle = l.inflight.is_none() && s.pending.lock().is_empty();
                if !idle
                    || s.engine_generation
                        .compare_exchange(
                            wanted_gen,
                            sb.generation,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_err()
                {
                    drop(l);
                    sb.state = StandbyState::Ready(srv);
                    self.standby = Some(sb);
                    return false;
                }
                l.writer = srv.stdin_clone().ok();
                l.generation = sb.generation;
                l.contexts_sent.clear();
                l.inflight = None;
            }
            if let Some(mut old) = self.server.replace(srv) {
                let _ = old.shutdown();
            }
            self.swapped_from = Some((self.server_generation, sb.generation));
            self.server_generation = sb.generation;
            self.server_bbl = self.server.as_ref().and_then(server_bbl_hash);
            self.labels_sent = 0;
            // verdicts reached against the old server's citations no longer apply
            s.live.lock().on_server_swap();
            s.events
                .send(Event::EngineState {
                    engine_generation: sb.generation,
                    state: "Ready".into(),
                    reason: Some("restarted with the bibliography of the latest layout".into()),
                })
                .ok();
            return true;
        }
        let (aux, lv) = layout_aux(&s);
        if lv == self.bbl_checked
            || self.server_generation != wanted_gen
            || !self.server.as_mut().is_some_and(|srv| srv.is_alive())
        {
            return false;
        }
        self.bbl_checked = lv;
        let bbl = bbl_hash(aux.as_deref());
        if bbl.is_none() || bbl == self.server_bbl {
            return false;
        }
        let preamble = effective_preamble(&texts_of(&s.files.lock()), &s.cfg.main_file);
        let generation = wanted_gen + 1;
        let s2 = s.clone();
        let handle = std::thread::spawn(move || {
            FastServer::spawn_with(
                &s2.tl,
                &s2.cfg.project_root,
                &s2.cfg.build_dir.join("serve-next"),
                &preamble,
                generation,
                aux.as_deref(),
                s2.cfg.debug_dir.is_some(),
            )
        });
        self.standby = Some(Standby {
            generation,
            state: StandbyState::Starting(handle),
        });
        false
    }

    /// Step 3: (Re)start the server when the generation changed or it died. A preamble edit bumps
    /// the generation per keystroke; wait until it has been quiet for the debounce time so a
    /// burst of preamble keystrokes costs one restart, not one per keystroke. False: the
    /// iteration is settled (the generation moved on, or the start failed).
    fn ensure_server(&mut self, wanted_gen: u64) -> bool {
        let s = self.s.clone();
        let healthy = self.server_generation == wanted_gen
            && self.server.as_mut().is_some_and(|srv| srv.is_alive());
        if healthy {
            return true;
        }
        if self.server.is_some() && self.server_generation != wanted_gen {
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
                return false; // re-evaluate with the settled generation
            }
        }
        reset_link(&s);
        if let Some(mut old) = self.server.take() {
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
        // the layout the server starts from: its .bbl is compared with later layouts'
        let (aux, aux_layout) = layout_aux(&s);
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
                self.server_bbl = server_bbl_hash(&srv);
                self.server = Some(srv);
                self.server_generation = wanted_gen;
                self.bbl_checked = aux_layout;
                self.labels_sent = 0;
                s.events
                    .send(Event::EngineState {
                        engine_generation: wanted_gen,
                        state: "Ready".into(),
                        reason: None,
                    })
                    .ok();
                true
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
                false
            }
        }
    }

    /// Step 3b: Labels (\newlabel/\bibcite of the last pass) when they changed; done while idle
    /// when possible, and before a compile otherwise. False: the server failed (dropped).
    fn sync_labels(&mut self, wanted_gen: u64) -> bool {
        let s = self.s.clone();
        let (labels, labels_hash) = {
            let layout = s.layout.lock();
            (layout.labels.clone(), layout.labels_hash)
        };
        if labels_hash == self.labels_sent || labels.is_empty() {
            return true;
        }
        let mut link = s.link.lock();
        if link.inflight.is_some() {
            return true;
        }
        let srv = self.server.as_mut().unwrap();
        match srv.set_labels(&labels) {
            Ok(()) => {
                self.labels_sent = labels_hash;
                link.labels_sent = labels_hash;
                true
            }
            Err(e) => {
                drop(link);
                s.events
                    .send(Event::EngineState {
                        engine_generation: wanted_gen,
                        state: "Restarting".into(),
                        reason: Some(format!("labels: {e}")),
                    })
                    .ok();
                self.drop_server();
                false
            }
        }
    }

    /// Steps 4, 4b and 5: context, probe, compile frame, under the link lock (released around the
    /// probe compile and every side effect that takes it again).
    fn serve(&mut self, mut req: FastRequest, wanted_gen: u64) {
        let s = self.s.clone();
        let mut link = s.link.lock();
        if !self.ensure_context(&mut link, &mut req, wanted_gen) {
            return;
        }
        if req.probe {
            match self.prove(&mut link, req, wanted_gen) {
                ProbeOutcome::Send(r) => req = r,
                ProbeOutcome::Done => return,
            }
        }
        // 5. the compile frame; the result is collected by step 1
        let req_id = link.next_req;
        link.next_req += 1;
        let t0 = Instant::now();
        let srv = self.server.as_mut().unwrap();
        match srv.send_compile(req_id, req.seq, &req.source, &req.pics) {
            Ok(()) => {
                link.inflight = Some(InFlight { req, t0 });
            }
            Err(e) => {
                drop(link);
                engine_failed(&s, &req, e, wanted_gen, self.server.as_ref());
                self.drop_server();
            }
        }
    }

    /// Step 4: Send the context when the server does not hold it. False: the iteration is settled
    /// (the context is gone, or the server failed).
    fn ensure_context(
        &mut self,
        link: &mut parking_lot::MutexGuard<'_, EngineLink>,
        req: &mut FastRequest,
        wanted_gen: u64,
    ) -> bool {
        let s = self.s.clone();
        if link.context_rev_sent != req.versions.context_revision {
            link.contexts_sent.clear();
            link.context_rev_sent = req.versions.context_revision;
        }
        if link.contexts_sent.contains(&req.seq) {
            return true;
        }
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
            parking_lot::MutexGuard::unlocked(link, || {
                s.events
                    .send(Event::BackgroundScheduled {
                        par_id: Some(req.par_id),
                        reasons: vec!["context no longer available".into()],
                        edit_id: req.edit_id,
                    })
                    .ok();
            });
            return false;
        };
        if link.contexts_sent.contains(&req.seq) {
            // the lazy lookup mapped to a context already installed
            return true;
        }
        let srv = self.server.as_mut().unwrap();
        match srv.set_context(req.seq, &ctx) {
            Ok(()) => {
                link.contexts_sent.insert(req.seq);
                true
            }
            Err(e) => {
                parking_lot::MutexGuard::unlocked(link, || {
                    s.events
                        .send(Event::EngineState {
                            engine_generation: wanted_gen,
                            state: "Restarting".into(),
                            reason: Some(format!("set_context: {e}")),
                        })
                        .ok();
                    self.drop_server();
                });
                false
            }
        }
    }

    /// Step 4b: Probe mode: prove the unit first — its snapshot text (from the layout current now)
    /// compiled and compared with that layout's rows. The compile runs without the link lock
    /// (the host's apply_edit must not wait on it); `probing` keeps the direct dispatch path
    /// off the server meanwhile. A verdict is kept for the layout version.
    fn prove(
        &mut self,
        link: &mut parking_lot::MutexGuard<'_, EngineLink>,
        req: FastRequest,
        wanted_gen: u64,
    ) -> ProbeOutcome {
        let s = self.s.clone();
        let (text, lv) = {
            let layout = s.layout.lock();
            (
                layout.snapshot_text(req.par_id).map(|t| t.to_string()),
                layout.layout_version,
            )
        };
        // bound first: a guard in a `match` scrutinee would live through the arms, and the
        // arm that stores the verdict takes the same lock again (a self-deadlock)
        let cached = s.live.lock().probe_for(req.par_id, lv);
        let verdict = match cached {
            Some(v) => v,
            None => {
                let Some(text) = text else {
                    parking_lot::MutexGuard::unlocked(link, || {
                        demote_span(&s, &req, "no snapshot to compare with")
                    });
                    return ProbeOutcome::Done;
                };
                link.probing = true;
                let t_probe = Instant::now();
                let srv = self.server.as_mut().unwrap();
                let res = parking_lot::MutexGuard::unlocked(link, || {
                    srv.compile_with_pics(req.seq, &text, &req.pics)
                });
                link.probing = false;
                link.probes += 1;
                link.probe_us += t_probe.elapsed().as_micros() as u64;
                let requeue = |req: FastRequest| {
                    s.pending.lock().insert(req.par_id, req);
                    s.pending_signal.0.send(()).ok();
                };
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
                        parking_lot::MutexGuard::unlocked(link, || requeue(req));
                        return ProbeOutcome::Done;
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
                                // the layout moved while the probe ran: judge against the new
                                // one (the request goes back to the queue)
                                drop(layout);
                                parking_lot::MutexGuard::unlocked(link, || requeue(req));
                                return ProbeOutcome::Done;
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
                        parking_lot::MutexGuard::unlocked(link, || {
                            engine_failed(&s, &req, e, wanted_gen, self.server.as_ref());
                            self.drop_server();
                        });
                        return ProbeOutcome::Done;
                    }
                };
                s.live.lock().unit(req.par_id).probe = Some((lv, v.clone()));
                v
            }
        };
        match verdict {
            ProbeVerdict::Verified => ProbeOutcome::Send(req),
            ProbeVerdict::Mismatch(why) => {
                parking_lot::MutexGuard::unlocked(link, || demote_span(&s, &req, &why));
                ProbeOutcome::Done
            }
            ProbeVerdict::Leak(why) => {
                parking_lot::MutexGuard::unlocked(link, || {
                    s.live.lock().unit(req.par_id).leak = Some(why.clone());
                    demote_span(&s, &req, &why);
                    // the server's state is no longer the document's: start over
                    s.engine_generation.fetch_add(1, Ordering::SeqCst);
                    self.drop_server();
                });
                ProbeOutcome::Done
            }
        }
    }
}

/// A fast request the probe (or a leak) took away from the live path after `apply_edit`
/// reported it as fast: undo the live bookkeeping (no overlay claims a result at this
/// revision; the change counts as a background change for the spans after it), tell the host,
/// and schedule the pass that will typeset it.
pub(super) fn demote_span(s: &Shared, req: &FastRequest, why: &str) {
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
        s.live.lock().unit(req.par_id).over_budget = Some((QUARANTINED, 0));
        s.events
            .send(Event::BackgroundScheduled {
                par_id: Some(req.par_id),
                reasons: vec!["engine restarted".into()],
                edit_id: req.edit_id,
            })
            .ok();
    }
}

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
            .live
            .lock()
            .unit(req.par_id)
            .internal_error
            .replace((lv, msg.to_string()))
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
        s.live.lock().unit(req.par_id).leak = Some(why.clone());
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
                let derived = s.live.lock().get(req.par_id).and_then(|u| u.derived);
                let frags = derived
                    .and_then(|(parent, anchor, after)| {
                        let placed = s.live.lock().get(anchor).and_then(|u| u.place);
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
                                let live = s.live.lock().get(a).and_then(|u| u.rows);
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
    {
        let mut live = s.live.lock();
        let u = live.unit(req.par_id);
        u.rows = Some(dl.lines.len() as i64);
        if let Some(f) = fragments.last() {
            if let (Some(x), Some(last)) = (f.xs.first(), f.baselines.last()) {
                u.place = Some((f.page, *x, *last));
            }
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
        let prev = s
            .live
            .lock()
            .unit(req.par_id)
            .counters
            .replace(cr.counters.clone())
            .unwrap_or(expected);
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
        // one lock scope: the strikes and the mark are the same record
        let marked = {
            let mut live = s.live.lock();
            let u = live.unit(req.par_id);
            u.slow_strikes += 1;
            if u.slow_strikes >= STRIKES {
                u.slow_strikes = 0;
                u.over_budget = Some((req.versions.layout_version, total_us / 1000));
                true
            } else {
                false
            }
        };
        if marked {
            s.events
                .send(Event::BackgroundScheduled {
                    par_id: Some(req.par_id),
                    reasons: vec![reason_str(&Reason::OverBudget(total_us / 1000))],
                    edit_id: req.edit_id,
                })
                .ok();
        }
    } else {
        s.live.lock().unit(req.par_id).slow_strikes = 0;
    }
}
