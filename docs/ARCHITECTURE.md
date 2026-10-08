# Architecture

```
host app ──Rust API / C ABI──▶ Session (crates/rtex-core/src/session.rs)
                                ├─ documents: FileBuf per file, spans with stable ParaIds (document.rs)
                                ├─ eligibility: allow-list over the unit's source + capture facts (eligibility.rs)
                                ├─ engine thread: one persistent lualatex unit server (engine.rs); apply_edit
                                │  writes to it directly when it is idle
                                ├─ background thread: snapshot → instrumented passes → LayoutStore (background.rs, layout.rs)
                                └─ events: ParagraphUpdate / LayoutUpdate / Diagnostics / EngineState / BackgroundScheduled / PdfExported
tex/
  rtex-serve.lua       serve loop inside \begin{document}: context replay (parameters, fonts, counters,
                       kernel switches, \everypar, float-box state), state fingerprint, labels, diagnostics
  rtex-dl.lua          node-list traversal = LuaTeX backend cursor (verified 0 sp with the late_lua oracle)
  rtex-capture.sty/lua units, paragraph contexts, row tags, shipout placements, page display lists, image map
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
0) or at a sectioning command, and sets the `rtex_unit` attribute so every node the unit creates
carries it; at shipout each page's rows are attributed to their units wherever the page builder
put them (floats, display rows, list lines, split pages). Headers and footers are built with the
attribute unset (`\@outputpage` hook); footnote text rows belong to no unit. The host's segmenter
produces the same regions (blank lines, headings, environment ends) so that each span maps to
exactly one unit.

A source span (text between blank lines) that the engine typesets as several consecutive
paragraphs, such as `{\Large\bfseries Title\par}` followed by a line of text, or a paragraph
with an explicit `\par`, maps to one **composite** unit: the capture units' rows are concatenated
in order, the first one provides the context, and the fast path typesets the span as one box.

## Fast path (per keystroke)

1. `apply_edit` updates the buffer, re-segments a window around the edit, keeps ids stable, bumps
   `source_revision`. Across a boundary change the first new span keeps the id of the first old
   span when it starts at the same byte (a split's first half, a merged paragraph), spans that
   only border the edit keep theirs, and the rest get fresh ids; removed spans are announced as
   `ParagraphUpdate{status: "removed"}` so hosts clear them.
2. Every touched or created span is classified (paragraph / environment / heading) and checked
   against the allow-list; it must map to exactly one unit of the latest layout whose capture
   facts are clean (replayable `\everypar`, no direction changes, placed rows, a context). A
   plain paragraph the layout does not know yet (a split's second half, a paragraph typed fresh)
   **borrows** the context of the nearest paragraph unit before it (after it when there is none):
   same parameters, fonts and counters, with the paragraph-start state of a paragraph that follows
   a paragraph (or a heading when a heading span precedes it). Its rows are placed right after the
   parent's current rows (exact for consecutive paragraphs with the same baselineskip and no
   parskip), `approximate` and `context_stale`; the next layout replaces the borrowed context with
   a captured one.
3. The request goes straight to the server when it is idle and already holds the unit's context
   (no engine-thread wake-up on the keystroke path); otherwise it is queued, latest per unit. The
   server replays the context, typesets the source in a `\vbox`, restores the counters, checks
   its state fingerprint, traverses the box and returns the display list with stage timings.
4. The result is discarded if the span changed meanwhile; otherwise fragments are built from the
   cached row placements (page positions anchored at each page's first row, the fast box's own
   geometry within the page) and a `ParagraphUpdate` is emitted. Row-count changes mark
   `pagination_stale`; inserts (footnote text) and degraded content are listed in `reasons`;
   both schedule a background pass. A unit whose compiles exceed `fast_budget` (5 ms) three times in
   a row leaves the fast path until the next layout (`OverBudget`; the first slow compiles are
   forgiven because they may be loading fonts).

### Server-only shortcuts (`tex/latex/rtex-serve-patches.tex`)

The server inputs a small patch file after `\begin{document}`. Each patch removes work whose
result cannot be observed in a unit box, or memoizes a deterministic computation, and each one
checks that the macro it replaces still has the definition it was written against (`\ifx`
against a copy of the expected body) and is skipped otherwise:

- `\markright`/`\markboth` keep only their typesetting side effect (a `\nobreak` after a
  heading in vertical mode) and insert no `\marks` nodes: the box is never shipped, so running
  heads never read them. Saves ≈ 150 µs per `\section`, ≈ 225 µs per `\chapter` (two or three
  `\mark_insert:nn` calls).
- `\glb@settings` memoizes the math font assignments per (math version, size), reused while the
  `\mv@<version>` list is token-identical. The kernel rebuilds them (three `\pickup@font` per
  math group, about forty per call, each running microtype's font hook) every time math is
  entered at a size other than the last one, which is what a footnote mark does twice. Saves
  ≈ 0.7 ms per footnote paragraph with microtype loaded.
- graphics: the two file-existence probes per `\includegraphics` (`\IfFileExists` in
  `\Gin@getbase`, `\openin` in `\Gread@pdftex`) are remembered for files that were found; the
  image resource itself was already cached by `luatex.def`. `\Gin@log` and `\GenericInfo`
  (log-only messages) are dropped. Saves ≈ 170 µs per figure.

`rtex verify` compares every unit kind row-exactly against a background pass that runs without
these patches, which is the evidence that they do not change typesetting (FIDELITY.md).

## Background path

Debounced passes on a snapshot copy, each run in a **standby engine**: a lualatex started while
the user types that has already processed `\RequirePackage{rtex-capture}` and the preamble (from
`rtex-preamble.tex`, the text before `\begin{document}`) and blocks in `tex/rtex-bg.lua` until the
session writes `GO`; it then inputs the body, written as `main.tex` with one empty line per
preamble line so every line number and file name the capture records is the original. Engine
start, format and preamble are ~75 % of a pass over a short document, so a body-only pass is
3–4× faster (10-page fixture: 1.9 s first layout, 0.42 s thereafter). When a layout needs more
passes, the next standby starts as the previous one is released (two snapshot directories
alternate), so its preamble loads while the body is typeset. A preamble edit drops the standby;
`SessionConfig.warm_background = false` restores plain runs. `rtex-capture` records per unit the
context and the rows,
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
