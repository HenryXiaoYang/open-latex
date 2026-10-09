# Changelog

## 0.0.2 (in progress)

- **Picture cache**: background passes reuse unchanged `tikzpicture`/`circuitikz` drawings from
  an earlier pass's PDF (`SessionConfig.picture_cache`, default on; `rtex verify --pic-cache`;
  `fixtures/corpus/pictures`). Pictures are keyed by `file:line` with a start- and end-line check,
  hashed with the definitions before them in `\input` order (whole statements), compared against
  the font, color and width in force, and taken from per-page extracts of the pass PDF. A 111-page
  document with 120 pictures: 45 s → 18 s per pass. A cached picture is `UNSUPPORTED{cached_picture}`
  in the display list, never an image (hosts render the page from the PDF as for a drawn picture).
- Debugging: `SessionConfig::debug_dir` (`"debug_dir"`, `rtex serve --debug-dir`,
  `$RTEX_DEBUG_DIR`) writes a bundle per engine failure (request source, context, picture
  entries, server driver, preamble, TeX log, per-stage and per-font-load trace; `\tracingmacros`
  only with `$RTEX_TRACE_MACROS=1`, with a ten times longer watchdog, since it made a drawn
  pgfplots axis slow enough to trip the watchdog and kill the engine) and a `requests.log`; a server that fails to start leaves a bundle too. The debug extras
  install after the server's `ready` and under `pcall` (with luatexja loaded, wrapping
  luaotfload's font callback by name failed and kept the engine from ever starting).
- The live engine reuses cached pictures too: a unit that contains a picture the cache holds
  (same text, definitions and font/color/width state) is compiled with the cached region in
  place of the drawing, so editing a sentence that shares its unit with a pgfplots axis stays
  within the fast budget (compile frame field `<plen>`, result `pics_seen`/`pics_used`; shared
  `rtex-pic.lua`/`rtex-pic.tex`). The `cached_picture` item carries the picture's rectangle, so a
  host can carry the picture along when it redraws the unit live.
- A block environment the layout does not know yet (a blank line typed between a sentence and
  its `center`/`itemize`/`equation`, a new environment) borrows a neighbouring paragraph's
  context like a new paragraph does and is delivered live (`context_stale`, `approximate`,
  cached pictures in place); before, the split halted the fast path (`NoContext`) until the
  next pass. Floats, and units with vocabulary that only a probe against the layout could
  verify, still wait for the pass; a unit let through for its cached pictures is demoted when
  the engine drew one after all. A borrowed unit is placed after the nearest preceding span
  with rows, a heading included, at its left edge (it used to follow the nearest paragraph's
  last row, which put a paragraph typed after a heading over the heading, at the x of a
  centered display formula); a split's halves follow each other. A live placement that would
  fall off its page (above the top, past the bottom) is dropped and the unit waits for the
  pass, rather than drawing its rows and pictures at the page edge (a plot pinned to the page
  top, a figure past the last line). In probe mode a unit whose only unknown vocabulary is
  cached pictures (a sentence and its plot) is no longer probed after each layout: the probe
  compiled the snapshot's text with entries scanned from the current text, drew the plot when
  they did not match and, with the debug trace on, timed out and quarantined the unit.
- The server always answers: a Lua error of its own while finishing a compile (or in the
  context replay, the picture hooks or the request handler) used to be swallowed by TeX in
  batch mode, leaving the host to wait out the 5 s watchdog, kill the engine and quarantine
  the unit for the session (seen on macOS on plain paragraphs). It is now traced with its
  traceback and answered at once as an `internal` error result; the unit goes to the pass
  until the next layout and the engine keeps running. `finish` traces its stages, so a stall
  shows where it stopped (tests/internal_error.rs).
- Internal: `session.rs` split into `session/{mod,route,engine_loop,passes,diag,live}.rs`; the
  per-span facts of the live path (borrowed contexts, live rows and placements, counters seen,
  probe verdicts, budget marks, internal errors, leaks), ten maps with their own clearing rules,
  are one record per span with three explicit lifetimes (`session/live.rs`); the write-only
  `overlays` map is gone. The engine thread is an `EngineLoop` with one method per step, and
  the eligibility classifier a `Scan` with one method per construct (checked identical to the
  original over 1.6 million classifications).
- Clippy-clean workspace, enforced in CI with `-D warnings`, and `cargo fmt --check` enforced
  (it was not). API: `background::run_pass_with_runner` takes a `PassPlan` and
  `WarmEngine::spawn` a `StandbySpec` instead of eight arguments each; `IdAllocator::next` is
  `next_id`; `engine::Response::Result` holds a `Box<CompileResult>`.
- The host drains the server's response FIFO on a reader thread with blocking reads instead of
  polling it. On macOS the server blocked writing a 9 KB display list (more than the FIFO
  holds) while the host's `poll` never reported the frame: the watchdog killed the engine
  after 5 s and the paragraph was quarantined. The traced stages of a bundle showed it
  (traversal done, result never sent). tests/engine_robustness.rs produces a result larger
  than any FIFO and reads nothing for two seconds; the old transport fails it.
- Stress test (a 58-page LuaLaTeX document: luatexja, biblatex, imakeidx, glossaries, tcolorbox,
  pgfplots, pdfpages, pdflscape, verbatim and catcode tricks, `\include` chains; 28 scripted
  live-typing scenarios). Fixes:
  - Input PDFs (`\includegraphics{x.pdf}`, `\includepdf`) are copied into the pass snapshot;
    every `.pdf` was skipped as a build output, so the pass failed on them. Only a PDF next to
    the `.tex` it was built from is skipped now.
  - makeindex runs between passes on every `.idx` a pass writes (imakeidx's named indexes too)
    when its entries changed or its `.ind` is missing; the index was missing from every layout.
    `.ind` files are part of the aux signature. `.idx`/`.glo` are no longer copied between pass
    directories: TeX only writes them, and a standby engine may already hold them open.
  - The aux signature ignores the pass directory's path (plain and hex-encoded): bookmark
    records the absolute path of the `.ind` it read, which differs between `pass-0` and
    `pass-1`, so every layout ended in `PassLimitReached`.
  - `\begin{document}`/`\end{document}` count only where TeX reads them as commands, not in
    comments, `\verb` or verbatim-like environments, both in the segmenter and in
    `split_preamble`. A chapter that shows a document in `verbatim` was taken as preamble up to
    that point and as a trailer after it: each keystroke there restarted the engine.
  - Picture cache: a picture whose body reads part of itself under other catcodes (`luacode*`,
    `verbatim`, `\verb`, `\catcode`, `\directlua` …) or whose `\end{env}` the body skip cannot
    reach (brace balance after comment stripping) is not cached. A `luacode*` with `%.4f` in a
    `tikzpicture` made the skip run to the end of the file.
  - Page display lists are taken in `pre_shipout_filter` (registered at `\begin{document}`),
    after LaTeX added the `shipout/background`/`foreground` material: `\includepdf` pages and
    eso-pic content were missing from pages reported exact. A page with a `/Rotate` page
    attribute (pdflscape) is flagged `page_rotate` (PDF fallback).
  - Run-in headings (`\paragraph`, `\subparagraph`) and lists continuing an earlier list
    (enumitem `resume`, `series`) are background-only (`RunInHeading`, `OutsideState`, kept in
    probe mode): the first shares its rows with the following paragraph, the second numbered
    from 1 live where the document said 3.
  - `fast_budget` defaults to 50 ms (was 5): a demoted unit waits for a pass (20 s on this
    document), so an `align` of 15 ms per compile waited 20 s per keystroke.
  - biblatex citations are live in a session opened without an earlier build: the server read
    no `.bbl` at start and printed every citation as its key, so the probe kept each citing
    unit on the pass for the whole session. When a layout's `.bbl` differs from the server's,
    a server with it is started on a helper thread and swapped in when idle
    (tests/bibliography.rs).
  - glossaries' first-use state is replayed per unit: `\gls` sets `\ifglo@<label>@flag`
    globally, so after one live compile the server printed the short form where the document
    has its first use (the probe compile set it, and the delivered compile was already wrong).
    The capture tracks the switch of every entry the source defines (`\newglossaryentry`,
    `\newacronym`, `\newabbreviation`, `\loadglsentries` files; `\iftrue`/`\iffalse` meanings),
    the server sets it at the unit's value inside the compile and resets it globally after the
    group. Body-only passes also scan `rtex-preamble.tex` for definitions (the main file's
    preamble is blanked there).
- `LayoutUpdate.pdf_fallback` is a per-layout copy of the pass PDF (`build/bg/layout-N.pdf`) and is
  set whenever any page is degraded: the next pass no longer rewrites the file a host is reading.
  Background passes run in `build/bg/pass-0`/`pass-1` alternately (the aux family is carried
  forward): a standby engine wrote its log and PDF into the running pass's files, which produced
  corrupt, ever-growing PDFs on long documents (blank pages in pdf.js). `build/bg/<jobname>.pdf`
  and `.log` now name the latest delivered layout.
  Inline pictures stay in their paragraph (segmenter and eligibility); setup statements before a
  block environment no longer hide the environment's shape; the verification rasterizer rounds
  page sizes like MuPDF.
- Fast-path latency: raw-framed compile requests, `\luafunction` dispatch in the server, one
  state fingerprint per compile, bounded busy-poll for replies, direct submission from
  `apply_edit` when the server is idle, lazy context upload, windowed re-segmentation next to the
  preamble. Short-paragraph round trip through a session: 0.63 ms → 0.40 ms on the reference
  container (docs/BENCHMARKS.md).
- `rtex probe`: latency breakdown of the fast path (direct server, in-engine profile, session).
- Fast path generalized from paragraphs to **units**: paragraphs with display math (`\[ \]`,
  `equation`, amsmath `align`/`gather`/`multline`), footnote marks, `\ref`/`\eqref`/`\pageref`/
  `\cite`/`\label`; block environments (lists, `quote`, `center`, theorem-like, `figure`/`table`
  with `\includegraphics`, `tabular`, `booktabs`, `\caption`); headings. Counters, kernel
  switches, `\everypar` patterns and float-box state are captured and replayed; aux labels are
  uploaded to the server; images are mapped by resource index. Rows (hlists reached through
  vlists) replace lines as the unit of placement; fragments carry per-row positions anchored at
  each page's first row. A per-unit fast budget (5 ms) routes slow units to the background path.
- Display list: `LINE_UNIT`, `IMAGE_INFO` and `MATRIX` records (graphicx scaling is now exact,
  not degraded); unit lists carry an insert count.
- `rtex gen-book --variant units` fixture; `rtex verify` works per unit kind with `--units` and
  `--dump-rows`; `rtex bench` categories display-math/footnote/list/figure/table/heading with
  5 ms × h and size-independence gates.
- Measured on the reference container (100-page book, per keystroke): display-math paragraph
  1.33 ms, footnote paragraph 2.38 ms, list 1.60 ms, figure 1.91 ms, table 1.34 ms, heading
  0.95 ms; short paragraph 0.45 ms (0.20 ms amortized), medium 1.21 ms, long 2.26 ms.
- Engine: server-only shortcuts in `tex/latex/rtex-serve-patches.tex` (no running-head marks,
  memoized math font setup per size, cached graphics file probes, no log-only info messages),
  each guarded by a check of the definition it replaces and verified row-exact by `rtex verify`;
  traversal with cached advance widths and flat glyph runs (−30 %, byte-identical output);
  array fingerprint with reused token objects; contexts replay only the parameters that differ
  from the idle state. Per keystroke on the 100-page `units` book: footnote paragraph 2.38 →
  1.45 ms, heading 0.95 → 0.67 ms, figure 1.91 → 1.57 ms, list 1.60 → 1.32 ms, display-math
  paragraph 1.33 → 1.16 ms; long paragraph 2.26 → 1.79 ms, short 0.45 → 0.34 ms
  (docs/BENCHMARKS.md, `bench/results/roundtrip-20261008-0318.md`).
- `rtex probe` times one unit of every kind; `rtex verify` builds both references to aux
  convergence (labels resolved on both sides).
- Paragraph boundaries no longer leave the fast path: a split keeps the first half's id and
  context, the second half (and any paragraph typed fresh) borrows the context of its nearest
  paragraph neighbour and is placed after it; a merge keeps the first paragraph's id and
  announces the other as `ParagraphUpdate{status: "removed"}`. Spans that only border an edit
  are unchanged (typing at the top of the first paragraph no longer counts as a preamble change).
  A burst of preamble keystrokes restarts the engine once, after the debounce, instead of once
  per keystroke.
- Background passes run in a standby engine that has already loaded the preamble (body-only
  passes, 3–4× faster: 10-page fixture 1.9 s → 0.42 s per layout, a fontspec/tikz/hyperref
  document 2.3 s → 0.74 s); a bibliography run that changed nothing no longer forces a second
  pass. `warm_background` config key.
- Capture: paragraphs that start inside a group (`{\em …}`, `{\bfseries …}`) and bare
  `tabularx` tables had no rows (the unit attribute was restored with the group) and the wrong
  base font; both fixed, and `tabularx` is on the fast path.
- Fix: the project copy for background passes never created subfolders, so any project with a
  subfolder (`images/`, …) failed every full compile with "No such file or directory". A pass
  that cannot run now reports `LayoutUpdate` with `compile: Failed` instead of only a
  diagnostic. Fixtures keep their image in `images/` so the integration tests cover nested
  projects.
- Fix: an edit arriving while a layout was being installed could deadlock the session (the two
  threads took the file and layout locks in opposite orders). A unit whose live compile hung or
  crashed the engine is quarantined on the full compile until the preamble changes instead of
  killing the server again on the next keystroke.
- A span holding several consecutive paragraphs (an explicit `\par`, a title line with its own
  `\par` followed by a text line) is one composite unit on the fast path instead of
  "ParagraphBreak"/"NoContext" background-only.
- Fast-path coverage from a corpus of realistic documents (`fixtures/corpus`, gated in CI):
  macros defined in the preamble are trusted when their bodies are allow-listed; hyperref
  (`\url`, `\href`, `\autoref`), natbib citations, siunitx, ulem, soul, listings
  (`\lstinline`, `lstlisting`), `verbatim`, amsthm `proof`, setspace `spacing`, subcaption
  `subfigure`, multirow, colortbl, cancel, bm, boxes and rules, enumitem counter formats,
  `\newpage` and friends; `\setlength`/`\renewcommand{\arraystretch}` inside groups. The fast
  server reads the last pass's aux at `\begin{document}` (natbib's mode, hyperref), and
  hyperref's link/destination whatsits no longer degrade a unit.
- Multi-file projects: the session loads the main file and, transitively, every file it
  `\input`s/`\include`s (at open and when a new `\input` line appears); their paragraphs are
  fast-path units, an edit to a preamble `\input` file restarts the engine, the server's
  preamble has those files inlined, `\include` works with the output directory (its
  subdirectories are mirrored) and partial `.aux` files count towards convergence. Corpus
  fixture `multi` (14/14 live).
- Counters on the fast path: `\setcounter`/`\addtocounter`/`\stepcounter`/`\refstepcounter`
  inside a unit are allowed; the capture records the counters each unit advances, the server
  reports what a compile advanced (`counters` in the result), and the session schedules a layout
  pass only when that changes (an equation or item added, a counter set), so what follows is
  renumbered. hyperref `\texorpdfstring` and caption `\captionof` are allowed with their
  packages. Corpus fixture `counters` (9/10 live).
- User-defined environments: `\newenvironment`/`\renewenvironment` in the preamble whose
  begin/end code is allow-listed are fast-path material; one that opens a block environment
  becomes a unit environment (captured like a theorem), the others are typeset inside the
  paragraph that uses them. `\ ` (control space) is allowed.
- Citations with biblatex are live: the server loads the last pass's `.bbl` next to its aux,
  and biblatex's citation commands are allowed with the package. Manual bibliographies
  (`thebibliography`, `\bibitem`, `\newblock`) are block units. A macro several packages
  provide (`\citeauthor`, `\mathbb` via amsfonts/unicode-math) is allowed when any of them is
  loaded.
- Title block on the fast path (article/report without titlepage): `\title`/`\author`/`\date`/
  `\thanks`/`\and`/`\maketitle` are allowed, the span maps to the `center` unit the class
  builds, and the server restores the kernel's title macros after every compile. A unit that
  begins on the blank line after a span (hyperref's `\maketitle` reads ahead) maps to that
  span. Also allowed: `\S`, `\P`, `\dag`, `\ddag`, `\pounds`, guillemets, `\protect`,
  `\selectfont`, `\fontsize`/`\frenchspacing` inside groups, `\triangle`; with their packages
  `\xspace`, `\nicefrac`, `\ce` (mhchem), `\enquote` (csquotes), `\ding`, `\subfloat`.
- Setup after `\begin{document}` reaches the fast path: a body span made only of definitions
  and settings (`\newcommand`, `\def`, `\setlength`, `\renewcommand{\arraystretch}`,
  `\pagestyle`, `\lstset`, … `eligibility::SETUP_MACROS`) is loaded with the server's preamble,
  macros it defines are trusted, and editing it is a preamble change.
- `fast_budget_ms` is exposed everywhere the session is configured (C ABI JSON, `rtex serve
  --fast-budget-ms`, `SessionConfig::fast_budget`); default unchanged (5 ms).
- Research (docs/ELIGIBILITY.md): a universal `\globaldefs=-1` leak barrier breaks LaTeX
  internals (rejected); `rtex verify --permissive` ignores the allow-list and lets the row
  comparison judge every unit — 26 of 28 constructs the allow-list rejects are exact, and the
  comparison catches the two that are not. Research fixture `fixtures/research/permissive`.
- **Probe mode** (default): eligibility is decided by comparison, not vocabulary. A unit whose
  commands are not allow-listed is compiled once as the last pass typeset it and compared row
  by row with the pass; a match makes its edits live, a mismatch keeps it on the background
  path for that layout (`unverified:` reasons). The server reports leaked definitions
  (`leaks`), which demote the unit and restart the engine. `SessionConfig::eligibility`,
  C ABI `"eligibility"`, `rtex serve --eligibility`; `rtex verify --permissive --max-differing`
  gates it in CI (`--expect-differing`: the known state-dependent units must be caught
  exactly). Paragraph units reported on the blank line before a span map to that span.
- Macros the body (re)defines are captured per unit and replayed by the server (`macros` in
  the context: the capture scans the sources for `\renewcommand`/`\def`/`\let` names), so
  `\renewcommand{\arraystretch}` before one table applies to that table only, as in the pass.
  Setup statements are detected on the real segmentation (a definition inside a picture is
  not one). `tikzpicture`/`circuitikz`/`pgfpicture` are block units. Paragraphs opening with
  display math have a context (the unit's own record). Slow multi-pass runs deliver each
  finished pass as a provisional layout (docs/CONVERGENCE.md). Over-budget compiles are not
  counted while a layout pass is running.
- Fixes from a 111-page real-world document (TikZ, pgfplots, circuitikz, fancyhdr, xeCJK):
  a heading right after a paragraph's last line no longer loses that paragraph's last rows
  (the capture closes the unit after the heading's own `\par`); running heads and feet built
  inside the output routine are never attributed to the unit that happened to be open (page-top
  lists and `center` blocks carried the header rows); `\the<counter>` formats are captured per
  unit and replayed by the server (`\appendix` headings now number as the pass does); user
  macros wrapping a heading command have that heading's shape; `\rm`/`\bf`/… in groups and
  math, graphicx `\scalebox`/`\rotatebox`/`\resizebox`, and a batch of math arrows,
  integrals and symbols are allow-listed. Snapshots skip the build tree by canonical path.
  The install script gains pgfplots, circuitikz, xecjk, xypic, gensymb, regexpatch, haranoaji.
- Versioning reset: the library, protocol and documents are versioned together as 0.0.x; nothing
  is frozen before 0.1.

## 0.0.1

- Initial release: persistent LuaTeX paragraph server, capture package, display list (binary
  encoding revision 1 + JSON mirror), Rust core with C ABI, background passes with convergence
  contract, PDF export, fidelity verification, fixtures and benchmarks (milestones M0–M7).
