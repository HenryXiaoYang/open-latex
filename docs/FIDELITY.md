# Fidelity verification

Three independent layers establish that a lode display list places every glyph exactly where
LuaTeX places it. They are deliberately independent so that a bug shared by two implementations
of the same idea cannot hide.

## Layer 0 — backend oracle (engine-internal, exact)

`tex/experiments/e10-oracle.tex`: a zero-width `late_lua` whatsit is inserted before every glyph
after line breaking. At shipout LuaTeX's backend executes it and `pdf.getpos()` returns the
backend's own cursor. The traversal (`tex/lode-dl.lua`) must agree **to the scaled point** with
that cursor for every glyph. Current status: 369/369 glyphs, 0 sp, across lines with expansion
factors −2 %, −0.4 %, 0, +0.3 %, +2 %, with font kerns, inter-word glue, italics and inline math.
This layer does not depend on PDF parsing at all and does not change the typeset output.

## Layer 1 — fast path vs shipout extractor (sp-exact)

The persistent server typesets the paragraph from its captured context; `lode-capture` runs the
same traversal on the shipped page of a full compile. For the same paragraph the two display
lists must be identical after translating each page fragment by its first line's placement
(book-class margins alternate between odd and even pages, so a paragraph that crosses a page
break has one offset per page): fonts (by stable font key), glyph indices, x/baseline positions,
advances, expansion factors, line boxes and glue ratios.

Status (`lode verify`): 10-page `pure` fixture 49/49 paragraphs eligible, 21 396/21 396 glyphs
identical; 10-page `mixed` fixture 29/104 paragraphs eligible (the rest are background-only
for the reasons the allow-list reports: `\footnote`, `\ref`, `\cite`, `\label`, list items,
headings, TOC lines, display math, floats), 12 709/12 709 glyphs identical.

### Units (0.0.2)

Layer 1 runs per **unit** (`lode verify`, `compare_rows` in `crates/lode-cli/src/verify.rs`): the
fast result of every eligible unit — paragraphs with display math and footnote marks, lists,
quotes, theorem environments, figures with images and captions, tables, headings — is compared
row by row with the rows of the same unit on the shipped page(s): row box (width, height, depth,
glue set), every glyph (font identity, char, glyph index, x, baseline, advance, expansion), rule
and image counts. Each row gets its own offset from its placement, so the comparison is of each
row's content, not of the page builder's vertical arrangement. On the `units` fixture
(`fixtures/book-10-units-lmtfm`): 33 eligible units of 46, 9331/9331 glyphs identical, 0 units
with differences; the ineligible ones are the `\tableofcontents` line, the bibliography and
`\cite` under biblatex (documented in LIMITATIONS.md). `--units` restricts the run and
`--dump-rows` prints both sides as text for a differing unit.

## Layer 2 — PDF content stream (independent parser, quantified tolerance)

`crates/lode-verify/src/pdftext.rs` replays the PDF text state machine (`Tm Td TD T* TL Tc Tw Tz
Ts Tf Tj TJ ' "`), the CTM (`cm q Q`), fills (`re f`) and stroked rules (`w m l S`) using the
PDF's own `/W` and `/Widths` tables, and `compare.rs` matches every display-list glyph to a PDF
glyph by glyph index and position.

Tolerances are derived from what LuaTeX actually writes, not chosen to make tests pass:

| Quantity | Quantum | Why |
|---|---|---|
| `Tm`-anchored glyph origin | 10⁻ᵈ bp, d = decimals LuaTeX wrote (3 here) | coordinate printing precision |
| Interior glyph origin | one `TJ` unit = font size / 1000 bp (0.0109 bp at 10.95 pt) | `TJ` adjustments are integers |
| Horizontal scale (`Tm` a-component) | 10⁻³ | printed with 3 decimals; equals `1 + expansion_factor / 10⁶` |
| Rules | 10⁻² bp | stroked-line geometry printed with 3 decimals |

Status: all 10 pages of `pure` and all 16 pages of `mixed` pass with anchored glyphs max
0.0005 bp, interior glyphs max 0.0106 bp (< 1 TJ unit), zero scale error, every glyph matched,
all rules (footnote rules, fraction bars, `\rule` placeholders, TOC leaders) and images
(`\includegraphics`, matched by the `Do` operator's CTM rectangle) matched. The backend oracle
shows the same ≤ 1 TJ-unit deviation between LuaTeX's cursor and its PDF, i.e. the display list
is as precise as the engine and slightly more precise than the PDF rendering of it.

A fourth cross-check used during development: PyMuPDF's per-character origins agree with the
Rust parser (same deltas to 10⁻⁵ bp).

## Layer 3 — rendered comparison

`crates/lode-verify/src/raster.rs` rasterizes the page display list at 150 dpi with the font
files it names (OpenType/TrueType outlines via `ttf-parser`, horizontal scale from the expansion
factor, `slant`/`extend` honoured) and compares it with PyMuPDF's raster of the PDF page. The
metric is anti-aliasing tolerant: an ink pixel counts as matched if the other image has ink
within one pixel; the unmatched fraction must stay below 1 % and no glyph may be skipped.

Status: `pure` 0.007 % unmatched ink (worst page), `mixed` 0.018 %. Fixtures use `unicode-math`
so every glyph, including math, is an OpenType glyph; Type1 fonts (classic Computer Modern
math) are reported as skipped by the rasterizer and documented in LIMITATIONS.

Image scaling (graphicx wraps bitmap images in `pdf_save`/`pdf_setmatrix`/`pdf_restore`) is
represented by MATRIX items; the page comparator applies the composed transform to the image
rectangle before matching it with the PDF's `Do` placement, so figure pages are *exact*, not
degraded.

## Degraded pages

When the traversal meets something it cannot represent (non-TLT direction, `\pdfliteral`
drawing, unknown whatsits, …) the page display list carries flags and the page is *Degraded*:
`lode verify` reports it separately and the library hands hosts the PDF page as fallback (M4).
Both fixtures currently have 0 degraded pages; TOC dot leaders, images and color stacks are
represented natively.

## Robustness (crates/lode-core/tests/engine_robustness.rs)

Undefined macros and unbalanced braces produce `error` results with diagnostics and leave the
engine state fingerprint intact; a `\footnote` yields `ok_degraded` with an `ins` flag; a
runaway `\loop` is killed by the per-request watchdog and a fresh generation serves again;
results do not depend on request order.

## Running

```bash
source build/texlive.env
cargo run --release -p lode-cli -- slice --project build/fx/book-10-pure --edits 200 --json-out build/slice-report.json
cd tex/experiments && LUAINPUTS=../../tex//: max_print_line=100000 lualatex -output-directory=../../build/exp e10-oracle.tex && grep ^E10 ../../build/exp/e10-oracle.log
```
