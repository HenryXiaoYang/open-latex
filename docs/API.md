# API

```rust
use rtex_core::{Session, SessionConfig, Edit, Event};

let mut cfg = SessionConfig::new("/path/to/project", "main.tex");
cfg.debounce = std::time::Duration::from_millis(300);
cfg.trusted_macros = vec!["mymacro".into()];       // host-vouched pure macros (optional)
let session = Session::open(cfg)?;                 // loads main.tex and the files it \inputs, spawns the server, first pass

let r = session.apply_edit("main.tex", Edit { start_byte, end_byte, text })?;  // or "chapters/one.tex"; paths are
// project-relative (a leading "./" or an absolute path inside the project root is accepted)
// r.routed ∈ {"fast", "background", "preamble"}, r.reasons explains background routing,
// r.outcome lists touched/added/removed ParaIds, r.edit_id / r.source_revision tag the edit

for ev in session.poll(std::time::Duration::from_millis(16)) {
    match ev {
        Event::ParagraphUpdate { par_id, status, fragments, dl, pagination_stale, context_stale, versions, .. } => { /* draw; status "removed": the span is gone, clear it */ }
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

Display lists: `rtex_dl::DisplayList` (lines → items; see `docs/PROTOCOL.md` for item kinds and
`docs/DISPLAY_LIST.md`).

Scripted hosts can drive the same API over JSON lines: `rtex serve --project DIR` reads
`{"cmd":"edit","path":"main.tex","start":N,"end":M,"text":"…"}`, `set_document`, `spans`,
`status`, `request_layout`, `export_pdf`, `quit` on stdin and writes events and replies as JSON
objects on stdout.

## C ABI (`include/rtex.h`, `librtex_core.{so,a}`)

```c
RtexSession *s = rtex_session_open("{\"project_root\":\"/proj\",\"main_file\":\"main.tex\"}", &err);
char *r = rtex_session_apply_edit(s, "main.tex", start, end, " text");   /* JSON EditResult */
RtexEvent *e = rtex_session_poll(s, 16);                                  /* NULL when idle */
switch (rtex_event_kind(e)) {
  case RTEX_EVENT_PARAGRAPH_UPDATE: { size_t n; const uint8_t *dl = rtex_event_dl(e, 0, &n); /* binary display list, encoding revision 1 */ }
  case RTEX_EVENT_LAYOUT_UPDATE:    { for (i = 0; i < rtex_event_dl_count(e); i++) rtex_event_dl(e, i, &n); }
}
const char *json = rtex_event_json(e);   /* everything else about the event */
rtex_event_free(e); rtex_string_free(r); rtex_session_close(s);
```

Build: `cargo build --release -p rtex-core` produces `target/release/librtex_core.so` (and `.a`);
`examples/c/edit_loop.c` is a complete host (see its header comment for the compile line). The
header is maintained by hand and checked against the exported symbols in CI (`scripts/check-abi.sh`).
Event JSON mirrors the Rust `Event` enum (serde, `"event"` tag) with display-list payloads replaced
by `{"bytes": n, "index": i}`; `rtex_dl_to_json` converts a binary display list to the JSON mirror.
