# How we know the output is right

rtex promises that what it shows is what LuaLaTeX would produce: the same glyphs, at the same
positions, to the scaled point (1/65536 pt). This page describes how that is checked. The checks
are deliberately independent of each other, so a bug shared by two parts of rtex cannot hide.

## The checks

### 1. The walk matches LuaTeX's own cursor

rtex reads positions by walking LuaTeX's boxes, replaying how LuaTeX's PDF writer moves its
cursor: glue with TeX's rounding, font expansion, kerns, offsets. To check the walk itself,
`tex/experiments/e10-oracle.tex` puts an invisible `late_lua` node before every glyph. When the
page is shipped out, LuaTeX's backend runs it and reports its real cursor position. The walk
must agree on every glyph. It does, with a difference of 0 sp, including lines stretched and
shrunk by microtype's font expansion, kerns, italics and inline math.

### 2. Live results equal the full run

For every unit that may go live, `rtex verify` compiles it in the live engine, from the context
the background pass recorded. It then compares the result row by row with the same unit on the
pages of the full run. That covers each row's box (width, height, depth, glue) and each glyph's
font, character, glyph index, position, advance and expansion. Rules and images are counted.
Each row is compared at its own position, so this tests the content of every row, not how the
page builder stacked them.

The live engine runs with its server-only shortcuts ([engine-protocol.md](engine-protocol.md#server-only-shortcuts))
and the reference run without them, so this also checks that the shortcuts change nothing.

### 3. Pages equal the PDF

`crates/rtex-verify` contains its own PDF content-stream interpreter. It handles text state, text
matrices, `TJ` arrays with the PDF's own width tables, the graphics state, filled and stroked
rules, and image placement. Every glyph of every page display list is matched to a glyph in the
PDF LuaTeX wrote. The tolerances come from how precisely LuaTeX writes numbers, not from what
makes the test pass:

| Quantity | Tolerance | Why |
|---|---|---|
| glyph placed by a text matrix | 10⁻³ bp | LuaTeX writes three decimals |
| glyph placed by a `TJ` adjustment | one `TJ` unit (font size / 1000 bp; 0.011 bp at 10.95 pt) | `TJ` adjustments are integers |
| horizontal scale (font expansion) | 10⁻³ | three decimals |
| rules | 10⁻² bp | |

The oracle from check 1 shows the same one-`TJ`-unit difference between LuaTeX's cursor and its
own PDF. So the display list is as precise as the engine, and slightly more precise than the PDF.

### 4. Rendered pages look the same

`rtex verify --raster` draws each page display list at 150 dpi from the font files it names,
with the expansion, slant and extend applied. It compares the result with the PDF page rendered
by MuPDF (PyMuPDF). An ink pixel counts as matched if the other image has ink within one pixel.
At most 1 % of ink may be unmatched, and no glyph may be skipped.

### 5. The capture package changes nothing

The background pass loads `rtex-capture` into your document. `rtex verify` builds the PDF with
and without it, and they must be identical: same content streams, fonts and images; metadata
is ignored.

### 6. Converged layouts and exports equal a clean build

The integration tests compare:
- a converged session layout with a fresh run of the final text (`tests/convergence.rs`);
- the standby engine's layout with a fresh run (`tests/standby.rs`);
- an exported PDF with an independent clean LuaLaTeX build (`tests/export.rs`, `rtex export --check`).

## Current results

From `rtex verify` on 2026-10-10:

| Document | Live units | Glyphs identical | Pages exact vs PDF | Rendered |
|---|---|---|---|---|
| 10-page book, OpenType (`pure`) | 54 of 54 | 23 142 / 23 142 | 10 of 10 | 0.012 % unmatched ink |
| 10-page book, every unit kind (`units`) | 36 of 45 | 10 057 / 10 057 | 13 of 13 | |
| 10-page book with TOC, biblatex, floats (`mixed`) | 30 of 39 | 10 769 / 10 769 | 11 of 11 | 0.014 % |
| corpus: homework (`cs`) | 17 of 18 | 1 585 / 1 585 | 2 of 2 | |
| corpus: analysis notes (`math`) | 10 of 10 | 865 / 865 | 1 of 1 | |
| corpus: report (`report`) | 10 of 15 | 557 / 557 | 2 of 2 | |
| corpus: lab report (`layout`) | 10 of 10 | 773 / 773 | 1 of 1 | |
| corpus: code and units (`code`) | 8 of 8 | 344 / 344 | 1 of 1 | |
| corpus: multi-file project (`multi`) | 14 of 14 | 1 126 / 1 126 | 3 of 3 | |
| corpus: counters (`counters`) | 9 of 10 | 948 / 948 | 1 of 1 | |
| corpus: TikZ document (`pictures`) | 9 of 20 | 750 / 750 | 0 of 3 (drawn from the PDF) | |

Worst glyph deviation from the PDF: 0.0005 bp for glyphs placed by a text matrix, and 0.016 bp
(under one `TJ` unit) for glyphs placed by a `TJ` adjustment.

The units that are not live are mostly generated material: the table of contents and its
entries, and the bibliography. The rest are excluded by design:
- a `\stepcounter` that comes before a paragraph's text;
- the `\newpage` line in the homework;
- in the TikZ document, the pictures, because `rtex verify` uses the allow-list unless you pass
  `--permissive`. In a session's default probe mode they can go live, and their pages are drawn
  from the PDF.

**Probe mode** is checked separately. `fixtures/research/permissive` has 29 units built from
commands outside the allow-list. With the allow-list switched off, 28 are compiled live and
compared. 26 are identical; the 2 that differ depend on page state, as expected (`\thepage` on
page 2, and `\marginpar`). The comparison catches both. CI requires exactly those two, so a
construct the comparison stops catching fails the build.

## In CI

Every push runs:
- on Linux: all of the above, on the generated books, the corpus, the picture cache and the
  research fixture;
- on macOS and Windows: all integration tests and checks 2, 3 and 5 on the `pure` book.

## Running it yourself

```sh
source build/texlive.env
rtex gen-book --pages 10 --out build/fx/book-10-pure
rtex verify --project build/fx/book-10-pure --raster          # needs python3 with PyMuPDF for --raster
rtex verify --project fixtures/book-10-units-lmtfm --dump-rows  # print both sides of any unit that differs
rtex verify --project fixtures/corpus/pictures --pic-cache --raster
rtex verify --project fixtures/research/permissive --permissive --expect-differing 2
```

To run the oracle (check 1):

```sh
cd tex/experiments
LUAINPUTS=../../tex//: max_print_line=100000 lualatex -output-directory=../../build/exp e10-oracle.tex
grep ^E10 ../../build/exp/e10-oracle.log
```
