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

mod diag;
mod engine_loop;
mod live;
mod passes;
mod route;

pub use diag::parse_log;
use diag::*;
use engine_loop::*;
use live::*;
use passes::*;

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
    /// Units whose fast compiles take longer than this three times in a row are routed to the
    /// background path until the next layout (default 50 ms: a unit that slow, a large plot
    /// being drawn, would hold up the units typed after it; a quicker one, even an `align`
    /// of 15 ms, is still far quicker live than through a pass of seconds).
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
    /// Debugging: a directory the session writes diagnostics into (default: `$RTEX_DEBUG_DIR`
    /// when set, else none). Every engine failure (watchdog, state mismatch, crash) leaves a
    /// bundle there with the request's source, context and picture entries, the server's
    /// driver, preamble, TeX log and stage trace; `requests.log` gets one line per live
    /// compile. C ABI `"debug_dir"`, CLI `rtex serve --debug-dir`.
    pub debug_dir: Option<PathBuf>,
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
    /// probe. A unit the layout does not know yet (typed fresh, split off) has nothing to
    /// compare with: it is compiled on a borrowed context only when its vocabulary is
    /// allow-listed, or when what is not are picture environments the cache holds (their
    /// bodies are not run); otherwise it waits for the pass.
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
            fast_budget: Duration::from_millis(50),
            unit_envs: Vec::new(),
            warm_background: true,
            eligibility: EligibilityMode::Probe,
            picture_cache: true,
            debug_dir: std::env::var_os("RTEX_DEBUG_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
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
// ParagraphUpdate carries its display list inline: an event is moved once, through a channel,
// and boxing the field would change the public type hosts match on for no measurable gain
#[allow(clippy::large_enum_variant)]
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
    /// The unit was let through unproven because every picture in it comes from the cache
    /// (its only vocabulary beyond the allow-list): a result that drew a picture after all is
    /// unverified and the unit waits for the pass.
    pics_required: bool,
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
    /// Queued requests, latest per unit, served smallest id first (a split's halves in
    /// document order: the second chains its placement onto the first).
    pending: Mutex<BTreeMap<ParaId, FastRequest>>,
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
    edit_counter: AtomicU64,
    convergence: Mutex<Option<Convergence>>,
    /// What the live path records per span between layouts (borrowed contexts, live rows
    /// and placements, probe verdicts, budget marks, leaks...), with their expiry rules.
    live: Mutex<LiveUnits>,
    /// Standby background engine (preamble loaded, waiting for the body).
    standby: Mutex<Option<WarmEngine>>,
    /// Directory of the last finished background pass (its aux family seeds the next pass;
    /// its PDF and capture files stay until a later pass reuses the directory).
    last_pass_dir: Mutex<Option<PathBuf>>,
    /// The picture cache index as of a layout version (the live engine reuses cached pictures).
    pic_index: Mutex<Option<(u64, crate::piccache::PicCache)>>,
    /// The source scan for pictures at a revision (the whole project; one scan per edit, not
    /// one per routed span).
    pic_scan: Mutex<Option<(Revision, Arc<Vec<crate::piccache::PictureRef>>)>>,
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
        let project_root = crate::paths::canonical(&cfg.project_root).context("project root")?;
        let cfg = SessionConfig {
            project_root,
            main_file: crate::paths::key(&cfg.main_file),
            ..cfg
        };
        std::fs::create_dir_all(&cfg.build_dir)?;
        let cfg = SessionConfig {
            build_dir: crate::paths::canonical(&cfg.build_dir)?,
            ..cfg
        };
        // layout PDFs are numbered per session: an earlier session's copies would stay for
        // as long as this one takes to reach their numbers
        if let Ok(rd) = std::fs::read_dir(cfg.build_dir.join("bg")) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                if name.starts_with("layout-") && name.ends_with(".pdf") {
                    let _ = std::fs::remove_file(ent.path());
                }
            }
        }
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
            pending: Mutex::new(BTreeMap::new()),
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
            edit_counter: AtomicU64::new(0),
            live: Mutex::new(LiveUnits::default()),
            convergence: Mutex::new(None),
            standby: Mutex::new(None),
            last_pass_dir: Mutex::new(None),
            pic_index: Mutex::new(None),
            pic_scan: Mutex::new(None),
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
                return crate::paths::key(&rel.to_string_lossy());
            }
            if let Ok(canon) = crate::paths::canonical(p) {
                if let Ok(rel) = canon.strip_prefix(&self.shared.cfg.project_root) {
                    return crate::paths::key(&rel.to_string_lossy());
                }
            }
            return path.to_string();
        }
        let path = &crate::paths::key(path);
        let mut t = path.as_str();
        while let Some(r) = t.strip_prefix("./") {
            t = r;
        }
        t.to_string()
    }

    pub fn document_text(&self, rel_path: &str) -> Option<String> {
        let key = self.rel_key(rel_path);
        self.shared.files.lock().get(&key).map(|f| f.text.clone())
    }

    /// Probe compiles run so far (probe mode): a unit whose only vocabulary beyond the
    /// allow-list is cached pictures needs none.
    pub fn probes(&self) -> u64 {
        self.shared.link.lock().probes
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
            self.shared.live.lock().on_preamble_change();
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
                if !live && self.shared.cfg.debug_dir.is_some() {
                    debug_request_line(
                        &self.shared,
                        &format!("par {} refused: {}", id.0, reasons.join("; ")),
                    );
                }
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
                            versions,
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

/// The first line of a (multi-line) message, such as a Lua error with its traceback.
fn first_line_of(msg: &str) -> &str {
    msg.lines().next().unwrap_or("")
}
