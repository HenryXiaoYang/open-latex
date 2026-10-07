# Limitations (lode 0.0.2)

## What updates in real time, and what does not

"Real time" means the per-keystroke fast path (≈ 1 ms class): the edited paragraph is re-typeset
alone and its display list replaces it on screen immediately. Everything else is updated by the
background full compile, which takes as long as a LuaLaTeX run of the whole document (≈ 2 s for
10 pages, ≈ 5–15 s for 300 pages here, 2–3 passes when references change) and then arrives as a
`LayoutUpdate`. The host shows the edited source immediately in its editor pane either way; the
question is when the *typeset* view catches up.

| Content being edited | Typeset update | Why |
|---|---|---|
| Body text, font switches (`\emph`, `\textbf`, …), accents, colors | **real time** | fast path |
| Inline math `$…$` from the allow-listed vocabulary | **real time** | fast path |
| A paragraph that changes its line count (grows/shrinks) | paragraph in real time; following material after the next background pass | page breaks are global; `pagination_stale` says so |
| Display math `\[…\]`, equation environments | background (seconds) | ends the paragraph in TeX; not isolated by `\vbox` replay |
| Tables (`tabular`), lists, theorem-like environments | background | environments are background-only in 0.0.2 |
| Figures/images, captions, floats | background | float placement is global; `\includegraphics` paragraphs are not allow-listed |
| TikZ/PGF diagrams | background, and the page renders through the **PDF fallback** | `\pdfliteral` drawing is not representable in the display list |
| Footnotes, `\ref`/`\cite`/`\label`, counters, headings | background | page-global state |
| Preamble, packages, macro definitions | background after an engine restart (≈ 1–2 s) | the server must reload the preamble |
| Macros you defined yourself inside body text | background unless listed in `trusted_macros` | the allow-list cannot know they are pure |

So the paper's demo items map as follows: typing text and inline math — yes, real time; moving
paragraphs, adjusting tables, images and fonts "instantly" — not in 0.0.2, those are
background-path updates (correct, versioned, but seconds rather than milliseconds). Extending the
fast path to table cells, display-math blocks and figure boxes is possible with the same
mechanism (capture a unit's context, re-typeset it in isolation, overlay it) and is the main item
for a v2.

**Fast path scope.** Only body paragraphs built from the allow-list in `eligibility.rs` take the
per-keystroke path: plain text, font switches, inline math from a fixed vocabulary, colors,
accents and symbols. Everything else — including any macro defined in the preamble unless the
host lists it in `trusted_macros` — is typeset by the background path and arrives with the next
`LayoutUpdate` (seconds, not milliseconds). Display math, lists, footnotes, floats, headings,
`\ref`/`\cite`/`\label`, verbatim and environments are therefore not real-time.

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
otherwise `lode pdf-compare` ignores /ID, dates and producer and compares content streams, fonts
and images.

**Platforms.** Linux/macOS (FIFO transport). Windows named pipes are not implemented.

**Performance depends on the font stack.** With fontspec's default luaotfload node mode (and
more so with HarfBuzz), LuaTeX shapes every paragraph in Lua, which costs several times the
line-breaking time itself (docs/BENCHMARKS.md, E13). The 1 ms class is reached with TFM fonts
or OpenType fonts loaded with `Renderer=Basic`; node-mode documents still get paragraph-local,
document-size-independent updates, just slower per keystroke. Preamble changes restart the
engine (≈ 1–2 s with fontspec).
