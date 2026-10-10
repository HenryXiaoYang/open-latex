# Embedding rtex in an editor

rtex is a library. It owns the LaTeX side (the live engine, background passes, layouts) and
hands your program, the *host*, things to draw. This page covers how to connect to it, what you
receive, and what to do with it.

There are three ways in, all backed by the same `Session`:

| Interface | Use it when |
|---|---|
| Rust crate `rtex-core` | your host is written in Rust |
| C library (`include/rtex.h`, `librtex_core`) | any language with a C FFI: C, C++, Swift, Zig, Python via ctypes, … |
| `rtex serve` (JSON lines over stdin/stdout) | scripting, prototypes, editor extensions in JavaScript or other languages, tests |

The VS Code extension ([realtime-tex-vsc-plugin](https://github.com/HenryXiaoYang/realtime-tex-vsc-plugin))
is an example of a complete host.

## Prerequisites

rtex needs a TeX Live installation with LuaLaTeX. It finds `lualatex` on `PATH`, or in the
directory named by `RTEX_TEXLIVE_BIN`.

It also needs its own TeX and Lua support files. rtex looks for them in this order:
1. `$RTEX_TEXDIR`;
2. next to the `rtex` executable, as the [release archives](https://github.com/HenryXiaoYang/realtime-tex/releases/latest)
   lay them out (`bin/rtex` and `share/rtex/tex/`);
3. the source tree it was built from.

A host that loads the C library has its own executable, so step 2 does not apply to it. Set
`RTEX_TEXDIR` to the archive's `share/rtex/tex` directory, unless the library was built from a
checkout that is still there. `rtex doctor` prints what rtex finds.

## A session

A session is one open project. It loads the main file and every file the main file reaches
through `\input`, `\include` or `\subfile`. It starts the live engine and runs the first
background pass.

```rust
use rtex_core::{Edit, Event, Session, SessionConfig};
use std::time::Duration;

let cfg = SessionConfig::new("/path/to/project", "main.tex");
let session = Session::open(cfg)?;

// the user typed " world" at byte 1234 of main.tex
let r = session.apply_edit("main.tex", Edit { start_byte: 1234, end_byte: 1234, text: " world".into() })?;
// r.routed is "fast", "background" or "preamble"; r.reasons says why when it is not "fast"

loop {
    for ev in session.poll(Duration::from_millis(16)) {
        match ev {
            Event::ParagraphUpdate { .. } => { /* repaint one unit */ }
            Event::LayoutUpdate { .. } => { /* replace pages */ }
            _ => {}
        }
    }
}
```

**Edits** are byte ranges in the UTF-8 text of a file: `start_byte..end_byte` is replaced by
`text`. Paths are relative to the project root. An absolute path inside the project also works,
and on Windows either slash direction is accepted. If you have the whole buffer instead of a
diff (an unsaved file, a file switched in from disk), use `set_document(path, text)`.
`apply_edit` and `set_document` return immediately; results arrive as events.

Other calls:

| Call | Does |
|---|---|
| `request_layout()` | start a background pass now instead of after the debounce |
| `pause_background(true/false)` | hold background passes, for example while the user is typing fast on battery; live updates continue |
| `export_pdf(path)` | build a clean PDF (no capture package), report it with `PdfExported` |
| `spans(path)` | the current split of a file into units: ids, byte ranges, kinds |
| `versions()`, `convergence()` | current state |
| `close()` | stop the engines and threads |

`Session` is `Send + Sync`; wrap it in an `Arc` to share it. The calls only lock the document
briefly. The live engine and the background passes run on their own threads.

## Configuration

| Rust field | C / JSON key | `rtex serve` flag | Default | Meaning |
|---|---|---|---|---|
| `project_root` | `project_root` | `--project` | required | project directory |
| `main_file` | `main_file` | `--main` | `main.tex` | main file, relative to the root |
| `build_dir` | `build_dir` | `--build` | `<root>/build/rtex` | where passes, logs and PDFs go |
| `debounce` | `debounce_ms` | | 300 ms | quiet time before a background pass starts |
| `max_passes` | `max_passes` | | 5 | passes per layout before giving up (`PassLimitReached`) |
| `fast_budget` | `fast_budget_ms` | `--fast-budget-ms` | 50 ms | a unit slower than this three times in a row waits for the pass |
| `compile_timeout` | `compile_timeout_ms` | | 5 s | a live compile that takes longer is killed and the engine restarted |
| `eligibility` | `eligibility` | `--eligibility` | `probe` | `probe` verifies unknown commands by comparison; `allowlist` only allows known ones |
| `trusted_macros` | `trusted_macros` | | none | macros you vouch for, treated as allow-listed |
| `unit_envs` | `unit_envs` | | none | extra environments to treat like theorems (found automatically from `\newtheorem`) |
| `picture_cache` | `picture_cache` | `--no-picture-cache` | on | reuse unchanged TikZ pictures from earlier passes |
| `warm_background` | `warm_background` | | on | keep a standby engine with the preamble loaded for passes |
| `fast_on_stale_context` | `fast_on_stale_context` | | on | deliver live results even when something before the unit changed since the last pass |
| `bib_tool` | | | auto | `biber`, `bibtex` or none; auto picks from the files the pass writes |
| `debug_dir` | `debug_dir` | `--debug-dir` | `$RTEX_DEBUG_DIR` | write a diagnostic bundle per engine failure ([development.md](development.md#when-the-live-engine-fails)) |

## Events

Events arrive in order on one queue. Every event that comes from the engine or a pass carries
`versions` ([how-it-works.md](how-it-works.md#keeping-results-in-order)).

### `ParagraphUpdate`: a unit was re-typeset

| Field | What to do with it |
|---|---|
| `par_id` | the unit (span) this replaces. Remove everything this unit drew before. |
| `status` | `ok` draw it. `ok_degraded` draw it, but something in it could not be represented: see `reasons`, and expect a pass. `error` TeX reported an error: show `diagnostics`, don't trust the display list. `removed` the span no longer exists (merged or deleted): clear it. |
| `dl` | the unit's display list: rows of glyphs, rules and images, relative to the unit's box. |
| `fragments` | where to draw it: one entry per page the unit spans, with the page number, the rows it holds (`first_line..last_line`) and each row's `x` and `baseline` on that page. |
| `pagination_stale` | the unit's height or row count changed, so the material after it is out of place until the next layout, which is already scheduled. |
| `context_stale` | something before the unit changed since the last pass (or the unit borrowed a neighbour's context). The result is the best available; the next layout confirms it. |
| `fragments[].approximate` | the rows had no position in the last layout (a new paragraph, an extra line), so the position was estimated. |
| `timing` | microseconds: total, TeX, traversal. Handy for a status bar. |

To draw: for each fragment `f`, rows `f.first_line..=f.last_line` (1-based) of `dl.lines` go on
page `f.page`. The `j`-th of them is translated so that its baseline sits at `f.baselines[j]`
and its left edge at `f.xs[j]`; then draw its items.

### `LayoutUpdate`: a background pass finished

| Field | What to do with it |
|---|---|
| `pages_changed` | display lists of the pages that differ from the previous layout, and whether each one is `exact`. Replace those pages. |
| `pages_total` | page count. Drop pages beyond it. |
| `pdf_fallback` | a PDF of this layout, set whenever any page is degraded. Draw non-exact pages from it with your PDF renderer. The file stays valid until two layouts later. |
| `placements` | where every unit's rows are now. Use these to re-anchor live results you are still showing. |
| `convergence` | `Converged`, `Converging`, `PassLimitReached` or `Stale` ([how-it-works.md](how-it-works.md#convergence)). Good for a status indicator. |
| `compile` | `Ok`, `CompiledWithErrors{count}` (show the diagnostics) or `Failed` (keep showing the previous layout). |
| `eligible_paragraphs` | the units that may go live in this layout. |
| `wall_ms`, `passes` | how long the run took, and how many passes it needed. |

When a layout arrives, drop live results for units whose last edit is older than the layout's
snapshot. In practice: if `source_revision` hasn't moved since the edit, the layout already
contains it. Keep the newer ones and move them to the new `placements`.

### The others

| Event | Meaning |
|---|---|
| `BackgroundScheduled { par_id, reasons }` | this edit will appear with the next pass, not live. `reasons` is human-readable ("unverified: …", "last fast compile took 63 ms, over the budget"). |
| `Diagnostics { source, items }` | errors and warnings with file, line, message and the offending source line, from a pass or the live engine |
| `EngineState { engine_generation, state, reason }` | the live engine is `Starting`, `Ready`, `Restarting` or has `Failed`. Status bar material. |
| `PdfExported { job_id, path, status, converged, passes }` | an export finished. Only `converged: true` promises the same PDF as a clean LuaLaTeX build. |

## Drawing display lists

A display list is a list of rows, each with positioned items:

- **glyph:** a glyph index in a font file at a size, at `(x, baseline)`, with an optional
  horizontal scale (microtype font expansion);
- **rule:** a filled rectangle;
- **image:** an image file (PNG, JPEG, PDF page) in a rectangle, sometimes with a transform;
- **colour:** push, pop or set a colour.

Positions are in TeX scaled points (65536 per pt), and y grows downward. Fonts are referenced by
file name, so you need an OpenType/TrueType renderer that can load them, plus Type 1 for classic
Computer Modern math. The full format, binary and JSON, is in [display-list.md](display-list.md).

Pages marked not exact are best drawn from `pdf_fallback`. The usual approach is to render the
whole layout from the PDF and paint live paragraph updates over it. The display list then gives
you exact positions for hit-testing, cursor placement and the live overlay.

## C library

```sh
cargo build --release -p rtex-core
# shared: target/release/librtex_core.so (Linux), librtex_core.dylib (macOS), rtex_core.dll (Windows)
# static: librtex_core.a, or rtex_core.lib on Windows
```

```c
#include "rtex.h"

char *err = NULL;
RtexSession *s = rtex_session_open("{\"project_root\":\"/proj\",\"main_file\":\"main.tex\"}", &err);
if (!s) { fprintf(stderr, "%s\n", err); rtex_string_free(err); return 1; }

char *r = rtex_session_apply_edit(s, "main.tex", 1234, 1234, " world");   /* EditResult as JSON */
rtex_string_free(r);

RtexEvent *e;
while ((e = rtex_session_poll(s, 16)) != NULL) {
    const char *json = rtex_event_json(e);            /* the event, display lists left out */
    if (rtex_event_kind(e) == RTEX_EVENT_PARAGRAPH_UPDATE) {
        size_t n;
        const uint8_t *dl = rtex_event_dl(e, 0, &n);   /* binary display list */
        /* ... */
    }
    rtex_event_free(e);
}
rtex_session_close(s);
```

The event JSON has the same fields as the Rust events above. Display lists are left out of it
and replaced by `{"bytes": n, "index": i}`. Fetch them as binary with `rtex_event_dl`
(`rtex_event_dl_count` of them; for a `LayoutUpdate`, in `pages_changed` order). If you'd
rather parse JSON, `rtex_dl_to_json` converts one. Strings returned as `char *` are yours; free
them with `rtex_string_free`. `examples/c/edit_loop.c` is a complete program.

## `rtex serve`

```sh
rtex serve --project /path/to/project
```

It reads one JSON command per line on stdin and writes replies and events, one JSON object per
line, on stdout:

```json
{"cmd":"edit","path":"main.tex","start":1234,"end":1234,"text":" world"}
{"cmd":"set_document","path":"chapters/intro.tex","text":"…whole file…"}
{"cmd":"spans","path":"main.tex"}
{"cmd":"status"}
{"cmd":"request_layout"}
{"cmd":"export_pdf","out":"out.pdf"}
{"cmd":"quit"}
```

Replies have a `"reply"` key (`"edit"`, `"spans"`, …, or `"error"`). Events have an `"event"`
key (`"ParagraphUpdate"`, `"LayoutUpdate"`, …) with the fields above. Display lists are inline,
in their JSON form.

## Versioning

rtex is at 0.0.x. The API, the C ABI, the display-list format and the engine protocol are
versioned together and may still change before 0.1. The binary display list carries a format
revision number (currently 1) in its header.
