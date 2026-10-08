# Fidelity verification

Three independent layers establish that a rtex display list places every glyph exactly where
LuaTeX places it. They are deliberately independent so that a bug shared by two implementations
of the same idea cannot hide.

## Layer 0 — backend oracle (engine-internal, exact)

`tex/experiments/e10-oracle.tex`: a zero-width `late_lua` whatsit is inserted before every glyph
after line breaking. At shipout LuaTeX's backend executes it and `pdf.getpos()` returns the
backend's own cursor. The traversal (`tex/rtex-dl.lua`) must agree **to the scaled point** with
that cursor for every glyph. Current status: 369/369 glyphs, 0 sp, across lines with expansion
factors −2 %, −0.4 %, 0, +0.3 %, +2 %, with font kerns, inter-word glue, italics and inline math.
This layer does not depend on PDF parsing at all and does not change the typeset output.

## Layer 1 — fast path vs shipout extractor (sp-exact)

The persistent server typesets the paragraph from its captured context; `rtex-capture` runs the
same traversal on the shipped page of a full compile. For the same paragraph the two display
lists must be identical after translating each page fragment by its first line's placement
(book-class margins alternate between odd and even pages, so a paragraph that crosses a page
break has one offset per page): fonts (by stable font key), glyph indices, x/baseline positions,
advances, expansion factors, line boxes and glue ratios.

Status (`rtex verify`, 0.0.2): 10-page `pure` fixture 54/54 units eligible, 23 060/23 060 glyphs
identical; 10-page `mixed` fixture 28/39 units eligible (the rest are the TOC line and biblatex
`\cite`), 10 194/10 194 glyphs identical; `units` fixture below. Both reference builds (the
instrumented capture and the clean build) run to aux convergence like the background compiler,
so `\ref`/`\pageref`/`\eqref` carry their final values on both sides of the comparison.

### Units (0.0.2)

Layer 1 runs per **unit** (`rtex verify`, `compare_rows` in `crates/rtex-cli/src/verify.rs`): the
fast result of every eligible unit — paragraphs with display math and footnote marks, lists,
quotes, theorem environments, figures with images and captions, tables, headings — is compared
row by row with the rows of the same unit on the shipped page(s): row box (width, height, depth,
glue set), every glyph (font identity, char, glyph index, x, baseline, advance, expansion), rule
and image counts. Each row gets its own offset from its placement, so the comparison is of each
row's content, not of the page builder's vertical arrangement. On the `units` fixture
(`fixtures/book-10-units-lmtfm`): 36 eligible units of 46, 10 057/10 057 glyphs identical, 0 units
with differences; the ineligible ones are the `\tableofcontents` line and the chapter units it
shares a span with (documented in LIMITATIONS.md). `--units` restricts the run and
`--dump-rows` prints both sides as text for a differing unit. The fast side runs with the
server-only shortcuts of `tex/latex/rtex-serve-patches.tex` (ARCHITECTURE.md), the reference
side without them, so the comparison also covers those.

Two capture facts fixed in 0.0.2 after real documents hit them: the unit attribute is now set
globally and the line boxes are tagged explicitly at `post_linebreak`, because a paragraph that
starts inside a group (`{\em word} …`, `{\bfseries …}`, a bare `tabularx`, whose final table is
typeset inside its own group) had its lines built after TeX restored the attribute, leaving the
unit without rows; and the font and color of such a paragraph are taken from outside the group
(the state between units), not from `para/begin` inside it (`tests/capture_groups.rs`).
`tests/standby.rs` checks that the standby background engine produces the same units, rows,
placements, labels and page glyphs as a fresh run.

### Corpus (0.0.2)

`fixtures/corpus/` holds seven realistic documents (a CS homework with enumitem lists, listings,
hyperref, tabularx and user-defined `solution`/`hint` environments; analysis notes with amsthm, mathtools and user macros; a report with
natbib, subfigures, multirow and colortbl tables; a lab report with minipages, boxes and ulem;
a code/units document with verbatim, listings, siunitx and cancel; a project split over files
with a preamble `\input`, an `\input` chapter and an `\include` chapter; counters set by
hand, `\texorpdfstring` and `\captionof`). CI runs `rtex verify` on each with an eligibility
gate (`--min-eligible`): cs 17/18 units (the lone `\newpage` line draws nothing), math 9/9,
layout 9/9, code 8/8, multi 14/14, counters 9/10 (a `\stepcounter` before a paragraph's text
is background-only by design), report 9/17 (the rest is the title block, TOC entries and
bibliography, generated material), every eligible unit glyph-identical.

A paragraph that ends an `\input`ted file keeps one interword space before `\parfillskip`
(TeX reads the parent's end of line after the file, and `\par` drops only the last glue); the
fast path reproduces it (`document::fast_source`), so the last row's glue set is exact too.

## Layer 2 — PDF content stream (independent parser, quantified tolerance)

`crates/rtex-verify/src/pdftext.rs` replays the PDF text state machine (`Tm Td TD T* TL Tc Tw Tz
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

`crates/rtex-verify/src/raster.rs` rasterizes the page display list at 150 dpi with the font
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
`rtex verify` reports it separately and the library hands hosts the PDF page as fallback (M4).
Both fixtures currently have 0 degraded pages; TOC dot leaders, images and color stacks are
represented natively.

## Robustness (crates/rtex-core/tests/engine_robustness.rs)

Undefined macros and unbalanced braces produce `error` results with diagnostics and leave the
engine state fingerprint intact; a `\footnote` yields `ok_degraded` with an `ins` flag; a
runaway `\loop` is killed by the per-request watchdog and a fresh generation serves again;
results do not depend on request order.

## Running

```bash
source build/texlive.env
cargo run --release -p rtex-cli -- verify --project fixtures/book-10-units-lmtfm --dump-rows   # layers 1–2
cargo run --release -p rtex-cli -- verify --project fixtures/book-10-pure --raster             # + layer 3
cargo run --release -p rtex-cli -- slice --project build/fx/book-10-pure --edits 200 --json-out build/slice-report.json
cd tex/experiments && LUAINPUTS=../../tex//: max_print_line=100000 lualatex -output-directory=../../build/exp e10-oracle.tex && grep ^E10 ../../build/exp/e10-oracle.log
```
