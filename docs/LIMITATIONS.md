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
| Body text, font switches (`\emph`, `\textbf`, …), accents, colors | **real time** | fast path |
| Inline math `$…$` and display math `\[…\]`, `equation`, `align`, `gather`, `multline` (amsmath) from the allow-listed vocabulary, with `\label`/`\tag` and equation numbers | **real time** | paragraph unit; counters replayed |
| `\ref`, `\eqref`, `\pageref`, `\label`, plain `\cite` | **real time** with the numbers of the last layout | labels from the last pass's aux; biblatex/natbib citations are background |
| Footnotes | mark in **real time**; the footnote text at the page bottom after the next pass | inserts are placed by the page builder (`reasons: inserts`) |
| Lists (`itemize`, `enumerate`, `description`), `quote`/`quotation`/`verse`, `center`, `abstract`, theorem-like environments | **real time** as one unit | environment unit |
| Figures and tables (`figure`/`table` with `\includegraphics`, `tabular`, `booktabs` rules, `\caption`) | content in **real time** at the float's last position; a moved float after the next pass | float unit, float-box state replayed; placement is page-global |
| Headings (`\chapter`, `\section`, …) | **real time**; TOC and running heads after the next pass | heading unit |
| A unit that changes its row count (grows/shrinks) | unit in real time (rows placed with its own geometry); following material after the next pass | page breaks are global; `pagination_stale` says so |
| Splitting a paragraph (Enter + blank line), merging two, typing a new paragraph | **real time**: the known half keeps its context, the new paragraph borrows its neighbour's (counters may lag by one until the next pass) and is placed right after it | borrowed contexts are `context_stale`, placements `approximate` |
| `tabularx` (with the package loaded) | **real time** | inner environment like `tabular` |
| TikZ/PGF diagrams, `\pdfliteral` drawing | background, and the page renders through the **PDF fallback** | not representable in the display list |
| `minipage`/`parbox` blocks, `\marginpar`, `\verb`, verbatim | background | not allow-listed |
| Preamble, packages, macro definitions | background after an engine restart (≈ 1–2 s) | the server must reload the preamble |
| Macros you defined yourself inside body text | background unless listed in `trusted_macros` | the allow-list cannot know they are pure |
| Any unit whose fast compiles exceed `fast_budget` (5 ms by default) three times in a row | background until the next layout | the real-time budget is enforced per unit; a unit's first slow compiles (font loading) are forgiven |

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

**Documents.** One main file is tracked by the session today; `\input`/`\include`d files are
compiled (the project directory is snapshotted) but edits to them are not routed to the fast path.
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
