# API (Rust; the C ABI is generated from it in M5)

```rust
use lode_core::{Session, SessionConfig, Edit, Event};

let mut cfg = SessionConfig::new("/path/to/project", "main.tex");
cfg.debounce = std::time::Duration::from_millis(300);
cfg.trusted_macros = vec!["mymacro".into()];       // host-vouched pure macros (optional)
let session = Session::open(cfg)?;                 // spawns the server and the first background pass

let r = session.apply_edit("main.tex", Edit { start_byte, end_byte, text })?;
// r.routed ∈ {"fast", "background", "preamble"}, r.reasons explains background routing,
// r.outcome lists touched/added/removed ParaIds, r.edit_id / r.source_revision tag the edit

for ev in session.poll(std::time::Duration::from_millis(16)) {
    match ev {
        Event::ParagraphUpdate { par_id, fragments, dl, pagination_stale, context_stale, versions, .. } => { /* draw */ }
        Event::LayoutUpdate { versions, convergence, pages_changed, placements, pdf_fallback, .. } => { /* replace pages */ }
        Event::Diagnostics { source, items } => { /* show */ }
        Event::EngineState { engine_generation, state, reason } => { /* status bar */ }
        Event::BackgroundScheduled { par_id, reasons, .. } => { /* "will update shortly" */ }
        Event::PdfExported { job_id, path, status, converged, passes } => { /* done */ }
    }
}
let job = session.export_pdf("out/main.pdf");
session.request_layout();                         // force a pass
session.spans("main.tex");                        // current segmentation (ids, byte ranges, kinds)
session.versions(); session.convergence();
session.close();
```

Threading: `apply_edit`/`set_document` are non-blocking (they lock the document briefly). One
engine thread owns the server; one background thread owns passes and exports. `poll` and
`wait_for` drain a channel; `Session` is `Send` and can be shared behind an `Arc`.

Display lists: `lode_dl::DisplayList` (lines → items; see `docs/PROTOCOL.md` for item kinds and
`docs/DISPLAY_LIST.md` once the binary format is frozen).

Scripted hosts can drive the same API over JSON lines: `lode serve --project DIR` reads
`{"cmd":"edit","path":"main.tex","start":N,"end":M,"text":"…"}`, `set_document`, `spans`,
`status`, `request_layout`, `export_pdf`, `quit` on stdin and writes events and replies as JSON
objects on stdout.
