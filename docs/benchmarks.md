# Benchmarks

All numbers on this page were measured on **2026-10-10** with rtex 0.0.2. The machine was a
4-vCPU cloud VM (Intel Xeon @ 2.10 GHz) running Linux with TeX Live 2026. A typical laptop is
faster. Raw results are in [`bench/results/`](../bench/results/).

## Typing latency

This is the time from `apply_edit` to the `ParagraphUpdate` in the host's hands, with the
display list decoded. That is everything except drawing. Each figure is the median of 300
separate single-character edits, 2 ms apart, in a warm session. The documents are generated
books in the `book` class with Latin Modern, microtype and amsmath.

| Paragraph | 10-page book | 100-page book | 300-page book |
|---|---|---|---|
| 1 line | 0.50 ms | 0.48 ms | 0.61 ms |
| 4 lines | 1.04 ms | 1.10 ms | 1.18 ms |
| 10–11 lines | — | 2.39 ms | 1.94 ms |
| 3 lines with inline math | 0.89 ms | 0.90 ms | 1.24 ms |

The cost depends on the paragraph, not on the document. A 300-page book is as fast as a
10-page one; the cells differ by up to ±20 %. That is partly noise on a shared VM, and partly
because each book's paragraphs are different text. The 300-page "inline math" paragraph is
heavier on fractions than the 10-page one, and its TeX stage alone is 0.53 ms against 0.31 ms.

When edits come back to back (the paper's method: the mean of a burst of 100), threads stay warm
and the cost drops to 0.23–0.27 ms for one line and 0.7–1.0 ms for four lines.

### By kind of unit

Same method, on the books with every kind of unit (`--variant units`):

| Unit | 10-page book | 100-page book | Rows | TeX stage (100 p) |
|---|---|---|---|---|
| heading (`\section`) | 0.80 ms | 0.95 ms | 1 | 0.49 ms |
| paragraph with display math | 1.38 ms | 1.65 ms | 5 | 0.79 ms |
| paragraph with a footnote | 1.90 ms | 1.73 ms | 3–5 | 0.99 ms |
| list (`itemize`, 3 items) | 1.68 ms | 1.53 ms | 3 | 0.96 ms |
| figure (image and caption) | 1.74 ms | 1.68 ms | 3 | 1.07 ms |
| table (`booktabs`, caption) | 1.52 ms | 1.70 ms | 2 | 0.98 ms |

Environments spend most of their time in LaTeX's own machinery: `\list`, `\halign`,
`\@startsection`, `\includegraphics`, counters. The P95 is within 1.5× of the median for every
kind, except the footnote paragraph on the 100-page book (4.3 ms).

### Fonts matter most

The same 300-page book in three font setups:

| Font setup | 1 line | 4 lines | 11 lines | inline math |
|---|---|---|---|---|
| `[T1]{fontenc}` + `lmodern` (TFM) | 0.61 ms | 1.18 ms | 1.94 ms | 1.24 ms |
| fontspec, TeX Gyre Pagella, `Renderer=Basic` | 0.61 ms | 1.27 ms | 2.29 ms | 1.05 ms |
| fontspec, TeX Gyre Pagella, default (node mode) | 0.85 ms | 3.70 ms | 9.17 ms | 2.47 ms |

With fontspec's default node mode, luaotfload shapes the text in Lua on every compile. For the
11-line paragraph, the TeX stage is 8.0 ms against 0.9 ms with TFM fonts. `Renderer=Basic` uses
the engine's own ligature and kerning tables and is as fast as TFM. HarfBuzz mode
(`Renderer=HarfBuzz`) is slower still than node mode. If you want the fastest updates for Latin
text, use `Renderer=Basic` or TFM fonts ([live-editing.md](live-editing.md#making-it-faster)).

### Where the time goes

For a one-line paragraph on the 300-page book (0.61 ms in total):

| Stage | Time |
|---|---|
| TeX: restoring the context and typesetting the box | 0.16 ms |
| walking the box and encoding the display list | 0.07 ms |
| everything else: two process hand-offs, thread wake-ups, decoding, the session | 0.38 ms |

For four lines (1.18 ms) it is 0.55, 0.23 and 0.40 ms. The walk costs about 1 µs per glyph. The
fixed cost of 0.4 ms is mostly waking processes and threads that slept between keystrokes. That
is why bursts of edits are two to three times faster than isolated ones. On a laptop with
faster context switches this part is smaller.

This fixed cost is about 0.1 ms higher than in the previous recorded run (2026-10-08: 0.42 ms
for one line on the 10-page book, now 0.50 ms). In between, the host started reading the
engine's responses on a dedicated thread. That fixed lost results on macOS, where the old
polling missed frames ([engine-protocol.md](engine-protocol.md#channels)), and the hand-off to
that thread is the extra cost.

## Background passes

The time until the next layout arrives. A *pass* is one LuaLaTeX run with the capture package. A
layout is one or more passes, repeated until cross-references settle.

| Document | Pages | Live engine ready | First layout | Each later layout | Clean export |
|---|---|---|---|---|---|
| book, TFM fonts | 10 | 0.6 s | 1.5 s (2 passes) | 0.5 s | 1.4 s |
| book, TFM fonts | 100 | 0.7 s | 7.2 s (2 passes) | 3.1 s | 2.5 s |
| book, TFM fonts | 300 | 0.6 s | 19.4 s (2 passes) | 9.4 s | 5.1 s |
| book, OpenType (node mode) | 100 | 1.2 s | 15.2 s (2 passes) | 6.9 s | 10.9 s |
| mixed: TOC, biblatex/biber, floats, footnotes | 10 | 1.9 s | 6.3 s (3 passes) | 1.9 s | 7.8 s |
| mixed | 100 | 1.8 s | 19.0 s (3 passes) | 6.6 s | 16.2 s |

"Each later layout" is a pass after a one-character edit: the median of five, each needing one
pass. It runs in the standby engine, which already has the preamble loaded. "Clean export" is a
full PDF build without the capture package, with as many passes as the document needs (2 or 3
here).

**What makes passes slow.** For long documents, the capture package is the main cost. It writes
a display list for every page, as JSON: about 120 KB per page, 14 MB for the 100-page book.
Measured directly on the 100-page book, one LuaLaTeX run takes 1.25 s without the capture
package and 3.3 s with it. Passes only matter after you stop typing, and live updates continue
meanwhile. Still, this is the obvious next thing to optimise: write page lists in binary, and
only for pages that changed.

For short documents the standby engine helps most. Starting LuaLaTeX and loading the preamble
is most of a short run, so a later layout of the 10-page book takes 0.5 s, against 1.5 s for
the first.

**Pictures.** TikZ and pgfplots documents are dominated by drawing. The picture cache reuses
unchanged pictures from the previous pass. On a 111-page document with 120 pictures, that
shortened a pass from 45 s to 18 s.

## Compared with Typst and Overleaf

This times the same text in all three: the document the paper's Typst comparison uses. It is
Lorem ipsum paragraphs on A4 at 11 pt, about five lines each, with the middle paragraph edited
30 times. `bench/compare/compare.py` builds a LaTeX version of it and runs everything on the same
machine (2026-10-10, the VM above):

| Pages | rtex, per edit (P95) | Typst 0.15.1, `typst watch` | Typst, one page exported | Full pdfLaTeX run | Full LuaLaTeX run |
|---|---|---|---|---|---|
| 10 | 1.6 ms (2.4) | 18 ms | 9 ms | 0.12 s | 0.52 s |
| 100 | 1.3 ms (1.7) | 146 ms | 47 ms | 0.25 s | 0.91 s |
| 300 | 1.3 ms (1.7) | 414 ms | 121 ms | 0.51 s | 1.69 s |

The pages column is the LaTeX page count, rounded; the files record the exact counts. How each
tool was measured:

- **rtex:** the session's round trip for each edit, from dispatching it to the live engine to
  having the decoded result. This is the median over 29 edits, after the first.
- **Typst:** the paper's comparison script (`bench/upstream/luatex-benchmark/typst-comparison/
  bench.mjs`), unmodified. In watch mode Typst writes the whole PDF after every edit. With
  `--pages 1` it writes one page, which is close to its compile time alone. Typst's editor
  previews (the web app, tinymist) skip PDF export, so they behave more like the second column.
- **Full LaTeX run:** one pdfLaTeX or LuaLaTeX run over the whole document, best of three. This
  is the least an Overleaf recompile does, since Overleaf compiles the whole document every
  time. The hosted service adds its own queueing, sandbox, PDF download and rendering, so this
  is a lower bound, not a measurement of Overleaf.

The comparison is not quite like for like:
- rtex's figure is the paragraph alone. The pages, page breaks and references settle with the
  next background pass, seconds later (see above).
- Typst and a LaTeX run produce the whole consistent document each time, which is why their
  time grows with its length.

What the numbers do show is what you wait for after a keystroke before the text you typed
appears typeset:
- rtex: a millisecond or two, whatever the length;
- Typst: tens to hundreds of milliseconds, growing with the document;
- full recompiles: a second or more.

Beyond speed, the tools make different trade-offs:

| | rtex | Typst | Overleaf |
|---|---|---|---|
| Language | LaTeX (LuaLaTeX only), existing documents and packages | its own markup; LaTeX documents must be rewritten | LaTeX (pdfLaTeX, XeLaTeX, LuaLaTeX) |
| Output | identical to LuaLaTeX | Typst's own typesetting | real LaTeX output |
| While you type | the paragraph is exact at once; the layout catches up in seconds | everything consistent on every compile | everything consistent after each recompile |
| Collaboration | none, local | web app with real-time collaboration | real-time collaboration, comments, history, templates |
| Setup | TeX Live and Rust locally (the VS Code extension installs both) | one binary, or the browser | the browser |

To reproduce the comparison:

```sh
source build/texlive.env
cargo build --release -p rtex-cli
# Typst: a release binary on PATH (bench/upstream/luatex-benchmark/typst-comparison/fetch-typst.sh), and Node.js
python3 bench/compare/compare.py --pages 10 100 300
```

Without `typst` on PATH, the script measures only rtex and the full LaTeX runs.

## Compared with the paper

The design follows Clemens Lode, *Real-Time LuaTeX: Recompiling Large Documents in 1 ms*
(TUGboat 2026). Its benchmark ships with rtex in `bench/upstream/` and runs unmodified as part of
`rtex bench`. It times LuaTeX's line breaking alone, inside one process:

| Paragraph | Paper (i7-1355U laptop) | This VM |
|---|---|---|
| short | 0.17 ms | 0.05 ms |
| medium | 0.70 ms | 0.92 ms |
| long | 1.99 ms | 2.56 ms |
| inline math | 0.20 ms | 0.24 ms |
| display math | 0.11 ms | 0.14 ms |

So this VM is about 1.3× slower than the paper's laptop at typesetting (`rtex bench` calls this
the hardware factor *h*). The paper's full round trip, editor to engine and back excluding
drawing, was 0.79 ms for a short paragraph and 6.11 ms for a medium one. rtex measures 0.5–0.6
ms and 1.0–1.2 ms here, on slower hardware. Most of the difference is in the work around TeX.
For a medium paragraph, the paper reports 4.99 ms for line breaking plus walking the result. rtex
walks the boxes and encodes a compact binary display list in 0.23 ms.

## Running the benchmarks

```sh
source build/texlive.env
cargo build --release -p rtex-cli
for n in 10 100 300; do target/release/rtex gen-book --pages $n --fonts lm-tfm --out fixtures/book-$n-pure-lmtfm; done
for n in 10 100;     do target/release/rtex gen-book --pages $n --variant units --fonts lm-tfm --out fixtures/book-$n-units-lmtfm; done
target/release/rtex bench \
  --project fixtures/book-10-pure-lmtfm fixtures/book-100-pure-lmtfm fixtures/book-300-pure-lmtfm \
            fixtures/book-10-units-lmtfm fixtures/book-100-units-lmtfm \
  --categories short,medium,long,inline-math,display-math,footnote,list,figure,table,heading
```

This takes about five minutes. It writes a Markdown report, JSON and CSV to `bench/results/`.
Add `--quick` for a smoke run. Other font setups: `--fonts pagella-base` (`Renderer=Basic`),
`pagella` (node mode), `pagella-harf` (HarfBuzz).

`rtex bench` also checks a set of limits, all scaled by the hardware factor *h*:
- one line ≤ 1.5 ms × h (≤ 1.0 ms × h for bursts);
- four lines ≤ 6.11 ms × h (the paper's figure);
- every unit kind ≤ 5 ms × h;
- the walk and encoding of a 4-line paragraph ≤ 0.5 ms;
- 100- and 300-page books within 1.25× of the 10-page book.

In this run every limit passed except one size ratio: inline math on 300 pages vs 10 pages, 1.40.
As explained above, that compares two different paragraphs. CI runs a quick version on GitHub's
shared runners and only warns when a limit is missed.

`rtex probe --project DIR` breaks one paragraph's latency into stages and times every unit
kind directly against the engine. Use it to find where a slow document spends its time.
