# Architecture

```
host app ──Rust API / C ABI──▶ Session (crates/lode-core/src/session.rs)
                                ├─ documents: FileBuf per file, spans with stable ParaIds (document.rs)
                                ├─ eligibility: allow-list over the unit's source + capture facts (eligibility.rs)
                                ├─ engine thread: one persistent lualatex unit server (engine.rs); apply_edit
                                │  writes to it directly when it is idle
                                ├─ background thread: snapshot → instrumented passes → LayoutStore (background.rs, layout.rs)
                                └─ events: ParagraphUpdate / LayoutUpdate / Diagnostics / EngineState / BackgroundScheduled / PdfExported
tex/
  lode-serve.lua       serve loop inside \begin{document}: context replay (parameters, fonts, counters,
                       kernel switches, \everypar, float-box state), state fingerprint, labels, diagnostics
  lode-dl.lua          node-list traversal = LuaTeX backend cursor (verified 0 sp with the late_lua oracle)
  lode-capture.sty/lua units, paragraph contexts, row tags, shipout placements, page display lists, image map
```

## Units

The fast path works on **units**: the regions a document is made of that can be re-typeset in
isolation and overlaid on the page.

| Unit kind | Source shape | Rows |
|---|---|---|
| `par` | a top-level paragraph, including inline and display math (`\[ \]`, `equation`, `align`, …), footnotes marks, `\ref`/`\eqref`/`\pageref`/`\cite`, and a trailing block environment | its text lines and display rows |
| `env` | a block environment started in vertical mode: lists, `quote`/`quotation`/`verse`, `center`/`flush*`, `abstract`, theorem-like environments from `\newtheorem`, floats (`figure`, `table`) with images, `tabular` and captions | every hlist reached through vlists only (item lines, image line, caption lines, the tabular's line) |
| `heading` | one sectioning command | the heading's line(s) |

The capture package opens a unit at `para/begin` (nest 1), at a block environment's begin (nest
0) or at a sectioning command, and sets the `lode_unit` attribute so every node the unit creates
carries it; at shipout each page's rows are attributed to their units wherever the page builder
put them (floats, display rows, list lines, split pages). Headers and footers are built with the
attribute unset (`\@outputpage` hook); footnote text rows belong to no unit. The host's segmenter
produces the same regions (blank lines, headings, environment ends) so that each span maps to
exactly one unit.

## Fast path (per keystroke)

1. `apply_edit` updates the buffer, re-segments a window around the edit, keeps ids stable, bumps
   `source_revision`.
2. The touched span is classified (paragraph / environment / heading) and checked against the
   allow-list; it must map to exactly one unit of the latest layout whose capture facts are
   clean (replayable `\everypar`, no direction changes, placed rows, a context).
3. The request goes straight to the server when it is idle and already holds the unit's context
   (no engine-thread wake-up on the keystroke path); otherwise it is queued, latest per unit. The
   server replays the context, typesets the source in a `\vbox`, restores the counters, checks
   its state fingerprint, traverses the box and returns the display list with stage timings.
4. The result is discarded if the span changed meanwhile; otherwise fragments are built from the
   cached row placements (page positions anchored at each page's first row, the fast box's own
   geometry within the page) and a `ParagraphUpdate` is emitted. Row-count changes mark
   `pagination_stale`; inserts (footnote text) and degraded content are listed in `reasons`;
   both schedule a background pass. A unit whose compile exceeds `fast_budget` (5 ms) leaves the
   fast path until the next layout (`OverBudget`).

## Background path

Debounced passes on a snapshot copy; `lode-capture` records per unit the context and the rows,
tags line boxes, and at shipout writes placements and page display lists. The layout store maps
units to spans by snapshot line ranges, diffs page hashes, extracts the aux labels for the
server's `\ref`/`\cite`, and emits `LayoutUpdate` with the convergence state (CONVERGENCE.md).
Degraded pages carry a PDF fallback path.

## Guarantees and their evidence

- Display lists equal the engine's own output positions: `docs/FIDELITY.md` (backend oracle,
  extractor cross-check over every eligible unit kind, independent PDF parser, rendered
  comparison).
- Per-keystroke work never touches pages: the server has no notion of the document beyond the
  preamble and the replayed context (benchmarks in `docs/BENCHMARKS.md`).
- Nothing outside a unit can be changed by a fast-path unit: eligibility is an allow-list
  (`eligibility.rs`), counters are restored, and the engine refuses state changes (fingerprint).
