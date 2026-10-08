# Limitations (rtex 0.0.2)

## What updates in real time, and what does not

"Real time" means the per-keystroke fast path (≈ 1 ms class): the edited paragraph is re-typeset
alone and its display list replaces it on screen immediately. Everything else is updated by the
background full compile, which takes as long as a LuaLaTeX run of the whole document (≈ 2 s for
10 pages, ≈ 5–15 s for 300 pages here, 2–3 passes when references change) and then arrives as a
`LayoutUpdate`. The host shows the edited source immediately in its editor pane either way; the
question is when the *typeset* view catches up. A layout after the first costs a body-only pass
in the standby engine (ARCHITECTURE.md), about a quarter of a full LuaLaTeX run.

| Content being edited | Typeset update | Why |
|---|---|---|
| Body text, font switches (`\emph`, `\textbf`, …), accents, colors, boxes (`\fbox`, `\parbox`, `\colorbox`, `\underline`, `\rule`), `\url`/`\href`/`\autoref` (hyperref), `\sout`/`\uline` (ulem), siunitx `\SI`/`\num`/`\si`, `\verb`, `\lstinline` | **real time** | fast path; package macros need their package loaded |
| Macros you define in the preamble (`\newcommand`, `\DeclareMathOperator`, parameterless `\def`) whose bodies use only allow-listed material | **real time** | trusted automatically, in text and/or math mode as their body classifies |
| Inline math `$…$` and display math `\[…\]`, `equation`, `align`, `gather`, `multline` (amsmath) from the allow-listed vocabulary, with `\label`/`\tag` and equation numbers | **real time** | paragraph unit; counters replayed |
| `\ref`, `\eqref`, `\pageref`, `\label`, plain and natbib `\cite`/`\citep`/`\citet`/`\citeauthor`/`\citeyear` | **real time** with the numbers of the last layout | labels and `\bibcite` from the last pass's aux, which the server also reads at `\begin{document}`; biblatex citations are background |
| Footnotes | mark in **real time**; the footnote text at the page bottom after the next pass | inserts are placed by the page builder (`reasons: inserts`) |
| Lists (`itemize`, `enumerate`, `description`, enumitem labels), `quote`/`quotation`/`verse`, `center`, `abstract`, theorem-like environments and `proof`, `verbatim`, `lstlisting`, setspace `spacing` | **real time** as one unit | environment unit; verbatim bodies are opaque to the allow-list |
| `subfigure`/`subtable` in a float, `tabularx`, `multirow`, colortbl/xcolor `\rowcolor`/`\cellcolor`/`\rowcolors` | **real time** inside their float or table unit | inner environments and table macros |
| `\setcounter`/`\addtocounter`/`\stepcounter`/`\refstepcounter` inside a paragraph or environment, hyperref `\texorpdfstring`, caption `\captionof` | **real time** | the server reports the counters a compile advanced and the session schedules a layout pass when that changes (what follows renumbers); a counter command *before* the unit's first text runs before its counters are captured and is background-only (`LeadingCounter`) |
| Environments defined in the preamble (`\newenvironment`/`\renewenvironment`) whose begin/end code is allow-listed | **real time** | one that opens a block environment (`quote`, a list, `center`, a theorem) is a unit of its own, like a theorem; otherwise it is typeset inside the paragraph that uses it. Code with `\def`/`\let`/`\global`, TikZ or other disallowed macros keeps the environment background-only |
| biblatex citations (`\cite`, `\parencite`, `\textcite`, `\autocite`, `\footcite`, …) | **real time** | the server loads the last pass's `.bbl` next to its aux, so biblatex resolves citations as the layout did; `\printbibliography` is generated material (background) |
| Manual bibliographies (`thebibliography` with `\bibitem`/`\newblock`) | **real time** | one unit: the heading the environment prints and its items |
| Figures and tables (`figure`/`table` with `\includegraphics`, `tabular`, `booktabs` rules, `\caption`) | content in **real time** at the float's last position; a moved float after the next pass | float unit, float-box state replayed; placement is page-global |
| Headings (`\chapter`, `\section`, …) | **real time**; TOC and running heads after the next pass | heading unit |
| A unit that changes its row count (grows/shrinks) | unit in real time (rows placed with its own geometry); following material after the next pass | page breaks are global; `pagination_stale` says so |
| Several paragraphs in one source span (an explicit `\par`, a title line `{\Large\bfseries …\par}` followed by a text line) | **real time** as one unit | consecutive paragraph units of a span are merged into one composite unit |
| Splitting a paragraph (Enter + blank line), merging two, typing a new paragraph | **real time**: the known half keeps its context, the new paragraph borrows its neighbour's (counters may lag by one until the next pass) and is placed right after it | borrowed contexts are `context_stale`, placements `approximate` |
| `tabularx` (with the package loaded) | **real time** | inner environment like `tabular` |
| TikZ/PGF diagrams, `\pdfliteral` drawing | background, and the page renders through the **PDF fallback** | not representable in the display list |
| `\maketitle`, `\tableofcontents`, `\bibliography` | background | generated material (title block, TOC entries, bibliography) is not typed in place |
| `\marginpar`, `wrapfigure`, `\setlength`/`\renewcommand` at a unit's top level | background | would change the state the following units are typeset in (inside a group or environment they are fine) |
| Preamble, packages, macro definitions | background after an engine restart (≈ 1–2 s) | the server must reload the preamble |
| Macros you defined yourself whose bodies are not allow-listed (`\def` with parameters, TikZ, `\global` …) | background unless listed in `trusted_macros` | the allow-list cannot know they are pure |
| Any unit whose fast compiles exceed `fast_budget` (5 ms by default) three times in a row | background until the next layout | the real-time budget is enforced per unit; a unit's first slow compiles (font loading) are forgiven |
| A unit whose live compile hung (watchdog, 5 s) or crashed the engine | background until the preamble changes (`EngineFailed`) | retrying would kill the server on every keystroke; `rtex verify` on the project shows the engine error for that unit |

The fast server differs from a document run in two observable ways, both outside the unit box:
it inserts no running-head marks (`\markright`/`\markboth` keep only their `\nobreak`), and
package info messages (`\GenericInfo`, `\Gin@log`) do not reach its log. It also memoizes the
kernel's math font setup per size (ARCHITECTURE.md, "Server-only shortcuts"); a document that
redefines `\glb@settings` or `\Gin@getbase` gets the stock definitions, because every shortcut
checks the definition it replaces.

Every real-time item above is verified by `rtex verify`: the fast result of each eligible unit is
compared scaled-point-exact with the rows of the same unit on the shipped page
(`docs/FIDELITY.md`). Timings per unit kind are in `docs/BENCHMARKS.md`.

**Fast path scope.** Units built from the allow-list in `eligibility.rs` take the per-keystroke
path (see the table). Everything else — including any macro defined in the preamble unless the
host lists it in `trusted_macros`, and any environment not in the list — is typeset by the
background path and arrives with the next `LayoutUpdate` (seconds, not milliseconds). Text that
continues after a block environment inside the same span, and headings sharing a span with text,
are background-only (the capture closes the unit at the environment's end).

**Documents.** The session tracks the main file and, transitively, every file it `\input`s,
`\include`s or `\subfile`s (loaded from disk at open and when a new `\input` line appears;
hosts hand over unsaved buffers with `set_document`). Paragraphs in those files are fast-path
units like any other; an edit to a file the preamble `\input`s is a preamble change (engine
restart). Files named through macros (`\input{\chapterdir/x}`), `\includeonly` and
`\import` are not followed.
LaTeX only (the paragraph hooks `para/begin` and `shipout/before` are LaTeX kernel hooks, 2021+).
LuaTeX only; no pdfTeX/XeTeX.

**Rendering.** Display lists name font files and glyph indices; hosts need an OpenType/TrueType
renderer. Type1 fonts (classic Computer Modern math without `unicode-math`) are named by file and
character code and must be rasterized by the host (the built-in verification rasterizer skips
them). Pages containing `\pdfliteral`/`\special` drawing, non-left-to-right text, `\vadjust`
material, unknown whatsits or unexpanded virtual-font commands are marked *Degraded* and come
with a PDF fallback path; TikZ/PGF pictures therefore render through the PDF fallback, not the
display list.

**Bibliographies and indices.** `biber` and `bibtex` run automatically; `makeindex`, `xindy` and
glossaries do not (documented hook point: `background.rs::run_pass`).

**Determinism of exports.** Export equality with a clean build is byte-exact only when the
document suppresses optional PDF info (the fixtures set `\pdfvariable suppressoptionalinfo 1023`);
otherwise `rtex pdf-compare` ignores /ID, dates and producer and compares content streams, fonts
and images.

**Platforms.** Linux/macOS (FIFO transport). Windows named pipes are not implemented.

**Performance depends on the font stack.** With fontspec's default luaotfload node mode (and
more so with HarfBuzz), LuaTeX shapes every paragraph in Lua, which costs several times the
line-breaking time itself (docs/BENCHMARKS.md, E13). The 1 ms class is reached with TFM fonts
or OpenType fonts loaded with `Renderer=Basic`; node-mode documents still get paragraph-local,
document-size-independent updates, just slower per keystroke. Preamble changes restart the
engine (≈ 1–2 s with fontspec).
