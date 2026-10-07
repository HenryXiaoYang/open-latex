# Limitations (v1)

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

**Performance.** The provisional JSON display list costs ≈ 1–2 ms of serialization per paragraph;
the binary format (M5) removes most of it. Preamble changes restart the engine (≈ 1 s).
