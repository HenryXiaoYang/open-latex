//! The public session API: documents, edits, fast-path compiles, background layouts, events.

use crate::background::{
    run_pass_with, run_pass_with_runner, snapshot_dir, write_snapshot, BibTool, WarmEngine,
};
use crate::document::{Edit, EditOutcome, FileBuf, IdAllocator, ParaId, Revision, Span, SpanKind};
use crate::eligibility::{
    check_engine_unit, classify_source, classify_source_with, everypar_allowed, Policy, Reason,
    UnitShape,
};
use crate::engine::{FastServer, Response};
use crate::layout::{EngineUnit, Fragment, LayoutStore, SnapshotSpan};
use crate::texlive::TexLive;
use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use rtex_dl::DisplayList;
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
    /// Units whose last fast compile took longer than this are routed to the background path
    /// until the next layout (the host keeps its real-time guarantee).
    pub fast_budget: Duration,
    /// Extra block environments (theorem-like) treated as units, besides those found by scanning
    /// the preamble for `\newtheorem`.
    pub unit_envs: Vec<String>,
    /// Keep a standby lualatex with the preamble loaded for background passes (default true):
    /// a pass then only typesets the body, 3–4× faster for short documents.
    pub warm_background: bool,
    /// How a unit qualifies for the fast path (default `Probe`).
    pub eligibility: EligibilityMode,
    /// Background passes reuse unchanged pictures (tikzpicture, circuitikz) from an earlier
    /// pass's PDF instead of drawing them again (default true; `docs/ARCHITECTURE.md`).
    pub picture_cache: bool,
}

/// Diagnostic view of one layout unit (`Session::layout_units`, `rtex serve` `units`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct LayoutUnitInfo {
    pub uid: i64,
    pub kind: String,
    pub name: Option<String>,
    pub file: Option<String>,
    pub first_line: i64,
    pub last_line: Option<i64>,
    pub rows: i64,
    pub span: Option<ParaId>,
}

/// Outcome of a probe compile (probe mode).
#[derive(Debug, Clone)]
enum ProbeVerdict {
    /// The snapshot text reproduced the layout's rows: edits of the unit go live.
    Verified,
    /// Rows differed, the compile failed, or there was nothing to compare (why).
    Mismatch(String),
    /// The compile changed the meaning of a control sequence it mentions (names).
    Leak(String),
}

/// How the session decides that a unit may be typeset live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EligibilityMode {
    /// Only units whose every command is on the allow-list (`eligibility.rs`).
    AllowList,
    /// Any structurally sound unit: one whose vocabulary is not allow-listed is probed first —
    /// its text as the last pass typeset it is compiled once and compared row by row with the
    /// pass; a match verifies it for this layout, anything else keeps it on the background
    /// path (`BackgroundScheduled` with an `unverified:` reason). Allow-listed units skip the
    /// probe.
    #[default]
    Probe,
}

impl EligibilityMode {
    pub fn parse(s: &str) -> Option<EligibilityMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "probe" => Some(EligibilityMode::Probe),
            "allowlist" | "allow-list" | "allow_list" => Some(EligibilityMode::AllowList),
            _ => None,
        }
    }
}

impl SessionConfig {
    pub fn new(project_root: impl Into<PathBuf>, main_file: impl Into<String>) -> Self {
        let project_root = project_root.into();
        SessionConfig {
            build_dir: project_root.join("build").join("rtex"),
            project_root,
            main_file: main_file.into(),
            debounce: Duration::from_millis(300),
            max_passes: 5,
            trusted_macros: Vec::new(),
            fast_on_stale_context: true,
            bib_tool: BibTool::Auto,
            compile_timeout: Duration::from_secs(5),
            fast_budget: Duration::from_millis(5),
            unit_envs: Vec::new(),
            warm_background: true,
            eligibility: EligibilityMode::Probe,
            picture_cache: true,
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

#[derive(Debug, Clone, Default, Serialize)]
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
    /// Number of rows (typeset lines, display rows, float rows) of the unit.
    pub lines: i64,
    /// Unit kind: "par", "env:<name>" or "heading:<name>".
    pub kind: String,
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
    Diagnostics {
        source: String,
        items: Vec<Diagnostic>,
    },
    EngineState {
        engine_generation: u64,
        state: String,
        reason: Option<String>,
    },
    BackgroundScheduled {
        par_id: Option<ParaId>,
        reasons: Vec<String>,
        edit_id: u64,
    },
    PdfExported {
        job_id: u64,
        path: Option<PathBuf>,
        status: CompileStatus,
        converged: bool,
        passes: u32,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    pub edit_id: u64,
    pub source_revision: Revision,
    pub outcome: EditOutcome,
    pub routed: String,
    pub reasons: Vec<String>,
    /// Host-side stage times in µs: segmentation, eligibility, request dispatch.
    pub host_us: [u64; 3],
}

/// The kernel's `\everypar` after a heading (`\@afterheading`), replayed for a paragraph typed
/// fresh right after a heading span.
const AFTER_HEADING_EVERYPAR: &str = "\\if@nobreak \\@nobreakfalse \\clubpenalty \\@M \\if@afterindent \\else {\\setbox \\z@ \\lastbox }\\fi \\else \\clubpenalty \\@clubpenalty \\everypar {}\\fi ";

struct FastRequest {
    /// Internal warm-up compile: result is discarded, no event is emitted.
    warmup: bool,
    par_id: ParaId,
    edit_id: u64,
    span_hash: u64,
    source: String,
    /// Picture cache entries for the source's picture environments, in order (JSON array;
    /// empty when none is cached), and how many pictures the source has.
    pics: String,
    pics_n: usize,
    seq: i64,
    /// Context object for the server; None when the engine link already holds this context
    /// (the engine thread builds it from the layout store when it must send it).
    ctx: Option<serde_json::Value>,
    versions: Versions,
    context_stale: bool,
    expected_rows: i64,
    /// Probe mode: the unit must be proven first (its snapshot text, fetched from the layout
    /// current when the engine thread gets to it, compiled and compared with that layout).
    probe: bool,
}

/// A compile the server is working on (sent either by the engine thread or directly by the
/// host thread through `EngineLink::writer`).
struct InFlight {
    req: FastRequest,
    t0: Instant,
}

/// Host-visible side of the fast server: lets `apply_edit` submit a compile frame straight to
/// the server's stdin when the server is idle, saving the wake-up of the engine thread. The
/// engine thread owns the server and reads every result, in order.
#[derive(Default)]
struct EngineLink {
    writer: Option<std::fs::File>,
    generation: u64,
    inflight: Option<InFlight>,
    contexts_sent: HashSet<i64>,
    context_rev_sent: u64,
    labels_sent: u64,
    next_req: i64,
    /// A probe compile is running on the engine thread without the lock held: the direct
    /// dispatch path must not write to the server meanwhile.
    probing: bool,
    /// Probe mode statistics: probe compiles run and their total time.
    probes: u64,
    probe_us: u64,
}

struct Shared {
    link: Mutex<EngineLink>,
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
    /// A background pass is compiling right now: fast-path timings are not budget evidence.
    bg_running: AtomicBool,
    /// Background passes are deferred while paused (benchmarks, host-controlled quiet periods).
    bg_paused: AtomicBool,
    bg_pending_while_paused: AtomicBool,
    cfg: SessionConfig,
    tl: TexLive,
    policy: Mutex<Policy>,
    /// Files the preamble `\input`s: an edit to one of them is a preamble change.
    preamble_inputs: Mutex<std::collections::BTreeSet<String>>,
    /// Files read with `\input` anywhere (see `document::fast_source`).
    inputted: Mutex<std::collections::BTreeSet<String>>,
    /// Units over the fast budget: par id → (layout_version when measured, ms).
    slow_units: Mutex<HashMap<ParaId, (u64, u64)>>,
    /// Consecutive over-budget compiles per unit (the first ones may be loading fonts).
    slow_candidates: Mutex<HashMap<ParaId, u32>>,
    edit_counter: AtomicU64,
    convergence: Mutex<Option<Convergence>>,
    overlays: Mutex<HashMap<ParaId, Revision>>,
    /// Spans typeset with a borrowed context: span → (parent span, rows follow the parent).
    derived: Mutex<HashMap<ParaId, (ParaId, bool)>>,
    /// Counters the last fast compile of a span advanced (what the layout saw until then): a
    /// change renumbers what follows and needs a pass.
    counters_seen: Mutex<HashMap<ParaId, BTreeMap<String, i64>>>,
    /// Probe verdicts (probe mode): span → (layout version the verdict is for, verdict).
    probe: Mutex<HashMap<ParaId, (u64, ProbeVerdict)>>,
    /// Spans whose compile changed the meaning of a control sequence (a definition leaked):
    /// background-only until the preamble changes.
    leaky: Mutex<HashMap<ParaId, String>>,
    /// Row count of the latest fast result per span (anchors paragraphs placed after it).
    live_rows: Mutex<HashMap<ParaId, i64>>,
    /// Standby background engine (preamble loaded, waiting for the body).
    standby: Mutex<Option<WarmEngine>>,
    /// Directory of the last finished background pass (its aux family seeds the next pass;
    /// its PDF and capture files stay until a later pass reuses the directory).
    last_pass_dir: Mutex<Option<PathBuf>>,
    /// The picture cache index as of a layout version (the live engine reuses cached pictures).
    pic_index: Mutex<Option<(u64, crate::piccache::PicCache)>>,
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
        let cfg = SessionConfig {
            project_root,
            ..cfg
        };
        std::fs::create_dir_all(&cfg.build_dir)?;
        let cfg = SessionConfig {
            build_dir: cfg.build_dir.canonicalize()?,
            ..cfg
        };
        // the main file and, transitively, every file it \input/\includes
        let texts = crate::document::load_project_files(&cfg.project_root, &cfg.main_file)
            .with_context(|| format!("reading {}", cfg.main_file))?;
        let mut ids = IdAllocator(0);
        let mut files = BTreeMap::new();
        let preamble = effective_preamble(&texts, &cfg.main_file);
        let preamble_inputs = preamble_input_set(&texts, &cfg.main_file);
        let mut policy = Policy::from_preamble(&preamble, &cfg.trusted_macros, &cfg.unit_envs);
        policy.permissive = cfg.eligibility == EligibilityMode::Probe;
        let envs: Vec<String> = policy.theorem_envs.iter().cloned().collect();
        // the main file first so its span ids come first
        files.insert(
            cfg.main_file.clone(),
            FileBuf::with_block_envs(&texts[&cfg.main_file], &mut ids, 1, envs.clone()),
        );
        for (name, text) in texts.iter().filter(|(n, _)| **n != cfg.main_file) {
            files.insert(
                name.clone(),
                FileBuf::with_block_envs(text, &mut ids, 1, envs.clone()),
            );
        }
        let (tx, rx) = unbounded();
        let shared = Arc::new(Shared {
            link: Mutex::new(EngineLink {
                next_req: 1,
                ..Default::default()
            }),
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
            bg_running: AtomicBool::new(false),
            bg_paused: AtomicBool::new(false),
            bg_pending_while_paused: AtomicBool::new(false),
            cfg,
            tl,
            policy: Mutex::new(policy),
            preamble_inputs: Mutex::new(preamble_inputs),
            inputted: Mutex::new(crate::document::inputted_files(&texts)),
            slow_units: Mutex::new(HashMap::new()),
            slow_candidates: Mutex::new(HashMap::new()),
            edit_counter: AtomicU64::new(0),
            convergence: Mutex::new(None),
            overlays: Mutex::new(HashMap::new()),
            derived: Mutex::new(HashMap::new()),
            counters_seen: Mutex::new(HashMap::new()),
            probe: Mutex::new(HashMap::new()),
            leaky: Mutex::new(HashMap::new()),
            live_rows: Mutex::new(HashMap::new()),
            standby: Mutex::new(None),
            last_pass_dir: Mutex::new(None),
            pic_index: Mutex::new(None),
        });
        let mut threads = Vec::new();
        {
            let s = shared.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("rtex-engine".into())
                    .spawn(move || engine_thread(s))?,
            );
        }
        {
            let s = shared.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("rtex-background".into())
                    .spawn(move || background_thread(s))?,
            );
        }
        shared.bg_signal.0.send(BgCmd::Pass).ok();
        shared.pending_signal.0.send(()).ok(); // eager server start
        Ok(Session {
            shared,
            events: rx,
            requeued: Mutex::new(std::collections::VecDeque::new()),
            threads,
        })
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

    /// Diagnostic view of the installed layout's units: (unit id, kind, first line, last line,
    /// mapped span).
    pub fn layout_units(&self) -> Vec<LayoutUnitInfo> {
        let layout = self.shared.layout.lock();
        layout
            .units
            .iter()
            .map(|u| LayoutUnitInfo {
                uid: u.uid,
                kind: u.captured.kind.clone(),
                name: u.captured.name.clone(),
                file: u.captured.file.clone(),
                first_line: u.captured.begin_line,
                last_line: u.captured.end_line,
                rows: u.rows(),
                span: u.span,
            })
            .collect()
    }

    pub fn convergence(&self) -> Option<Convergence> {
        self.shared.convergence.lock().clone()
    }

    /// The key a host's path maps to: project-relative, without a leading `./`; an absolute
    /// path inside the project root is made relative.
    fn rel_key(&self, path: &str) -> String {
        let p = Path::new(path);
        if p.is_absolute() {
            if let Ok(rel) = p.strip_prefix(&self.shared.cfg.project_root) {
                return rel.to_string_lossy().into_owned();
            }
            if let Ok(canon) = p.canonicalize() {
                if let Ok(rel) = canon.strip_prefix(&self.shared.cfg.project_root) {
                    return rel.to_string_lossy().into_owned();
                }
            }
            return path.to_string();
        }
        let mut t = path;
        while let Some(r) = t.strip_prefix("./") {
            t = r;
        }
        t.to_string()
    }

    pub fn document_text(&self, rel_path: &str) -> Option<String> {
        let key = self.rel_key(rel_path);
        self.shared.files.lock().get(&key).map(|f| f.text.clone())
    }

    pub fn spans(&self, rel_path: &str) -> Vec<crate::document::Span> {
        let key = self.rel_key(rel_path);
        self.shared
            .files
            .lock()
            .get(&key)
            .map(|f| f.spans.clone())
            .unwrap_or_default()
    }

    /// Replace a whole buffer. Treated as an edit covering the full text.
    pub fn set_document(&self, rel_path: &str, text: &str) -> Result<EditResult> {
        let key = self.rel_key(rel_path);
        let rel_path = key.as_str();
        let len = self
            .shared
            .files
            .lock()
            .get(rel_path)
            .map(|f| f.text.len())
            .unwrap_or(0);
        if len == 0 && !self.shared.files.lock().contains_key(rel_path) {
            let rev = self.shared.source_revision.fetch_add(1, Ordering::SeqCst) + 1;
            let fb = FileBuf::with_block_envs(
                text,
                &mut self.shared.ids.lock(),
                rev,
                self.shared
                    .policy
                    .lock()
                    .theorem_envs
                    .iter()
                    .cloned()
                    .collect(),
            );
            self.shared.files.lock().insert(rel_path.to_string(), fb);
            self.schedule_background();
            let edit_id = self.shared.edit_counter.fetch_add(1, Ordering::SeqCst) + 1;
            return Ok(EditResult {
                edit_id,
                source_revision: rev,
                outcome: EditOutcome::default(),
                routed: "background".into(),
                reasons: vec!["new file".into()],
                host_us: [0, 0, 0],
            });
        }
        self.apply_edit(
            rel_path,
            Edit {
                start_byte: 0,
                end_byte: len,
                text: text.to_string(),
            },
        )
    }

    pub fn apply_edit(&self, rel_path: &str, edit: Edit) -> Result<EditResult> {
        let key = self.rel_key(rel_path);
        let rel_path = key.as_str();
        let t_start = Instant::now();
        let edit_id = self.shared.edit_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let rev = self.shared.source_revision.fetch_add(1, Ordering::SeqCst) + 1;
        let is_setup = |text: &str| {
            crate::eligibility::setup_statements(&crate::eligibility::strip_comments(text))
                .is_some()
        };
        let (mut outcome, setup_span) = {
            let mut files = self.shared.files.lock();
            let fb = files
                .get_mut(rel_path)
                .ok_or_else(|| anyhow!("unknown file {rel_path}"))?;
            // a setup span (definitions, lengths after \begin{document}) before or after the
            // edit: the server's preamble changes
            let was_setup = fb.spans.iter().any(|sp| {
                sp.range.start <= edit.end_byte
                    && edit.start_byte <= sp.range.end
                    && is_setup(&fb.text[sp.range.clone()])
            });
            let outcome = fb.apply(&edit, &mut self.shared.ids.lock(), rev);
            let now_setup = outcome
                .touched
                .iter()
                .chain(outcome.added.iter())
                .any(|id| fb.span_text(*id).map(is_setup).unwrap_or(false));
            (outcome, was_setup || now_setup)
        };
        if setup_span || self.shared.preamble_inputs.lock().contains(rel_path) {
            outcome.preamble_changed = true;
        }
        if outcome.preamble_changed {
            self.shared.preamble_revision.store(rev, Ordering::SeqCst);
            self.load_new_inputs();
            {
                let files = self.shared.files.lock();
                let texts = texts_of(&files);
                let preamble = effective_preamble(&texts, &self.shared.cfg.main_file);
                *self.shared.preamble_inputs.lock() =
                    preamble_input_set(&texts, &self.shared.cfg.main_file);
                *self.shared.inputted.lock() = crate::document::inputted_files(&texts);
                let mut policy = Policy::from_preamble(
                    &preamble,
                    &self.shared.cfg.trusted_macros,
                    &self.shared.cfg.unit_envs,
                );
                policy.permissive = self.shared.cfg.eligibility == EligibilityMode::Probe;
                let envs: Vec<String> = policy.theorem_envs.iter().cloned().collect();
                *self.shared.policy.lock() = policy;
                drop(files);
                let mut files = self.shared.files.lock();
                for fb in files.values_mut() {
                    if fb.extra_block_envs != envs {
                        fb.extra_block_envs = envs.clone();
                        fb.resegment(&mut self.shared.ids.lock(), rev);
                    }
                }
            }
            self.shared
                .bg_change
                .lock()
                .entry(rel_path.to_string())
                .or_default()
                .push((rev, 0));
            // restart the engine with the new preamble, drop overlays and the standby, schedule a pass
            if let Some(w) = self.shared.standby.lock().take() {
                w.kill();
            }
            self.shared.overlays.lock().clear();
            self.shared.slow_units.lock().clear();
            self.shared.probe.lock().clear();
            self.shared.leaky.lock().clear();
            self.shared.engine_generation.fetch_add(1, Ordering::SeqCst);
            self.shared.pending.lock().clear();
            self.shared.pending_signal.0.send(()).ok();
            self.schedule_background();
            return Ok(EditResult {
                edit_id,
                source_revision: rev,
                outcome,
                routed: "preamble".into(),
                reasons: vec!["preamble changed".into()],
                host_us: [t_start.elapsed().as_micros() as u64, 0, 0],
            });
        }
        let t_seg = t_start.elapsed();
        // a new \input/\include line: load the file so its paragraphs get spans (and contexts
        // from the next pass)
        if outcome
            .touched
            .iter()
            .chain(outcome.added.iter())
            .any(|id| {
                let files = self.shared.files.lock();
                files
                    .get(rel_path)
                    .and_then(|fb| fb.span_text(*id))
                    .map(|t| !crate::document::input_targets(t).is_empty())
                    .unwrap_or(false)
            })
        {
            self.load_new_inputs();
        }
        // Every body span the edit touched or created is routed (a split compiles both halves,
        // a merge the merged paragraph, a fresh paragraph borrows a context); the first one in
        // document order decides the reported outcome. Removed spans are announced as empty
        // updates so hosts clear what they drew for them.
        let mut requests: Vec<FastRequest> = Vec::new();
        let mut primary: Option<(ParaId, bool, Vec<String>)> = None;
        let mut any_background = false;
        {
            let files = self.shared.files.lock();
            let fb = files
                .get(rel_path)
                .ok_or_else(|| anyhow!("unknown file {rel_path}"))?;
            let layout = self.shared.layout.lock();
            let policy = self.shared.policy.lock();
            let mut cands: Vec<ParaId> = outcome
                .touched
                .iter()
                .chain(outcome.added.iter())
                .copied()
                .collect();
            cands.sort_by_key(|id| fb.span(*id).map(|sp| sp.range.start).unwrap_or(usize::MAX));
            cands.dedup();
            for id in cands {
                let Some(span) = fb.span(id) else { continue };
                let (req, reasons) =
                    self.route_span(rel_path, &files, fb, span, rev, edit_id, &layout, &policy);
                let live = req.is_some();
                any_background |= !live;
                if primary.is_none() {
                    primary = Some((id, live, reasons));
                }
                if let Some(r) = req {
                    requests.push(r);
                }
            }
            if !outcome.removed.is_empty() {
                let versions = Versions {
                    source_revision: rev,
                    context_revision: layout.context_revision,
                    engine_generation: self.shared.engine_generation.load(Ordering::SeqCst),
                    layout_version: layout.layout_version,
                };
                for id in &outcome.removed {
                    self.shared
                        .events
                        .send(Event::ParagraphUpdate {
                            par_id: *id,
                            edit_id,
                            versions: versions.clone(),
                            status: "removed".into(),
                            reasons: vec![],
                            fragments: vec![],
                            pagination_stale: true,
                            context_stale: false,
                            dl: DisplayList::default(),
                            diagnostics: vec![],
                            timing: Timing::default(),
                        })
                        .ok();
                }
            }
        }
        let t_elig = t_start.elapsed();
        let Some((primary_id, primary_live, primary_reasons)) = primary else {
            // nothing routable (e.g. everything deleted): background only
            self.note_bg_change(rel_path, rev, &outcome);
            self.schedule_background();
            self.shared
                .events
                .send(Event::BackgroundScheduled {
                    par_id: None,
                    reasons: vec!["paragraph boundaries changed".into()],
                    edit_id,
                })
                .ok();
            return Ok(EditResult {
                edit_id,
                source_revision: rev,
                outcome,
                routed: "background".into(),
                reasons: vec!["paragraph boundaries changed".into()],
                host_us: [t_seg.as_micros() as u64, 0, 0],
            });
        };
        for req in requests {
            self.shared.overlays.lock().insert(req.par_id, rev);
            self.dispatch_fast(req);
        }
        // a boundary change always needs a pass (pagination of what follows); so does a span
        // the fast path could not take
        if any_background || !outcome.added.is_empty() || !outcome.removed.is_empty() {
            self.note_bg_change(rel_path, rev, &outcome);
            self.schedule_background();
        }
        let host_us = [
            t_seg.as_micros() as u64,
            (t_elig - t_seg).as_micros() as u64,
            (t_start.elapsed() - t_elig).as_micros() as u64,
        ];
        if primary_live {
            Ok(EditResult {
                edit_id,
                source_revision: rev,
                outcome,
                routed: "fast".into(),
                reasons: primary_reasons,
                host_us,
            })
        } else {
            self.shared
                .events
                .send(Event::BackgroundScheduled {
                    par_id: Some(primary_id),
                    reasons: primary_reasons.clone(),
                    edit_id,
                })
                .ok();
            Ok(EditResult {
                edit_id,
                source_revision: rev,
                outcome,
                routed: "background".into(),
                reasons: primary_reasons,
                host_us: [host_us[0], host_us[1], 0],
            })
        }
    }

    /// Decide the fast path for one span: the allow-list on its source, the capture facts of
    /// its unit (or a borrowed context when the layout does not know it yet), the budget and the
    /// context staleness. Returns the request to send, or the reasons it goes to the background.
    #[allow(clippy::too_many_arguments)]
    fn route_span(
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
        for r in src_reasons {
            if probe_mode && crate::eligibility::is_vocabulary_reason(&r) {
                // not a verdict: the probe compile decides
                needs_probe = true;
            } else {
                reasons.push(reason_str(&r));
            }
        }
        if let Some(why) = self.shared.leaky.lock().get(&span.id) {
            reasons.push(format!("unverified: {why}"));
        }
        let mut seq = 0i64;
        let mut ctx: Option<serde_json::Value> = None;
        let mut expected_rows = 0i64;
        let mut derived_from: Option<(ParaId, bool)> = None;
        let mut probe = false;
        match layout.unit(span.id) {
            Some(eu) => {
                if needs_probe {
                    let verdict = self
                        .shared
                        .probe
                        .lock()
                        .get(&span.id)
                        .filter(|(lv, _)| *lv == layout.layout_version)
                        .map(|(_, v)| v.clone());
                    match verdict {
                        Some(ProbeVerdict::Verified) => {}
                        Some(ProbeVerdict::Mismatch(why)) | Some(ProbeVerdict::Leak(why)) => {
                            reasons.push(format!("unverified: {why}"))
                        }
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
                if let Some((lv, ms)) = self.shared.slow_units.lock().get(&span.id).copied() {
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
                if needs_probe {
                    // a borrowed context has no layout rows to prove the result against
                    reasons.push("unverified: no layout unit to compare with".into());
                }
                if reasons.is_empty() && shape == UnitShape::Par && span.kind == SpanKind::Body {
                    match self.derive_context(fb, span, layout) {
                        Some((parent, after, json)) => {
                            // negative context ids never collide with unit ids
                            seq = -(span.id.0 as i64);
                            ctx = Some(json);
                            derived_from = Some((parent, after));
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
            self.shared.derived.lock().insert(span.id, d);
        }
        let (pics, pics_n) = self.unit_pics(rel_path, files, fb, span, layout.layout_version);
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
        };
        (Some(req), reasons)
    }

    /// Picture cache entries for the picture environments of a span, in document order (the
    /// live engine replaces a matching picture with the cached region instead of drawing it:
    /// the compile of a paragraph followed by a plot costs the text, not the plot). Entries are
    /// looked up by the same hash the background pass uses; a picture without one is `null`.
    /// Returns the JSON array and the number of pictures, or nothing when no entry applies.
    fn unit_pics(
        &self,
        rel_path: &str,
        files: &BTreeMap<String, FileBuf>,
        fb: &FileBuf,
        span: &Span,
        layout_version: u64,
    ) -> (String, usize) {
        use crate::piccache::{preamble_hash, scan_pictures, PicCache, PICTURE_ENVS};
        let text = &fb.text[span.range.clone()];
        if !self.shared.cfg.picture_cache
            || !PICTURE_ENVS
                .iter()
                .any(|e| text.contains(&format!("\\begin{{{e}}}")))
        {
            return (String::new(), 0);
        }
        let texts: BTreeMap<String, String> = files
            .iter()
            .map(|(n, f)| (n.clone(), f.text.clone()))
            .collect();
        let main = &self.shared.cfg.main_file;
        let pics = scan_pictures(&texts, main, preamble_hash(&texts, main));
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
        let mut any = false;
        for p in &pics {
            let Some((f, line)) = p.key.rsplit_once(':') else {
                continue;
            };
            let Ok(line) = line.parse::<i64>() else {
                continue;
            };
            if f != file || line < first || line > last {
                continue;
            }
            let e = cache.entry_json(p);
            any |= e.is_some();
            entries.push(e.unwrap_or(serde_json::Value::Null));
        }
        if !any {
            return (String::new(), 0);
        }
        let n = entries.len();
        (serde_json::to_string(&entries).unwrap_or_default(), n)
    }

    /// Context for a paragraph the layout does not know yet (created by a split or a merge, or
    /// typed fresh): a plain body paragraph borrows the parameters, fonts and counters of the
    /// nearest paragraph unit before it (after it when there is none) with the paragraph-start
    /// state of a paragraph that follows a paragraph, or a heading when a heading span precedes
    /// it. The next layout replaces the borrowed context with a captured one. Returns (parent
    /// span, whether the rows follow the parent, context).
    fn derive_context(
        &self,
        fb: &FileBuf,
        span: &Span,
        layout: &LayoutStore,
    ) -> Option<(ParaId, bool, serde_json::Value)> {
        let idx = fb.spans.iter().position(|sp| sp.id == span.id)?;
        let usable = |sp: &Span| -> Option<&EngineUnit> {
            if sp.kind != SpanKind::Body {
                return None;
            }
            let eu = layout.unit(sp.id)?;
            let ok = eu.kind() == "par"
                && eu.rows() > 0
                && eu
                    .first_para
                    .as_ref()
                    .map(|p| p.begin.is_some())
                    .unwrap_or(false)
                && everypar_allowed(&eu.captured.everypar);
            ok.then_some(eu)
        };
        let before = fb.spans[..idx]
            .iter()
            .rev()
            .find_map(|sp| usable(sp).map(|eu| (sp.id, true, eu)));
        let (parent, after, eu) = match before {
            Some(b) => b,
            None => fb.spans[idx + 1..]
                .iter()
                .find_map(|sp| usable(sp).map(|eu| (sp.id, false, eu)))?,
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
        Some((parent, after, json))
    }

    /// Hand a fast request to the server: written straight to its stdin when the server is idle
    /// and already holds the context (no engine-thread wake-up on the critical path), queued for
    /// the engine thread otherwise. Queued requests coalesce per paragraph.
    fn dispatch_fast(&self, req: FastRequest) {
        let mut link = self.shared.link.lock();
        let direct = !req.probe
            && !link.probing
            && link.writer.is_some()
            && link.inflight.is_none()
            && link.generation == req.versions.engine_generation
            && link.context_rev_sent == req.versions.context_revision
            && link.contexts_sent.contains(&req.seq);
        if direct {
            let req_id = link.next_req;
            link.next_req += 1;
            let frame = FastServer::encode_compile(req_id, req.seq, &req.source, &req.pics);
            let t0 = Instant::now();
            use std::io::Write;
            if link.writer.as_mut().unwrap().write_all(&frame).is_ok() {
                link.inflight = Some(InFlight { req, t0 });
                drop(link);
                // wake the engine thread so it is spinning on the FIFO when the result lands
                self.shared.pending_signal.0.send(()).ok();
                return;
            }
            link.writer = None; // broken pipe: the engine thread restarts the server
        }
        drop(link);
        self.shared.pending.lock().insert(req.par_id, req);
        self.shared.pending_signal.0.send(()).ok();
    }

    fn note_bg_change(&self, rel_path: &str, rev: Revision, outcome: &EditOutcome) {
        let files = self.shared.files.lock();
        let line = files
            .get(rel_path)
            .and_then(|fb| {
                outcome
                    .touched
                    .iter()
                    .chain(outcome.added.iter())
                    .filter_map(|id| fb.span(*id))
                    .map(|s| fb.line_range(s).0)
                    .min()
            })
            .unwrap_or(0);
        self.shared
            .bg_change
            .lock()
            .entry(rel_path.to_string())
            .or_default()
            .push((rev, line));
    }

    pub fn request_layout(&self) {
        self.schedule_background();
    }

    /// Defer background passes (they run when resumed). The fast path keeps working.
    pub fn pause_background(&self, paused: bool) {
        self.shared.bg_paused.store(paused, Ordering::SeqCst);
        if !paused
            && self
                .shared
                .bg_pending_while_paused
                .swap(false, Ordering::SeqCst)
        {
            self.shared.bg_signal.0.send(BgCmd::Pass).ok();
        }
    }

    fn schedule_background(&self) {
        *self.shared.convergence.lock() = Some(Convergence::Stale {
            pending_since: self.shared.source_revision.load(Ordering::SeqCst),
        });
        self.shared.bg_signal.0.send(BgCmd::Pass).ok();
    }

    /// Load from disk every file the tracked files now `\input` that is not tracked yet.
    /// Returns the number of files added.
    fn load_new_inputs(&self) -> usize {
        let mut files = self.shared.files.lock();
        let envs: Vec<String> = self
            .shared
            .policy
            .lock()
            .theorem_envs
            .iter()
            .cloned()
            .collect();
        let mut added = 0;
        loop {
            let wanted: Vec<String> = files
                .values()
                .flat_map(|fb| crate::document::input_targets(&fb.text))
                .filter(|t| !files.contains_key(t))
                .collect();
            if wanted.is_empty() || files.len() > 256 {
                break;
            }
            let mut any = false;
            for rel in wanted {
                if files.contains_key(&rel) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(self.shared.cfg.project_root.join(&rel))
                else {
                    // missing on disk: remember it as empty so we do not retry on every edit;
                    // the compiler reports it
                    files.insert(
                        rel,
                        FileBuf::with_block_envs("", &mut self.shared.ids.lock(), 1, envs.clone()),
                    );
                    continue;
                };
                let rev = self.shared.source_revision.load(Ordering::SeqCst);
                files.insert(
                    rel,
                    FileBuf::with_block_envs(&text, &mut self.shared.ids.lock(), rev, envs.clone()),
                );
                added += 1;
                any = true;
            }
            if !any {
                break;
            }
        }
        if added > 0 {
            *self.shared.inputted.lock() = crate::document::inputted_files(&texts_of(&files));
        }
        added
    }

    pub fn export_pdf(&self, out: impl Into<PathBuf>) -> u64 {
        let job = self.shared.edit_counter.fetch_add(1, Ordering::SeqCst) + 1;
        self.shared
            .bg_signal
            .0
            .send(BgCmd::Export(job, out.into()))
            .ok();
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
    pub fn wait_for(
        &self,
        timeout: Duration,
        mut pred: impl FnMut(&Event) -> bool,
    ) -> (Option<Event>, Vec<Event>) {
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
        Reason::OverBudget(ms) => {
            format!("OverBudget: last fast compile took {ms} ms, over the budget")
        }
        other => format!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Engine thread: owns the fast server; coalesces pending requests (latest per paragraph).
// ---------------------------------------------------------------------------------------------
fn engine_thread(s: Arc<Shared>) {
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
            srv.timeout = s.cfg.compile_timeout;
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
                    );
                    server = None;
                    reset_link(&s);
                }
                Err(e) => {
                    engine_failed(&s, &fl.req, e, wanted_gen);
                    server = None;
                    reset_link(&s);
                }
            }
            continue;
        }
        // 2. next queued request (any)
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
            match FastServer::spawn(
                &s.tl,
                &s.cfg.project_root,
                &s.cfg.build_dir.join("serve"),
                &preamble,
                wanted_gen,
                aux.as_deref(),
            ) {
                Ok(mut srv) => {
                    srv.timeout = s.cfg.compile_timeout.max(Duration::from_secs(30)); // first compile loads fonts
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
                    s.events
                        .send(Event::EngineState {
                            engine_generation: wanted_gen,
                            state: "Failed".into(),
                            reason: Some(e.to_string()),
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
                            engine_failed(&s, &req, e, wanted_gen);
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
                engine_failed(&s, &req, e, wanted_gen);
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
fn demote_span(s: &Shared, req: &FastRequest, why: &str) {
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

fn engine_failed(s: &Shared, req: &FastRequest, e: anyhow::Error, wanted_gen: u64) {
    s.events
        .send(Event::EngineState {
            engine_generation: wanted_gen,
            state: "Restarting".into(),
            reason: Some(e.to_string()),
        })
        .ok();
    s.events
        .send(Event::Diagnostics {
            source: format!("fast:{:?}", req.par_id),
            items: vec![Diagnostic {
                severity: "error".into(),
                file: None,
                line: None,
                message: e.to_string(),
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
const QUARANTINED: u64 = u64::MAX;

fn handle_result(
    s: &Shared,
    req: FastRequest,
    cr: crate::engine::CompileResult,
    rt: crate::engine::RoundTrip,
    t0: Instant,
    wanted_gen: u64,
) {
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
    if req.warmup {
        return;
    }
    // the engine saw a different number of picture environments than the source scan (a
    // picture made by a macro, a nested one): its cache entries may have gone to the wrong
    // pictures, so the result is dropped and the compile repeated without them
    if req.pics_n > 0 && cr.pics_seen != Some(req.pics_n as i64) {
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
                // a borrowed context: placed relative to the parent unit's rows
                let parent = s.derived.lock().get(&req.par_id).copied();
                let frags = parent
                    .and_then(|(parent, after)| {
                        let live = s.live_rows.lock().get(&parent).copied();
                        layout.fragments_relative(parent, live, after, &rows)
                    })
                    .unwrap_or_default();
                (frags, true)
            }
        }
    };
    s.live_rows.lock().insert(req.par_id, dl.lines.len() as i64);
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

// ---------------------------------------------------------------------------------------------
// Background thread: debounced full passes with capture, layout installation, exports.
// ---------------------------------------------------------------------------------------------
fn background_thread(s: Arc<Shared>) {
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

fn standby_dir(s: &Shared, n: usize) -> PathBuf {
    s.cfg.build_dir.join(format!("src-body-{n}"))
}

/// Output directory of the pass that runs from `src-body-{n}`. Two alternate: a standby
/// (lualatex up to the end of the preamble) opens its log, and with some preambles its PDF,
/// as soon as it starts, so it must never share a directory with the pass that is running.
fn pass_dir(s: &Shared, n: usize) -> PathBuf {
    s.cfg.build_dir.join("bg").join(format!("pass-{n}"))
}

/// The directory of the last finished pass (seeded from disk on the first call: the newer
/// of the two pass directories, or the pre-pass-directory layout `build/bg` itself).
fn last_pass_dir(s: &Shared) -> Option<PathBuf> {
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
fn standby_slot(s: &Shared) -> usize {
    match last_pass_dir(s) {
        Some(d) if d.ends_with("pass-0") => 1,
        _ => 0,
    }
}

fn jobname_of(main: &str) -> String {
    Path::new(main)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("main")
        .to_string()
}

/// Start a standby engine for the next background pass (while the user types, it loads the
/// current preamble). Replaces a standby whose preamble is outdated; keeps a matching one.
fn prepare_standby(s: &Shared) {
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

fn texts_of(files: &BTreeMap<String, FileBuf>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(n, fb)| (n.clone(), fb.text.clone()))
        .collect()
}

/// The preamble the fast server loads: the main file's preamble with `\input`ted files inlined
/// from the buffers, plus the document's body setup statements (`crate::server_preamble`).
fn effective_preamble(texts: &BTreeMap<String, String>, main: &str) -> String {
    crate::server_preamble(texts, main)
}

/// The preamble as the standby engine hashes it (`write_body_snapshot`): inputs inlined, no
/// body setup (the standby compiles the whole body itself).
fn standby_preamble_hash(texts: &BTreeMap<String, String>, main: &str) -> Option<u64> {
    texts
        .get(main)
        .and_then(|t| crate::split_preamble(t))
        .map(|(p, _)| crate::document::hash_str(&crate::document::expand_inputs(p, texts)))
}

fn preamble_input_set(
    texts: &BTreeMap<String, String>,
    main: &str,
) -> std::collections::BTreeSet<String> {
    texts
        .get(main)
        .and_then(|t| crate::split_preamble(t))
        .map(|(p, _)| crate::document::transitive_inputs(p, texts))
        .unwrap_or_default()
}

fn snapshot(s: &Shared) -> (BTreeMap<String, String>, Vec<SnapshotSpan>, Revision) {
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

fn run_background_pass(s: &Shared) {
    s.bg_running.store(true, Ordering::SeqCst);
    run_background_pass_inner(s);
    s.bg_running.store(false, Ordering::SeqCst);
}

/// A pass of a multi-pass run that took long enough to be worth showing before the run is
/// stable (TikZ-heavy documents: 45 s per pass, three passes to converge).
const PROVISIONAL_LAYOUT_AFTER: Duration = Duration::from_secs(2);

fn run_background_pass_inner(s: &Shared) {
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
        absorb(cap);
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

fn layout_failed(s: &Shared, t0: Instant, rev: Revision, msg: String) {
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
fn deliver_layout(
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
                        if v >= 2 {
                            let _ = std::fs::remove_file(bg.join(format!("layout-{}.pdf", v - 2)));
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

fn run_export(s: &Shared, job: u64, out: PathBuf) {
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

/// Parse a LuaLaTeX log (with -file-line-error) into diagnostics.
pub fn parse_log(log: &Path) -> Vec<Diagnostic> {
    let Ok(text) = std::fs::read_to_string(log) else {
        return vec![];
    };
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let re_fle = regex::Regex::new(r"^(?P<file>[^:\s][^:]*):(?P<line>\d+): (?P<msg>.*)$").unwrap();
    let re_warn =
        regex::Regex::new(r"^(?:LaTeX|Package \w+|Class \w+) Warning: (?P<msg>.*)$").unwrap();
    let re_box =
        regex::Regex::new(r"^(Overfull|Underfull) \\[hv]box .* at lines (\d+)--(\d+)").unwrap();
    for (i, l) in lines.iter().enumerate() {
        if let Some(c) = re_fle.captures(l) {
            if c["msg"].starts_with("Undefined") || !c["msg"].is_empty() {
                out.push(Diagnostic {
                    severity: "error".into(),
                    file: Some(c["file"].to_string()),
                    line: c["line"].parse().ok(),
                    message: c["msg"].to_string(),
                    context: lines.get(i + 1).map(|s| s.to_string()),
                });
            }
        } else if let Some(c) = re_warn.captures(l) {
            out.push(Diagnostic {
                severity: "warning".into(),
                file: None,
                line: None,
                message: c["msg"].to_string(),
                context: None,
            });
        } else if let Some(c) = re_box.captures(l) {
            out.push(Diagnostic {
                severity: "info".into(),
                file: None,
                line: c[2].parse().ok(),
                message: l.to_string(),
                context: None,
            });
        } else if l.starts_with("! ") {
            out.push(Diagnostic {
                severity: "error".into(),
                file: None,
                line: None,
                message: l[2..].to_string(),
                context: lines.get(i + 1).map(|s| s.to_string()),
            });
        }
    }
    out
}
