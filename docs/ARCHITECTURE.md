# Architecture

```
host app ──Rust API / C ABI──▶ Session (crates/rtex-core/src/session/mod.rs: API, shared state)
                                ├─ documents: FileBuf per file, spans with stable ParaIds (document.rs)
                                ├─ routing: live or not, own or borrowed context, picture entries (session/route.rs)
                                │  over the allow-list and capture facts (eligibility.rs)
                                ├─ engine thread: one persistent lualatex unit server (engine.rs), results into
                                │  updates or demotions (session/engine_loop.rs); apply_edit writes to it directly
                                │  when it is idle
                                ├─ background thread: snapshot → instrumented passes → LayoutStore
                                │  (session/passes.rs, background.rs, layout.rs)
                                ├─ live state: one record per span with its expiry rules (session/live.rs)
                                ├─ diagnostics: debug bundles, requests.log, TeX log parsing (session/diag.rs)
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
   plain paragraph or a block environment (not a float) the layout does not know yet (a split's
   second half, a paragraph or environment typed fresh, a blank line typed before a `center`)
   whose vocabulary is allow-listed, or whose only non-allow-listed content is picture
   environments the picture cache holds (in probe mode nothing else can be verified without a
   layout unit), **borrows** the context of the nearest paragraph unit before it (after it when there is none):
   same parameters, fonts and counters, with the paragraph-start state of a paragraph that follows
   a paragraph (or a heading when a heading span precedes it); an environment is told from a
   paragraph by the context's `kind`/`name`. Its rows are placed after the nearest span before it
   that has rows (a heading, an environment, a paragraph, or a unit itself live on a borrowed
   context such as the other half of a split), at that span's left edge (exact for consecutive
   paragraphs with the same baselineskip and no parskip), `approximate` and `context_stale`; the next layout replaces the borrowed context with a
   captured one. A float typed fresh waits for the next pass (its box state comes from the
   capture only). Any live placement (borrowed or anchored) whose rows would fall off the page
   they land on is dropped, so the unit waits for the pass instead of drawing at the page edge;
   a host renders that page from the last pass until then.
3. The request goes straight to the server when it is idle and already holds the unit's context
   (no engine-thread wake-up on the keystroke path); otherwise it is queued, latest per unit. The
   server replays the context, typesets the source in a `\vbox`, restores the counters, checks
   its state fingerprint, traverses the box and returns the display list with stage timings.
   When the source contains picture environments the picture cache holds (same hash, same
   font/color/width state), the request carries their entries, each keyed by the source line its
   `\begin` starts on, and the server places the cached regions instead of drawing
   (`rtex-pic.lua`, shared with the capture; a picture without an entry is drawn): a sentence followed by a
   pgfplots axis in one unit costs the sentence, not the plot (280 ms → ~1 ms on a real-world
   document). The engine reports how many pictures it began; a count that differs from the
   source scan (a picture made by a macro) repeats the compile without the cache.
4. The result is discarded if the span changed meanwhile; otherwise fragments are built from the
   cached row placements (page positions anchored at each page's first row, the fast box's own
   geometry within the page) and a `ParagraphUpdate` is emitted. Row-count changes mark
   `pagination_stale`; inserts (footnote text) and degraded content are listed in `reasons`;
   both schedule a background pass. A unit whose compiles exceed `fast_budget` (`fast_budget_ms`, default 50) three times in
   a row leaves the fast path until the next layout (`OverBudget`; the first slow compiles are
   forgiven because they may be loading fonts).
5. The server starts from the latest layout's `.aux` and `.bbl`. Labels are refreshed with each
   layout (`labels` request), but biblatex reads its `.bbl` once, at `\begin{document}`: when a
   layout's `.bbl` differs from the one the server read (a session opened without an earlier
   build, an edited `.bib`), the engine thread starts a server with it on a helper thread
   (`build/serve-next`, generation + 1) while the running one keeps serving, and swaps it in once
   ready and idle (`EngineState Ready`, reason "restarted with the bibliography of the latest
   layout"); queued requests carry over and probe verdicts are cleared.

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
Passes write to `build/bg/pass-0` and `pass-1` alternately, the slot of the snapshot directory
they run from: a standby opens its log, and with some preambles its PDF, the moment it starts, so
it must never share a directory with the pass that is running (two writers once produced PDFs
with gaps that renderers showed as blank pages). Before a pass is released, the previous pass's
aux family (`.aux .toc .bbl …`, chapter `.aux` files included) is copied into its directory.
Degraded pages carry a PDF fallback path: `build/bg/layout-<layout_version>.pdf`, a copy of the
pass PDF made before the layout is delivered (the two previous layouts' copies are kept, older ones
are removed); `build/bg/<jobname>.pdf` is a link to the latest one and `<jobname>.log` a copy of its
log, for hosts that name these files themselves. Nothing writes those files in place. The path is
given whenever any page of the layout is degraded, changed in this layout or not.

**Picture cache.** Drawings (`tikzpicture`, `circuitikz`) dominate a pass over a document that
has many of them, and almost none of them change between two passes. Each pass records where
every picture environment landed (`pics` in the capture JSON: `file:line` → page, position, box;
the capture's wrapper of the environment's begin macro tags the picture's output boxes with an
attribute, set while the environment runs so every node it makes inherits it and nothing made
before it does, and `rtex-dl.lua` reports the union of the outermost tagged boxes at shipout).
After a pass the session extracts the pages that carry new pictures from the pass PDF into
`build/bg/pic-cache/` and indexes each picture by a hash of its text, the preamble and the
definitions (`\def`, `\newcommand`, `\tikzset`, `\pgfplotsset`, …, whole statements) made before
it in the body, in the order TeX reads them (`\input` chains followed). Before the next pass it
writes `pic-manifest.json`: every current picture whose hash the cache holds, keyed by the
`file:line` of its `\begin`. The capture then **gobbles the body** of such a picture (scanning to
its `\end`) and puts an `img.node` of the recorded region of the earlier PDF in its place, an
image of exactly the picture's width, height and depth, so every placement is identical to a pass
that draws it; the page is degraded (`pic_cache` flag) and the host renders it from the pass PDF,
where the region is embedded as a form. Two checks guard the key: the line the `\begin` executes
on must start with it (a picture inside a macro argument executes on the argument's last line and
is drawn), and the skipped body must end on the line the scan found the `\end` on (otherwise the
pass reports the key and the cache forgets it). What the picture inherits from its surroundings without showing it in its
text, the current font (by name and size), color, `\hsize` and `\linewidth`, is recorded with the
picture and compared before a cached copy is used: a picture inside `{\small …}` is drawn again
when that becomes `\Large`, and the new drawing replaces the cached one. A picture that mentions `\ref`, `\cite`, `\label`, counters, `\today`,
`remember picture`/`overlay`, `\includegraphics`, `\input`, `\verb`, data files (`\addplot
table`/`file`), counters and links (`\thesection`, `\thepage`, `\href`, `\footnotemark`),
pgf material outside the picture box (`trim axis left/right`), or assignments that escape its
group (`\global`, `\xdef`, `\savebox`, `\pgfdeclarelayer` …) is never cached, nor is one whose
output spans lines or pages, one whose `\begin` does not start its line, pictures of a file
`\input` twice, or anything when the sources (the preamble included) mention `remember picture`.
Entries unused for four passes are evicted; a picture whose cached body did not end where the
scan said is drawn for four passes before it is cached again. In probe mode a unit whose only
vocabulary beyond the allow-list is picture environments the cache holds is not probed (a probe
compiles the snapshot's text, whose pictures the current entries need not match, and would draw
them); a compile of such a unit that draws a picture after all is demoted. `rtex verify --pic-cache` builds a cache from a converged pass, runs one
more pass on it and checks units, placements and (with `--raster`) the rendered pages. On the
reference container a 111-page document with 120 pictures passes in 18 s instead of 45 s. The
live engine uses the same cache (fast path, step 3), so a paragraph that shares its unit with a
picture stays within the fast budget.

## Debugging an engine failure

`SessionConfig::debug_dir` (C ABI `"debug_dir"`, `rtex serve --debug-dir`, or `$RTEX_DEBUG_DIR`)
turns on diagnostics. Every engine failure (watchdog, state fingerprint mismatch, crash, protocol
error) writes `<debug_dir>/engine-<time>-g<generation>-par<id>/` with `report.json` (reason,
unit, versions, timeout, server banner), `source.tex` (the exact text sent), `context.json`,
`pics.json` (picture cache entries), and the server's driver, preamble copy, TeX log and
`rtex-serve-g<generation>.trace`: one flushed line per request stage (`begin`, `mark` after the
context replay, `finish`) and per font load with its cost, so a hung request shows the last stage
it reached and whether a font was loading, even though the killed process's log tail is lost
(LuaTeX ignores no signal politely: SIGINT ends it without a word, so the watchdog kills
outright). `finish` traces its stages too (counters, leak check, fingerprint, traversal, result
sent). Every Lua function TeX calls during a compile (`apply`, `finish`, the picture hooks) and
the request handler run protected: a Lua error of the server's own (in batch mode TeX would
swallow it and the server would answer nothing, so the host waited out the watchdog, killed
the engine and quarantined the unit) is traced with its traceback and answered at once with
an `internal` error result. The host sends that unit to the pass until the next layout (no
restart, no quarantine, one debug bundle per unit and layout; a warm-up's internal error
schedules nothing, so it cannot loop passes). With `$RTEX_TRACE_MACROS=1` as well (opt-in), the server also runs with
`\tracingmacros=1`, so a macro loop shows in the log's last flushed block; it makes a heavy
compile (a pgfplots axis drawn) many times slower and the log grows without bound, so the
watchdog is ten times longer while it is on. `requests.log` gets one line per live compile (unit,
status, rows, TeX and total time, cached pictures). The event's `reason` names the bundle.

## Guarantees and their evidence

- Display lists equal the engine's own output positions: `docs/FIDELITY.md` (backend oracle,
  extractor cross-check over every eligible unit kind, independent PDF parser, rendered
  comparison).
- Per-keystroke work never touches pages: the server has no notion of the document beyond the
  preamble and the replayed context (benchmarks in `docs/BENCHMARKS.md`).
- Nothing outside a unit can be changed by a fast-path unit: eligibility is an allow-list
  (`eligibility.rs`), counters are restored, and the engine refuses state changes (fingerprint).
