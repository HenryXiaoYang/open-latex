//! The public session API: documents, edits, fast-path compiles, background layouts, events.

use crate::background::{run_pass, snapshot_dir, write_snapshot, BibTool};
use crate::document::{Edit, EditOutcome, FileBuf, IdAllocator, ParaId, Revision, SpanKind};
use crate::eligibility::{check_engine, check_source, Policy, Reason};
use crate::engine::FastServer;
use crate::layout::{Fragment, LayoutStore, SnapshotSpan};
use crate::texlive::TexLive;
use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use lode_dl::DisplayList;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub project_root: PathBuf,
    pub main_file: String,
    pub build_dir: PathBuf,
    pub debounce: Duration,
    pub max_passes: u32,
    pub trusted_macros: Vec<String>,
    pub fast_on_stale_context: bool,
    pub bib_tool: BibTool,
    pub compile_timeout: Duration,
}

impl SessionConfig {
    pub fn new(project_root: impl Into<PathBuf>, main_file: impl Into<String>) -> Self {
        let project_root = project_root.into();
        SessionConfig {
            build_dir: project_root.join("build").join("lode"),
            project_root,
            main_file: main_file.into(),
            debounce: Duration::from_millis(300),
            max_passes: 5,
            trusted_macros: Vec::new(),
            fast_on_stale_context: true,
            bib_tool: BibTool::Auto,
            compile_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Default, PartialEq, Eq)]
pub struct Versions {
    pub source_revision: Revision,
    pub context_revision: u64,
    pub engine_generation: u64,
    pub layout_version: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state")]
pub enum Convergence {
    Converged,
    Converging { pass: u32, reasons: Vec<String> },
    PassLimitReached { passes: u32, reasons: Vec<String> },
    Stale { pending_since: Revision },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state")]
pub enum CompileStatus {
    Ok,
    CompiledWithErrors { count: usize },
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: String,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub message: String,
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Timing {
    pub total_us: u64,
    pub tex_us: u64,
    pub traverse_us: u64,
    pub pack_us: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageUpdate {
    pub page: i64,
    pub exact: bool,
    pub hash: u64,
    pub dl: DisplayList,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParagraphPlacement {
    pub par_id: ParaId,
    pub fragments: Vec<Fragment>,
    pub lines: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event")]
pub enum Event {
    ParagraphUpdate {
        par_id: ParaId,
        edit_id: u64,
        versions: Versions,
        status: String,
        reasons: Vec<String>,
        fragments: Vec<Fragment>,
        pagination_stale: bool,
        context_stale: bool,
        dl: DisplayList,
        diagnostics: Vec<Diagnostic>,
        timing: Timing,
    },
    LayoutUpdate {
        versions: Versions,
        compile: CompileStatus,
        convergence: Convergence,
        passes: u32,
        pages_changed: Vec<PageUpdate>,
        pages_total: i64,
        placements: Vec<ParagraphPlacement>,
        eligible_paragraphs: Vec<ParaId>,
        pdf_fallback: Option<PathBuf>,
        wall_ms: u64,
    },
    Diagnostics { source: String, items: Vec<Diagnostic> },
    EngineState { engine_generation: u64, state: String, reason: Option<String> },
    BackgroundScheduled { par_id: Option<ParaId>, reasons: Vec<String>, edit_id: u64 },
    PdfExported { job_id: u64, path: Option<PathBuf>, status: CompileStatus, converged: bool, passes: u32 },
}

#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    pub edit_id: u64,
    pub source_revision: Revision,
    pub outcome: EditOutcome,
    pub routed: String,
    pub reasons: Vec<String>,
}

struct FastRequest {
    /// Internal warm-up compile: result is discarded, no event is emitted.
    warmup: bool,
    par_id: ParaId,
    edit_id: u64,
    span_hash: u64,
    source: String,
    seq: i64,
    ctx: serde_json::Value,
    versions: Versions,
    context_stale: bool,
    baselineskip: i64,
    expected_lines: i64,
}

struct Shared {
    files: Mutex<BTreeMap<String, FileBuf>>,
    ids: Mutex<IdAllocator>,
    source_revision: AtomicU64,
    preamble_revision: AtomicU64,
    /// Per file: last revision at which a background-only span or the preamble changed.
    bg_change: Mutex<HashMap<String, Vec<(Revision, i64)>>>, // (revision, first line of the changed span)
    layout: Mutex<LayoutStore>,
    engine_generation: AtomicU64,
    events: Sender<Event>,
    pending: Mutex<HashMap<ParaId, FastRequest>>,
    pending_signal: (Sender<()>, Receiver<()>),
    bg_signal: (Sender<BgCmd>, Receiver<BgCmd>),
    shutdown: AtomicBool,
    /// Background passes are deferred while paused (benchmarks, host-controlled quiet periods).
    bg_paused: AtomicBool,
    bg_pending_while_paused: AtomicBool,
    cfg: SessionConfig,
    tl: TexLive,
    policy: Policy,
    edit_counter: AtomicU64,
    convergence: Mutex<Option<Convergence>>,
    overlays: Mutex<HashMap<ParaId, Revision>>,
}

enum BgCmd {
    Pass,
    Export(u64, PathBuf),
    Quit,
}

pub struct Session {
    shared: Arc<Shared>,
    events: Receiver<Event>,
    /// Events taken out by a poll but handed back (C ABI returns one event per call).
    requeued: Mutex<std::collections::VecDeque<Event>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Session {
    pub fn open(cfg: SessionConfig) -> Result<Session> {
        let tl = TexLive::discover()?;
        let project_root = cfg.project_root.canonicalize().context("project root")?;
        let cfg = SessionConfig { project_root, ..cfg };
        std::fs::create_dir_all(&cfg.build_dir)?;
        let cfg = SessionConfig { build_dir: cfg.build_dir.canonicalize()?, ..cfg };
        let main_text = std::fs::read_to_string(cfg.project_root.join(&cfg.main_file)).with_context(|| format!("reading {}", cfg.main_file))?;
        let mut ids = IdAllocator(0);
        let mut files = BTreeMap::new();
        files.insert(cfg.main_file.clone(), FileBuf::new(&main_text, &mut ids, 1));
        let (tx, rx) = unbounded();
        let mut policy = Policy::default();
        for m in &cfg.trusted_macros {
            policy.trusted_macros.insert(m.clone());
        }
        let shared = Arc::new(Shared {
            files: Mutex::new(files),
            ids: Mutex::new(ids),
            source_revision: AtomicU64::new(1),
            preamble_revision: AtomicU64::new(1),
            bg_change: Mutex::new(HashMap::new()),
            layout: Mutex::new(LayoutStore::default()),
            engine_generation: AtomicU64::new(0),
            events: tx,
            pending: Mutex::new(HashMap::new()),
            pending_signal: unbounded(),
            bg_signal: unbounded(),
            shutdown: AtomicBool::new(false),
            bg_paused: AtomicBool::new(false),
            bg_pending_while_paused: AtomicBool::new(false),
            cfg,
            tl,
            policy,
            edit_counter: AtomicU64::new(0),
            convergence: Mutex::new(None),
            overlays: Mutex::new(HashMap::new()),
        });
        let mut threads = Vec::new();
        {
            let s = shared.clone();
            threads.push(std::thread::Builder::new().name("lode-engine".into()).spawn(move || engine_thread(s))?);
        }
        {
            let s = shared.clone();
            threads.push(std::thread::Builder::new().name("lode-background".into()).spawn(move || background_thread(s))?);
        }
        shared.bg_signal.0.send(BgCmd::Pass).ok();
        shared.pending_signal.0.send(()).ok(); // eager server start
        Ok(Session { shared, events: rx, requeued: Mutex::new(std::collections::VecDeque::new()), threads })
    }

    pub fn versions(&self) -> Versions {
        let layout = self.shared.layout.lock();
        Versions {
            source_revision: self.shared.source_revision.load(Ordering::SeqCst),
            context_revision: layout.context_revision,
            engine_generation: self.shared.engine_generation.load(Ordering::SeqCst),
            layout_version: layout.layout_version,
        }
    }

    pub fn convergence(&self) -> Option<Convergence> {
        self.shared.convergence.lock().clone()
    }

    pub fn document_text(&self, rel_path: &str) -> Option<String> {
        self.shared.files.lock().get(rel_path).map(|f| f.text.clone())
    }

    pub fn spans(&self, rel_path: &str) -> Vec<crate::document::Span> {
        self.shared.files.lock().get(rel_path).map(|f| f.spans.clone()).unwrap_or_default()
    }

    /// Replace a whole buffer. Treated as an edit covering the full text.
    pub fn set_document(&self, rel_path: &str, text: &str) -> Result<EditResult> {
        let len = self.shared.files.lock().get(rel_path).map(|f| f.text.len()).unwrap_or(0);
        if len == 0 && !self.shared.files.lock().contains_key(rel_path) {
            let rev = self.shared.source_revision.fetch_add(1, Ordering::SeqCst) + 1;
            let fb = FileBuf::new(text, &mut self.shared.ids.lock(), rev);
            self.shared.files.lock().insert(rel_path.to_string(), fb);
            self.schedule_background();
            let edit_id = self.shared.edit_counter.fetch_add(1, Ordering::SeqCst) + 1;
            return Ok(EditResult { edit_id, source_revision: rev, outcome: EditOutcome::default(), routed: "background".into(), reasons: vec!["new file".into()] });
        }
        self.apply_edit(rel_path, Edit { start_byte: 0, end_byte: len, text: text.to_string() })
    }

    pub fn apply_edit(&self, rel_path: &str, edit: Edit) -> Result<EditResult> {
        let edit_id = self.shared.edit_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let rev = self.shared.source_revision.fetch_add(1, Ordering::SeqCst) + 1;
        let (outcome, span_info) = {
            let mut files = self.shared.files.lock();
            let fb = files.get_mut(rel_path).ok_or_else(|| anyhow!("unknown file {rel_path}"))?;
            let outcome = fb.apply(&edit, &mut self.shared.ids.lock(), rev);
            let info = outcome.touched.first().and_then(|id| fb.span(*id).cloned()).map(|s| (s.clone(), fb.text[s.range.clone()].to_string(), fb.line_range(&s)));
            (outcome, info)
        };
        if outcome.preamble_changed {
            self.shared.preamble_revision.store(rev, Ordering::SeqCst);
            self.shared.bg_change.lock().entry(rel_path.to_string()).or_default().push((rev, 0));
            // restart the engine with the new preamble, drop overlays, schedule a pass
            self.shared.overlays.lock().clear();
            self.shared.engine_generation.fetch_add(1, Ordering::SeqCst);
            self.shared.pending.lock().clear();
            self.shared.pending_signal.0.send(()).ok();
            self.schedule_background();
            return Ok(EditResult { edit_id, source_revision: rev, outcome, routed: "preamble".into(), reasons: vec!["preamble changed".into()] });
        }
        // split/merge or multi-span edits: background only
        let Some((span, text, (first_line, _last_line))) = span_info else {
            self.note_bg_change(rel_path, rev, &outcome);
            self.schedule_background();
            self.shared.events.send(Event::BackgroundScheduled { par_id: None, reasons: vec!["paragraph boundaries changed".into()], edit_id }).ok();
            return Ok(EditResult { edit_id, source_revision: rev, outcome, routed: "background".into(), reasons: vec!["paragraph boundaries changed".into()] });
        };
        // eligibility
        let mut reasons: Vec<String> = Vec::new();
        if span.kind != SpanKind::Body {
            reasons.push(format!("{:?} span", span.kind));
        }
        for r in check_source(&text, &self.shared.policy) {
            reasons.push(reason_str(&r));
        }
        let layout = self.shared.layout.lock();
        let ep = layout.engine_paragraph(span.id);
        let mut request: Option<FastRequest> = None;
        match ep {
            None => reasons.push("NoContext".into()),
            Some(ep) => {
                let c = &ep.captured;
                for r in check_engine(&c.groupcode, c.nest, &c.everypar, c.begin.is_some(), &c.flags) {
                    reasons.push(reason_str(&r));
                }
                if reasons.is_empty() {
                    // context staleness: a background-only change in this file before this span,
                    // after the snapshot this context came from
                    let stale = {
                        let bg = self.shared.bg_change.lock();
                        bg.get(rel_path).map(|v| v.iter().any(|(r, line)| *r > layout.snapshot_revision && *line < first_line)).unwrap_or(false)
                    };
                    if stale && !self.shared.cfg.fast_on_stale_context {
                        reasons.push("ContextStale".into());
                    } else {
                        let baselineskip = c.glues.get("baselineskip").and_then(|g| g.first()).map(|v| *v as i64).unwrap_or(0);
                        request = Some(FastRequest {
                            warmup: false,
                            par_id: span.id,
                            edit_id,
                            span_hash: span.hash,
                            source: text.trim_end_matches('\n').to_string(),
                            seq: c.seq,
                            ctx: c.context_json(),
                            versions: Versions {
                                source_revision: rev,
                                context_revision: layout.context_revision,
                                engine_generation: self.shared.engine_generation.load(Ordering::SeqCst),
                                layout_version: layout.layout_version,
                            },
                            context_stale: stale,
                            baselineskip,
                            expected_lines: c.lines.unwrap_or(0),
                        });
                    }
                }
            }
        }
        drop(layout);
        if let Some(req) = request {
            self.shared.pending.lock().insert(span.id, req);
            self.shared.pending_signal.0.send(()).ok();
            self.shared.overlays.lock().insert(span.id, rev);
            Ok(EditResult { edit_id, source_revision: rev, outcome, routed: "fast".into(), reasons })
        } else {
            self.note_bg_change(rel_path, rev, &outcome);
            self.schedule_background();
            self.shared.events.send(Event::BackgroundScheduled { par_id: Some(span.id), reasons: reasons.clone(), edit_id }).ok();
            Ok(EditResult { edit_id, source_revision: rev, outcome, routed: "background".into(), reasons })
        }
    }

    fn note_bg_change(&self, rel_path: &str, rev: Revision, outcome: &EditOutcome) {
        let files = self.shared.files.lock();
        let line = files.get(rel_path).and_then(|fb| {
            outcome.touched.iter().chain(outcome.added.iter()).filter_map(|id| fb.span(*id)).map(|s| fb.line_range(s).0).min()
        }).unwrap_or(0);
        self.shared.bg_change.lock().entry(rel_path.to_string()).or_default().push((rev, line));
    }

    pub fn request_layout(&self) {
        self.schedule_background();
    }

    /// Defer background passes (they run when resumed). The fast path keeps working.
    pub fn pause_background(&self, paused: bool) {
        self.shared.bg_paused.store(paused, Ordering::SeqCst);
        if !paused && self.shared.bg_pending_while_paused.swap(false, Ordering::SeqCst) {
            self.shared.bg_signal.0.send(BgCmd::Pass).ok();
        }
    }

    fn schedule_background(&self) {
        *self.shared.convergence.lock() = Some(Convergence::Stale { pending_since: self.shared.source_revision.load(Ordering::SeqCst) });
        self.shared.bg_signal.0.send(BgCmd::Pass).ok();
    }

    pub fn export_pdf(&self, out: impl Into<PathBuf>) -> u64 {
        let job = self.shared.edit_counter.fetch_add(1, Ordering::SeqCst) + 1;
        self.shared.bg_signal.0.send(BgCmd::Export(job, out.into())).ok();
        job
    }

    /// Put an event back at the front of the queue (used by the C ABI's one-at-a-time poll).
    pub fn requeue(&self, e: Event) {
        self.requeued.lock().push_back(e);
    }

    /// Drain available events, waiting up to `timeout` for the first one.
    pub fn poll(&self, timeout: Duration) -> Vec<Event> {
        let mut v: Vec<Event> = self.requeued.lock().drain(..).collect();
        if v.is_empty() {
            if let Ok(e) = self.events.recv_timeout(timeout) {
                v.push(e);
            }
        }
        while let Ok(e) = self.events.try_recv() {
            v.push(e);
        }
        v
    }

    /// Wait for the first event matching `pred` (others are kept in order in the returned vec).
    pub fn wait_for(&self, timeout: Duration, mut pred: impl FnMut(&Event) -> bool) -> (Option<Event>, Vec<Event>) {
        let deadline = Instant::now() + timeout;
        let mut others = Vec::new();
        let queued: Vec<Event> = self.requeued.lock().drain(..).collect();
        for e in queued {
            if pred(&e) {
                return (Some(e), others);
            }
            others.push(e);
        }
        loop {
            let now = Instant::now();
            if now >= deadline {
                return (None, others);
            }
            match self.events.recv_timeout(deadline - now) {
                Ok(e) => {
                    if pred(&e) {
                        return (Some(e), others);
                    }
                    others.push(e);
                }
                Err(_) => return (None, others),
            }
        }
    }

    pub fn close(mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.pending_signal.0.send(()).ok();
        self.shared.bg_signal.0.send(BgCmd::Quit).ok();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.pending_signal.0.send(()).ok();
        self.shared.bg_signal.0.send(BgCmd::Quit).ok();
    }
}

fn reason_str(r: &Reason) -> String {
    match r {
        Reason::DisallowedMacro(m) => format!("macro \\{m}"),
        Reason::DisallowedMathMacro(m) => format!("math macro \\{m}"),
        Reason::SizeDeclarationOutsideGroup(m) => format!("\\{m} outside group"),
        Reason::EngineFlag(s) => format!("engine {s}"),
        other => format!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Engine thread: owns the fast server; coalesces pending requests (latest per paragraph).
// ---------------------------------------------------------------------------------------------
fn engine_thread(s: Arc<Shared>) {
    let mut server: Option<FastServer> = None;
    let mut server_generation: u64 = u64::MAX;
    let mut contexts_sent: HashSet<i64> = HashSet::new();
    let mut context_rev_sent: u64 = 0;
    loop {
        if s.shutdown.load(Ordering::SeqCst) {
            if let Some(mut srv) = server.take() {
                let _ = srv.shutdown();
            }
            return;
        }
        // next pending request (any)
        let req = {
            let mut p = s.pending.lock();
            let key = p.keys().next().copied();
            key.and_then(|k| p.remove(&k))
        };
        let wanted_gen = s.engine_generation.load(Ordering::SeqCst);
        if req.is_none() && server.is_some() && server_generation == wanted_gen {
            let _ = s.pending_signal.1.recv_timeout(Duration::from_millis(200));
            continue;
        }
        if let Some(r) = &req {
            if r.versions.engine_generation != wanted_gen {
                continue; // superseded by a preamble change
            }
        }
        // (re)start the server when the generation changed or it died
        if server.is_none() || server_generation != wanted_gen || !server.as_mut().unwrap().is_alive() {
            if let Some(mut old) = server.take() {
                old.kill();
            }
            let preamble = {
                let files = s.files.lock();
                let main = files.get(&s.cfg.main_file).map(|f| f.text.clone()).unwrap_or_default();
                crate::split_preamble(&main).map(|(p, _)| p.to_string()).unwrap_or_default()
            };
            s.events.send(Event::EngineState { engine_generation: wanted_gen, state: "Starting".into(), reason: None }).ok();
            match FastServer::spawn(&s.tl, &s.cfg.project_root, &s.cfg.build_dir.join("serve"), &preamble, wanted_gen) {
                Ok(mut srv) => {
                    srv.timeout = s.cfg.compile_timeout.max(Duration::from_secs(30)); // first compile loads fonts
                    server = Some(srv);
                    server_generation = wanted_gen;
                    contexts_sent.clear();
                    s.events.send(Event::EngineState { engine_generation: wanted_gen, state: "Ready".into(), reason: None }).ok();
                }
                Err(e) => {
                    s.events.send(Event::EngineState { engine_generation: wanted_gen, state: "Failed".into(), reason: Some(e.to_string()) }).ok();
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }
        let Some(req) = req else { continue };
        let srv = server.as_mut().unwrap();
        if context_rev_sent != req.versions.context_revision {
            contexts_sent.clear();
            context_rev_sent = req.versions.context_revision;
        }
        if !contexts_sent.contains(&req.seq) {
            if let Err(e) = srv.set_context(req.seq, &req.ctx) {
                s.events.send(Event::EngineState { engine_generation: wanted_gen, state: "Restarting".into(), reason: Some(format!("set_context: {e}")) }).ok();
                server = None;
                continue;
            }
            contexts_sent.insert(req.seq);
        }
        let t0 = Instant::now();
        let result = srv.compile(req.seq, &req.source);
        srv.timeout = s.cfg.compile_timeout;
        match result {
            Err(e) => {
                s.events.send(Event::EngineState { engine_generation: wanted_gen, state: "Restarting".into(), reason: Some(e.to_string()) }).ok();
                s.events.send(Event::Diagnostics { source: format!("fast:{:?}", req.par_id), items: vec![Diagnostic { severity: "error".into(), file: None, line: None, message: e.to_string(), context: None }] }).ok();
                server = None;
                s.engine_generation.fetch_add(1, Ordering::SeqCst);
                // the paragraph goes to the background path
                s.bg_signal.0.send(BgCmd::Pass).ok();
                s.events.send(Event::BackgroundScheduled { par_id: Some(req.par_id), reasons: vec!["engine restarted".into()], edit_id: req.edit_id }).ok();
            }
            Ok(_) if req.warmup => {}
            Ok((cr, rt)) => {
                // discard rule: the span changed meanwhile, or the generation moved on
                let current_hash = s.files.lock().values().find_map(|fb| fb.span(req.par_id).map(|sp| sp.hash));
                if current_hash != Some(req.span_hash) || s.engine_generation.load(Ordering::SeqCst) != wanted_gen {
                    continue;
                }
                let diagnostics: Vec<Diagnostic> = cr.errors.iter().map(|e| Diagnostic {
                    severity: "error".into(), file: None,
                    line: e.line.map(|l| l - 1), // line 1 is the replay head
                    message: e.message.clone().unwrap_or_default(), context: e.context.clone(),
                }).collect();
                let timing = Timing { total_us: t0.elapsed().as_micros() as u64, tex_us: rt.t_tex.as_micros() as u64, traverse_us: rt.t_traverse.as_micros() as u64, pack_us: rt.t_pack.as_micros() as u64 };
                if cr.status == "error" || cr.dl.is_none() {
                    s.events.send(Event::ParagraphUpdate {
                        par_id: req.par_id, edit_id: req.edit_id, versions: req.versions, status: "error".into(), reasons: vec![],
                        fragments: vec![], pagination_stale: false, context_stale: req.context_stale, dl: cr.dl.unwrap_or_default(),
                        diagnostics, timing,
                    }).ok();
                    continue;
                }
                let dl = cr.dl.unwrap();
                let baselines: Vec<i64> = dl.lines.iter().map(|l| l.y).collect();
                let (fragments, mut stale) = s.layout.lock().fragments(req.par_id, &baselines, req.baselineskip).unwrap_or((vec![], true));
                if req.expected_lines != dl.lines.len() as i64 {
                    stale = true;
                }
                let reasons: Vec<String> = dl.flags_map().keys().cloned().collect();
                if stale || !reasons.is_empty() {
                    s.bg_signal.0.send(BgCmd::Pass).ok();
                }
                s.events.send(Event::ParagraphUpdate {
                    par_id: req.par_id, edit_id: req.edit_id, versions: req.versions,
                    status: if reasons.is_empty() { "ok".into() } else { "ok_degraded".into() },
                    reasons, fragments, pagination_stale: stale, context_stale: req.context_stale, dl, diagnostics, timing,
                }).ok();
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Background thread: debounced full passes with capture, layout installation, exports.
// ---------------------------------------------------------------------------------------------
fn background_thread(s: Arc<Shared>) {
    loop {
        let cmd = match s.bg_signal.1.recv() {
            Ok(c) => c,
            Err(_) => return,
        };
        match cmd {
            BgCmd::Quit => return,
            BgCmd::Export(job, out) => run_export(&s, job, out),
            BgCmd::Pass => {
                if s.bg_paused.load(Ordering::SeqCst) {
                    s.bg_pending_while_paused.store(true, Ordering::SeqCst);
                    continue;
                }
                // the first pass runs at once; later ones are debounced (drain Pass commands until quiet)
                let debounce = if s.layout.lock().layout_version == 0 { Duration::from_millis(1) } else { s.cfg.debounce };
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
                    return;
                }
                run_background_pass(&s);
            }
        }
    }
}

fn snapshot(s: &Shared) -> (BTreeMap<String, String>, Vec<SnapshotSpan>, Revision) {
    let files = s.files.lock();
    let rev = s.source_revision.load(Ordering::SeqCst);
    let mut texts = BTreeMap::new();
    let mut spans = Vec::new();
    for (name, fb) in files.iter() {
        texts.insert(name.clone(), fb.text.clone());
        for sp in &fb.spans {
            let (a, b) = fb.line_range(sp);
            let text = &fb.text[sp.range.clone()];
            let background_only = sp.kind != SpanKind::Body || !check_source(text, &s.policy).is_empty();
            spans.push(SnapshotSpan { id: sp.id, file: name.clone(), first_line: a, last_line: b, last_revision: sp.last_revision, background_only });
        }
    }
    (texts, spans, rev)
}

fn run_background_pass(s: &Shared) {
    let t0 = Instant::now();
    let (texts, spans, rev) = snapshot(s);
    let snap_dir = snapshot_dir(&s.cfg.build_dir);
    if let Err(e) = write_snapshot(&s.cfg.project_root, &texts, &snap_dir) {
        s.events.send(Event::Diagnostics { source: "background".into(), items: vec![Diagnostic { severity: "error".into(), file: None, line: None, message: format!("snapshot: {e}"), context: None }] }).ok();
        return;
    }
    let out_dir = s.cfg.build_dir.join("bg");
    let outcome = match run_pass(&s.tl, &snap_dir, &s.cfg.main_file, &out_dir, s.cfg.max_passes, s.cfg.bib_tool, true) {
        Ok(o) => o,
        Err(e) => {
            s.events.send(Event::Diagnostics { source: "background".into(), items: vec![Diagnostic { severity: "error".into(), file: None, line: None, message: e.to_string(), context: None }] }).ok();
            *s.convergence.lock() = Some(Convergence::PassLimitReached { passes: 0, reasons: vec![e.to_string()] });
            return;
        }
    };
    let diagnostics = parse_log(&outcome.capture.log);
    let errors = diagnostics.iter().filter(|d| d.severity == "error").count();
    let compile = if outcome.capture.json.pages == 0 { CompileStatus::Failed } else if errors > 0 { CompileStatus::CompiledWithErrors { count: errors } } else { CompileStatus::Ok };
    if compile == CompileStatus::Failed {
        s.events.send(Event::Diagnostics { source: "background".into(), items: diagnostics }).ok();
        s.events.send(Event::LayoutUpdate {
            versions: Versions { source_revision: rev, ..Default::default() }, compile, convergence: Convergence::PassLimitReached { passes: outcome.passes, reasons: vec!["no pages".into()] },
            passes: outcome.passes, pages_changed: vec![], pages_total: 0, placements: vec![], eligible_paragraphs: vec![], pdf_fallback: None, wall_ms: t0.elapsed().as_millis() as u64,
        }).ok();
        return;
    }
    let (changed, versions, placements, eligible, pdf) = {
        let mut layout = s.layout.lock();
        let changed = match layout.install(&outcome.capture, spans, rev) {
            Ok(c) => c,
            Err(e) => {
                s.events.send(Event::Diagnostics { source: "background".into(), items: vec![Diagnostic { severity: "error".into(), file: None, line: None, message: format!("install layout: {e}"), context: None }] }).ok();
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
        for (id, idx) in &layout.by_span {
            let ep = &layout.paragraphs[*idx];
            let lines = ep.captured.lines.unwrap_or(0);
            let base = ep.captured.glues.get("baselineskip").and_then(|g| g.first()).map(|v| *v as i64).unwrap_or(0);
            let fake: Vec<i64> = vec![0; lines as usize];
            if let Some((frags, _)) = layout.fragments(*id, &fake, base) {
                placements.push(ParagraphPlacement { par_id: *id, fragments: frags, lines });
            }
            let c = &ep.captured;
            if check_engine(&c.groupcode, c.nest, &c.everypar, c.begin.is_some(), &c.flags).is_empty() {
                eligible.push(*id);
            }
        }
        placements.sort_by_key(|p| p.par_id);
        eligible.sort();
        (changed, versions, placements, eligible, layout.pdf.clone())
    };
    // warm the engine: compile the first eligible paragraph once so fonts are loaded before
    // the first real keystroke
    if let Some(id) = eligible.first().copied() {
        let layout = s.layout.lock();
        let files = s.files.lock();
        if let (Some(ep), Some(text)) = (layout.engine_paragraph(id), files.values().find_map(|fb| fb.span_text(id))) {
            let c = &ep.captured;
            let mut p = s.pending.lock();
            if !p.contains_key(&id) {
                // exercise the font variants and math a body paragraph commonly needs so their
                // font instances are loaded before the first real keystroke
                let warm = format!("{} \\emph{{warm}} \\textbf{{warm}} \\textit{{warm}} \\textsc{{warm}} {{\\small warm}} $x^2_i + \\alpha \\sum \\frac{{1}}{{2}} \\mathbf{{v}}$", text.trim_end_matches('\n'));
                p.insert(id, FastRequest {
                    warmup: true, par_id: id, edit_id: 0, span_hash: 0, source: warm, seq: c.seq, ctx: c.context_json(),
                    versions: Versions { source_revision: rev, context_revision: layout.context_revision, engine_generation: s.engine_generation.load(Ordering::SeqCst), layout_version: layout.layout_version },
                    context_stale: false, baselineskip: 0, expected_lines: 0,
                });
                s.pending_signal.0.send(()).ok();
            }
        }
    }
    // commit overlays older than the snapshot
    s.overlays.lock().retain(|_, r| *r > rev);
    let current = s.source_revision.load(Ordering::SeqCst);
    let mut reasons = Vec::new();
    if !outcome.aux_stable {
        reasons.push("aux family still changing".into());
    }
    if errors > 0 {
        reasons.push(format!("{errors} compile errors"));
    }
    let convergence = if current > rev {
        Convergence::Stale { pending_since: rev + 1 }
    } else if outcome.aux_stable && errors == 0 {
        Convergence::Converged
    } else if !outcome.aux_stable && outcome.passes >= s.cfg.max_passes {
        Convergence::PassLimitReached { passes: outcome.passes, reasons: reasons.clone() }
    } else {
        Convergence::Converging { pass: outcome.passes, reasons: reasons.clone() }
    };
    *s.convergence.lock() = Some(convergence.clone());
    let pages_changed: Vec<PageUpdate> = {
        let layout = s.layout.lock();
        changed.iter().filter_map(|n| layout.pages.get(n).map(|dl| PageUpdate { page: *n, exact: dl.is_exact(), hash: layout.page_hashes[n], dl: dl.clone() })).collect()
    };
    let pages_total = outcome.capture.json.pages;
    if !diagnostics.is_empty() {
        s.events.send(Event::Diagnostics { source: "background".into(), items: diagnostics }).ok();
    }
    let any_degraded = pages_changed.iter().any(|p| !p.exact);
    s.events.send(Event::LayoutUpdate {
        versions, compile, convergence: convergence.clone(), passes: outcome.passes, pages_changed, pages_total, placements, eligible_paragraphs: eligible,
        pdf_fallback: if any_degraded { pdf } else { None }, wall_ms: t0.elapsed().as_millis() as u64,
    }).ok();
    if matches!(convergence, Convergence::Stale { .. }) {
        s.bg_signal.0.send(BgCmd::Pass).ok();
    }
}

fn run_export(s: &Shared, job: u64, out: PathBuf) {
    let (texts, _spans, _rev) = snapshot(s);
    let snap_dir = s.cfg.build_dir.join("export-src");
    if let Err(e) = write_snapshot(&s.cfg.project_root, &texts, &snap_dir) {
        s.events.send(Event::PdfExported { job_id: job, path: None, status: CompileStatus::Failed, converged: false, passes: 0 }).ok();
        let _ = e;
        return;
    }
    let out_dir = s.cfg.build_dir.join("export");
    match run_pass(&s.tl, &snap_dir, &s.cfg.main_file, &out_dir, s.cfg.max_passes, s.cfg.bib_tool, false) {
        Ok(o) => {
            let diags = parse_log(&o.capture.log);
            let errors = diags.iter().filter(|d| d.severity == "error").count();
            let status = if !o.capture.pdf.exists() { CompileStatus::Failed } else if errors > 0 { CompileStatus::CompiledWithErrors { count: errors } } else { CompileStatus::Ok };
            let path = if o.capture.pdf.exists() {
                if let Some(parent) = out.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::copy(&o.capture.pdf, &out).ok().map(|_| out.clone())
            } else {
                None
            };
            s.events.send(Event::PdfExported { job_id: job, path, status, converged: o.aux_stable && errors == 0, passes: o.passes }).ok();
        }
        Err(_) => {
            s.events.send(Event::PdfExported { job_id: job, path: None, status: CompileStatus::Failed, converged: false, passes: 0 }).ok();
        }
    }
}

/// Parse a LuaLaTeX log (with -file-line-error) into diagnostics.
pub fn parse_log(log: &Path) -> Vec<Diagnostic> {
    let Ok(text) = std::fs::read_to_string(log) else { return vec![] };
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let re_fle = regex::Regex::new(r"^(?P<file>[^:\s][^:]*):(?P<line>\d+): (?P<msg>.*)$").unwrap();
    let re_warn = regex::Regex::new(r"^(?:LaTeX|Package \w+|Class \w+) Warning: (?P<msg>.*)$").unwrap();
    let re_box = regex::Regex::new(r"^(Overfull|Underfull) \\[hv]box .* at lines (\d+)--(\d+)").unwrap();
    for (i, l) in lines.iter().enumerate() {
        if let Some(c) = re_fle.captures(l) {
            if c["msg"].starts_with("Undefined") || !c["msg"].is_empty() {
                out.push(Diagnostic { severity: "error".into(), file: Some(c["file"].to_string()), line: c["line"].parse().ok(), message: c["msg"].to_string(), context: lines.get(i + 1).map(|s| s.to_string()) });
            }
        } else if let Some(c) = re_warn.captures(l) {
            out.push(Diagnostic { severity: "warning".into(), file: None, line: None, message: c["msg"].to_string(), context: None });
        } else if let Some(c) = re_box.captures(l) {
            out.push(Diagnostic { severity: "info".into(), file: None, line: c[2].parse().ok(), message: l.to_string(), context: None });
        } else if l.starts_with("! ") {
            out.push(Diagnostic { severity: "error".into(), file: None, line: None, message: l[2..].to_string(), context: lines.get(i + 1).map(|s| s.to_string()) });
        }
    }
    out
}
