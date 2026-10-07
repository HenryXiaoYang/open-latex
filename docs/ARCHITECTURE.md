# Architecture

```
host app ──Rust API / C ABI──▶ Session (crates/lode-core/src/session.rs)
                                ├─ documents: FileBuf per file, spans with stable ParaIds (document.rs)
                                ├─ eligibility: allow-list + engine flags (eligibility.rs)
                                ├─ engine thread: one persistent lualatex paragraph server (engine.rs)
                                ├─ background thread: snapshot → instrumented passes → LayoutStore (background.rs, layout.rs)
                                └─ events: ParagraphUpdate / LayoutUpdate / Diagnostics / EngineState / BackgroundScheduled / PdfExported
tex/
  lode-serve.lua       serve loop inside \begin{document}: context replay, tex.runtoks, fingerprint, diagnostics
  lode-dl.lua          node-list traversal = LuaTeX backend cursor (verified 0 sp with the late_lua oracle)
  lode-capture.sty/lua paragraph contexts, line tags, shipout placements, page display lists, image map
```

## Fast path (per keystroke)

1. `apply_edit` updates the buffer, re-segments, keeps ids stable, bumps `source_revision`.
2. The touched span is checked: body span, allow-list clean, mapped to exactly one engine
   paragraph of the latest layout whose engine flags are clean.
3. A request is queued (latest per paragraph wins). The engine thread sends the context once per
   `(generation, context_revision)` and compiles; the server replays all line-breaking
   parameters, NFSS font state, `\parshape`, `\everypar` (allowed variants) and the starting
   color, typesets the source in a `\vbox` via `tex.runtoks`, checks its state fingerprint,
   traverses the box and returns the display list with stage timings.
4. The result is discarded if the span changed meanwhile; otherwise fragments are built from
   the cached per-line placements and a `ParagraphUpdate` is emitted. Line-count changes mark
   `pagination_stale` and schedule a background pass.

## Background path

Debounced passes on a snapshot copy; `lode-capture` records per paragraph the context and the
source lines, tags line boxes, and at shipout writes placements and page display lists. The
layout store maps engine paragraphs to spans by snapshot line ranges, diffs page hashes, and
emits `LayoutUpdate` with the convergence state (CONVERGENCE.md). Degraded pages carry a PDF
fallback path.

## Guarantees and their evidence

- Display lists equal the engine's own output positions: `docs/FIDELITY.md` (backend oracle,
  extractor cross-check, independent PDF parser, rendered comparison).
- Per-keystroke work never touches pages: the server has no notion of the document beyond the
  preamble and the replayed context (benchmarks in `docs/BENCHMARKS.md`).
- Nothing outside a paragraph can be changed by a fast-path paragraph: eligibility is an
  allow-list (`eligibility.rs`), and the engine refuses state changes (fingerprint).
