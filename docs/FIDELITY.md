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
lists must be identical after translating by the first line's placement: fonts (by stable font
key), glyph indices, x/baseline positions, advances, expansion factors, line boxes and glue
ratios. Current status (10-page `pure` fixture, paragraph of 3 lines, 166 glyphs): 166/166.

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

Measured on the fixture page: anchored glyphs max 0.0005 bp, interior glyphs max 0.0105 bp
(< 1 TJ unit), scale error 0.000000, 2268/2268 glyphs matched, rule matched. The backend oracle
shows the same ≤ 1 TJ-unit deviation between LuaTeX's cursor and its PDF, i.e. the display list
is as precise as the engine and slightly more precise than the PDF rendering of it.

A fourth cross-check used during development: PyMuPDF's per-character origins agree with the
Rust parser (same deltas to 10⁻⁵ bp).

## Layer 3 — rendered comparison (M2)

PDF pages rasterized with PyMuPDF versus the display list rasterized with the fonts named in its
font table; anti-aliasing-tolerant pixel comparison. Added in milestone M2.

## Running

```bash
source build/texlive.env
cargo run --release -p lode-cli -- slice --project build/fx/book-10-pure --edits 200 --json-out build/slice-report.json
cd tex/experiments && LUAINPUTS=../../tex//: max_print_line=100000 lualatex -output-directory=../../build/exp e10-oracle.tex && grep ^E10 ../../build/exp/e10-oracle.log
```
