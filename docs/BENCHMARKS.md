# Benchmarks

All numbers below were measured inside the development container (4 vCPU, Linux 6.18,
TeX Live 2026, LuaHBTeX 1.24.0). They are **not** comparable in absolute terms to the paper's
Intel Core i7-1355U figures; the hardware factor `h` below is used to qualify every gate.

## M0 — paper replica (in-engine line breaking, Table 1 methodology)

`bench/tex/systematic-benchmark.tex`: `book` class, `microtype`, `amsmath`, Latin Modern (TFM);
each sample is the mean of 100 consecutive `\setbox0\vbox{...}` compiles, 5 warm-up samples are
discarded, 30 samples kept; percentile index `floor(n*frac)+1`. "Single edit" rows time one
compile at a time (300 compiles) — what a typist experiences, without amortization.

| Category | Paper median | Here median (amortized) | Here P5 | Here P95 | Single-edit median | Single-edit P95 |
|---|---|---|---|---|---|---|
| Short (1 line) | 0.17 ms | **0.118 ms** | 0.114 | 0.125 | 0.113 ms | 0.152 ms |
| Medium (4–5 lines) | 0.70 ms | **0.949 ms** | 0.917 | 1.040 | 0.932 ms | 0.993 ms |
| Long (10+ lines) | 1.99 ms | **2.797 ms** | 2.727 | 2.959 | 2.853 ms | 3.163 ms |
| Inline math | 0.20 ms | **0.298 ms** | 0.281 | 0.327 | 0.290 ms | 0.321 ms |
| Display math | 0.11 ms | **0.060 ms** | 0.059 | 0.061 | 0.052 ms | 0.070 ms |

Hardware factor (plan §12): `h_short = 0.118 / 0.17 = 0.69`, `h_medium = 0.949 / 0.70 = 1.36`.
Gates use `h = max(h_short, h_medium) = 1.36` conservatively, i.e. the short-paragraph round-trip
gate on this machine is `1.0 ms × 1.36 = 1.36 ms` and the medium reference `6.11 ms × 1.36 = 8.3 ms`.
(The paragraph texts are not the paper's exact texts, which explains part of the medium/long gap.)

Single-paragraph sanity check (`paragraph-benchmark.tex`, cold, first compile): 0.35 ms.

## M0 — stability (Table 2 methodology, 500 consecutive compiles)

| Paragraph | Compiles 1–50 | 226–275 | 451–500 | `font.nextid()` before → after |
|---|---|---|---|---|
| Short | 0.123 ms | 0.119 ms | 0.120 ms | 16 → 16 |
| Multi-line | 0.927 ms | 0.898 ms | 0.938 ms | 16 → 16 |

No degradation, no font accumulation, node memory stays at the size of one paragraph.

Raw CSV: `bench/results/m0-container-*.csv` (ignored by git except this note).

## How to run

```bash
source build/texlive.env
cd bench/tex
export max_print_line=100000
lualatex -interaction=nonstopmode -output-directory=../../build/bench paragraph-benchmark.tex
LODE_BENCH_CSV=../results/run.csv lualatex -interaction=nonstopmode -output-directory=../../build/bench systematic-benchmark.tex
lualatex -interaction=nonstopmode -output-directory=../../build/bench stability-benchmark.tex
```
`LODE_BENCH_INNER`, `LODE_BENCH_WARMUP`, `LODE_BENCH_SAMPLES` override the sample plan.

## M1 — vertical slice (warm persistent server, 10-page `pure` fixture)

`lode slice --project build/fx/book-10-pure --edits 300`. Paragraph: 3 typeset lines, 166 glyphs,
TeX Gyre Pagella 10.95 pt, microtype on, inline math. Host side in Rust (release build), JSON
display list (provisional format), each edit toggles a trailing word, 2 ms pause between edits.

| Stage | median | P95 |
|---|---|---|
| TeX (context replay + macro expansion + line breaking + box) | 1.47 ms | 1.98 ms |
| Traversal (`lode-dl.lua`) | 0.25 ms | 0.45 ms |
| Serialization (JSON, provisional) | 1.21 ms | 1.77 ms |
| IPC + host-side parse | ≈ 0.54 ms | — |
| **Round trip, individual edits** | **3.46 ms** | 4.70 ms |
| Round trip, amortized (20 back-to-back) | 3.53 ms | 3.73 ms |

Other numbers: server start (preamble with fontspec + microtype) 0.9–1.0 s; first compile 44–48 ms
(font instances for italics/bold/math are loaded once); ping round trip 0.2–0.4 ms; instrumented
full compile of the 10-page book 1.6 s, clean compile 1.45 s.

Where the time goes relative to the paper's medium round trip (6.11 ms): line breaking proper for a
3-line paragraph is ≈ 0.6 ms on this machine, so the TeX stage carries ≈ 1 ms of replay overhead
(`\selectfont`, parameter assignment, `tex.runtoks`), and JSON serialization plus parsing is a
further ≈ 1.9 ms. The binary format (M5) and context-replay caching target bringing the round trip
under 1.5 ms for this paragraph; the short-paragraph gate stays `1.0 ms × h`.

## M5 — binary display list

Same slice as M1 after switching the engine protocol to the binary display list (fonts table,
glyph runs as typed records): the paragraph's display list is 5 055 bytes instead of ≈ 27 kB of
JSON.

| Stage | median | P95 |
|---|---|---|
| TeX (context replay + macro expansion + line breaking + box) | 1.44 ms | 1.99 ms |
| Traversal | 0.28 ms | 0.45 ms |
| Serialization (binary) | 0.13 ms | 0.20 ms |
| IPC + host-side decode | ≈ 0.33 ms | — |
| **Round trip, individual edits** | **2.17 ms** | 2.97 ms |
| Round trip, amortized (20 back-to-back) | 1.87 ms | 3.33 ms |

Engineering overhead outside line breaking is now dominated by the TeX replay (`\selectfont`,
parameter assignment, `tex.runtoks`), addressed in M6.

## M6 — where the time goes: the font stack, not the library

Profiling the server (`profile` request, `tex/lode-serve.lua`) shows the replay machinery costs
microseconds per request: empty `tex.runtoks` 1 µs, parameter replay 6 µs, group + `\vbox` 12 µs,
state fingerprint 6 µs. The TeX stage is the paragraph's own typesetting. How long that takes
depends on how the fonts are set up (`bench/tex/systematic-benchmark.tex` with different
preambles, medians of amortized samples, this machine):

| Preamble | Short | Medium | Long | Inline math |
|---|---|---|---|---|
| `\usepackage{microtype}` (CM/OT1, the paper's file) | 0.118 ms | 0.949 ms | 2.797 ms | 0.298 ms |
| `[T1]{fontenc}` + `lmodern` + microtype (TFM/Type1) | 0.048 ms | 0.281 ms | 0.891 ms | 0.125 ms |
| fontspec TeX Gyre Pagella, **`Renderer=Basic`** (luaotfload base mode) + microtype | 0.050 ms | 0.292 ms | 0.915 ms | 0.128 ms |
| fontspec Latin Modern (default node mode) + microtype | 0.139 ms | 1.079 ms | 3.302 ms | 0.330 ms |
| fontspec TeX Gyre Pagella (default node mode) + microtype | 0.207 ms | 1.703 ms | 5.509 ms | 0.527 ms |
| fontspec TeX Gyre Pagella, `Renderer=HarfBuzz` + microtype | 0.206 ms | 2.219 ms | 9.695 ms | 0.644 ms |

luaotfload's node mode runs OpenType shaping in Lua for every paragraph (6× the engine-native
cost); HarfBuzz mode is slower still through the Lua glue. Base mode uses the engine's own
ligature/kern tables and is as fast as TFM, at the price of advanced OpenType features
(contextual alternates, complex scripts). For real-time editing of Latin text, `Renderer=Basic`
(or TFM fonts) is the setting that keeps the 1 ms budget; the library reports the per-stage
times so hosts can show users where a slow paragraph spends its time.

The fixtures therefore come in three font setups: `*-pure-lmtfm` (paper-like TFM), `*-pure-base`
(OpenType base mode) and `*-pure` (OpenType node mode, the fontspec default).

## M6 — gates and their rationale

The `lode bench` gates (plan §12), hardware-qualified by `h = max(h_short, h_medium)` from the
in-engine replica run on the same machine in the same session:

| Gate | Limit | Why |
|---|---|---|
| short amortized median | ≤ 1.0 ms × h | the "1 ms" goal (paper method: mean of back-to-back compiles) |
| short individual median | ≤ 1.5 ms × h | what one keystroke costs with cold threads, not amortized |
| short P95 | ≤ 3 × median | no long tail |
| medium individual median | ≤ 6.11 ms × h | the paper's medium round trip |
| medium traversal + serialization | ≤ 0.5 ms | our own budget for the stages between line breaking and the wire |
| IPC + host overhead (short, medium) | ≤ 0.5 ms × h | everything outside TeX + traversal + packing: two process hops (stdin, FIFO) and two thread hops (edit → engine thread → event) |
| size independence (each category) | 100p/10p and 300p/10p ∈ [0.8, 1.25] | paragraph-local work must not depend on document length |

The paper's "engineering overhead under 20 %" is reported (`overhead share` column) but not
gated: our engine stage is several times faster than the paper's prototype (traversal fused with
binary serialization, ≈ 1.3 µs per glyph), so the same fixed wake-up cost of the pipes and
threads is a larger fraction of a smaller total. On this VM the fixed cost is ≈ 0.5 ms; on a
laptop with ≈ 20 µs context switches it is a few hundred µs at most.

Two implementation notes behind the numbers: (1) `tex.runtoks` from a `\directlua` that never
returns leaks one input level per call (E14), which killed the server after 10 000 requests; the
server now serves each request in two halves around a TeX-level `\loop`, with a flat input stack
over thousands of compiles. (2) Host-side per-edit work is independent of document size:
windowed re-segmentation, incremental line table, in-place splice (median 37 µs on a 3700-span
buffer); before that fix the 300-page round trip was 7 ms with a 0.5 ms TeX stage.

## M6 — results (final run, `bench/results/roundtrip-20261007-1547.md`)

Hardware factor this run: h = 1.35 (in-engine short 0.124 ms, medium 0.947 ms vs the paper's
0.17 / 0.70 ms). Warm session, 300 individual edits per cell (2 ms apart) and 30 × 100
back-to-back for the amortized figure; host measured from `apply_edit` to the `ParagraphUpdate`
in hand (binary display list decoded).

**Paper-like setup (`lm-tfm`: T1 Latin Modern, microtype), individual-keystroke medians / amortized:**

| Pages | Short (1 line) | Medium (4 lines) | Long (10–11 lines) | Inline math (3 lines) |
|---|---|---|---|---|
| 10 | 0.694 / 0.398 ms | 1.409 / 1.183 ms | — | 1.106 / 0.960 ms |
| 100 | 0.740 / 0.366 ms | 1.524 / 1.307 ms | 2.472 / 2.263 ms | 1.260 / 0.976 ms |
| 300 | 0.762 / 0.527 ms | 1.518 / 1.327 ms | 2.445 / 2.377 ms | 1.352 / 1.205 ms |
| paper round trip | 0.79 ms | 6.11 ms | — | — |

Stage medians at 300 pages (short / medium): TeX 0.17 / 0.58 ms, traversal + binary
serialization 0.08 / 0.35 ms, IPC + host 0.51 / 0.58 ms.

**OpenType base mode (`pagella-base`)** is within noise of the TFM numbers (short 0.73, medium
1.46, long 2.70 ms at 300 pages). **OpenType node mode (`pagella`, fontspec default)** keeps the
same size independence but pays luaotfload's Lua shaping in the TeX stage: short 1.04, medium
4.13, long 9.58 ms at 300 pages (TeX stage 0.42 / 3.21 / 8.11 ms).

All 13 gates pass: size-independence ratios 1.07–1.22, short amortized 0.527 ≤ 1.35 ms,
short individual 0.762 ≤ 2.03 ms, P95/median 1.24, medium 1.518 ≤ 8.26 ms, medium traversal +
serialization 0.354 ≤ 0.5 ms, IPC + host overhead 0.51 / 0.58 ≤ 0.68 ms. Overhead share
(everything outside the TeX stage) is 76 % for short and 62 % for medium — reported, see above.

## Upstream benchmark (texlode/luatex-benchmark, vendored verbatim in `bench/upstream/`)

The paper's own scripts, run unmodified on this machine (TeX Live 2026). `lode bench` uses this
`systematic-benchmark.tex` to derive the hardware factor; our replica in `bench/tex/` remains for
single-edit timings and CSV output but uses different paragraph texts.

| Category (upstream text) | Paper | Here |
|---|---|---|
| Short ("Hello world.") | 0.17 ms | 0.055 ms |
| Medium | 0.70 ms | 0.956 ms |
| Long (10 sentences) | 1.99 ms | 2.664 ms |
| Inline math | 0.20 ms | 0.246 ms |
| Display math | 0.11 ms | 0.144 ms |

`paragraph-benchmark.tex` (single cold compile): 0.45 ms. `stability-benchmark.tex`: short
0.119 / 0.115 / 0.113 ms and multi-line 2.545 / 2.543 / 2.545 ms over compiles 1–50 / 226–275 /
451–500 — no degradation, matching the paper's Table 2 pattern.

## 0.0.2 — latency work (`lode probe`)

`lode probe --project fixtures/book-10-pure-lmtfm` prints the breakdown that drove the 0.0.2
changes: direct server round trips (no session threads), in-engine micro-timings (`profile` op)
and session round trips with host stages. Before / after, short paragraph (1 line), this
container:

| Stage | 0.0.1 | 0.0.2 | Change |
|---|---|---|---|
| raw IPC (ping) | 0.090 ms | 0.015 ms | raw compile frame instead of JSON; `string.format` header |
| server overhead around the box build (decode, prepare, fingerprint, header) | ≈ 90 µs | ≈ 35 µs | `\luafunction` slots, one fingerprint, preprocessed contexts |
| host wait after sending | blocked `poll` | busy-poll ≤ 3 ms | ≈ 60 µs saved per round trip in this VM |
| direct round trip | 0.446 ms | 0.33 ms | |
| session: thread hops + event delivery | 0.111 ms | 0.02–0.05 ms | `apply_edit` writes the frame to the idle server itself |
| session: `apply_edit` (medium paragraph near the preamble) | 0.140 ms | 0.045 ms | windowed re-segmentation next to the preamble |
| **session round trip, short** | **0.634 ms** | **0.40–0.47 ms** | |
| session round trip, medium (4 lines) | 1.357 ms | 0.98–1.03 ms | |
| session round trip, long (10 lines) | 2.615 ms | 2.2 ms | |

Traversal (≈ 1 µs per glyph) is the Lua interpreter floor for the per-node work: a bare walk
performing the same accessor calls costs a third of it, and the remaining instructions are the
coalescing, index lookup and packing a renderer needs. Caching font descriptors removed repeated
`font.getfont` table builds for TFM fonts. What remains outside TeX for a short paragraph
(≈ 0.14 ms) is two process wake-ups and the cold-cache penalty of a process that slept between
keystrokes; the `profile` op shows the same box build running 2–3× faster in a hot loop than in
the real flow.

## 0.0.2 — units (`lode bench`, `bench/results/roundtrip-20261007-1834.md`)

Categories added to `lode bench`: `display-math` (paragraph containing a display, 3–8 rows),
`footnote` (paragraph with a footnote mark, 3–8 rows), `list`, `figure`, `table`, `heading`,
picked by unit kind from the `units` fixtures (`gen-book --variant units --fonts lm-tfm`).
Hardware factor this run h = 1.30; 300 individual edits per cell, 2 ms apart; the session runs
with a wide budget so the gates, not the budget, judge the 5 ms limit.

**Paper-like setup (`lm-tfm`), individual-keystroke medians / amortized, text categories:**

| Pages | Short (1 line) | Medium (4 lines) | Long (10–12 lines) | Inline math (2–3 lines) |
|---|---|---|---|---|
| 10 | 0.505 / 0.213 ms | 1.018 / 0.739 ms | — | 0.908 / 0.571 ms |
| 100 | 0.448 / 0.197 ms | 1.205 / 0.892 ms | 2.258 / 1.940 ms | 0.884 / 0.541 ms |
| 300 | 0.521 / 0.238 ms | 1.162 / 0.888 ms | 2.117 / 1.751 ms | 0.997 / 0.708 ms |
| 0.0.1 (300) | 0.762 / 0.527 ms | 1.518 / 1.327 ms | 2.445 / 2.377 ms | 1.352 / 1.205 ms |
| paper round trip | 0.79 ms | 6.11 ms | — | — |

Stage medians at 300 pages (short / medium): TeX 0.16 / 0.55 ms, traversal + serialization
0.07 / 0.30 ms, IPC + host 0.28 / 0.31 ms (0.0.1: 0.51 / 0.58 ms).

**Unit kinds, individual-keystroke medians (P95) / amortized:**

| Unit | 10 pages | 100 pages | rows / glyphs (100 p) | TeX stage (100 p) |
|---|---|---|---|---|
| paragraph with display math | 1.315 (1.65) / 1.058 ms | 1.330 (1.69) / 1.081 ms | 5 / 195 | 0.71 ms |
| paragraph with a footnote | 2.483 (3.21) / 2.298 ms | 2.376 (4.23) / 2.057 ms | 3 / 187 | 1.69 ms |
| list (3 items) | 1.513 (1.95) / 1.247 ms | 1.597 (3.37) / 1.230 ms | 3 / 97 | 1.04 ms |
| figure (image + 2 caption rows) | 1.952 (3.22) / 1.702 ms | 1.914 (2.36) / 1.720 ms | 3 / 105 | 1.39 ms |
| table (booktabs, caption) | 1.347 (1.58) / 1.130 ms | 1.337 (1.54) / 1.109 ms | 2 / 74 | 0.86 ms |
| heading (`\section`) | 0.935 (1.14) / 0.662 ms | 0.954 (1.17) / 0.640 ms | 1 / 15 | 0.62 ms |

The TeX stage dominates every environment kind: a footnote paragraph typesets the footnote text
as well (an insert, not drawn until the next layout); a figure re-reads the PNG (`\includegraphics`
through graphicx, ≈ 0.7 ms of its TeX stage); lists, tables and headings pay LaTeX's environment
and sectioning machinery (`\list`, `\halign`, `\@startsection`, counters, `\addcontentsline`).
Traversal stays at ≈ 1 µs per glyph and IPC + host at ≈ 0.3 ms.

**Gates (26):** all unit-kind gates pass — every kind ≤ 5 ms × h (worst: footnote 2.38 ms ≤
6.52 ms) and document-size independent (100p/10p ratios 0.96–1.06); the 0.0.1 text gates pass
with margin (short amortized 0.23 ≤ 1.30 ms, short individual 0.49 ≤ 1.96 ms, IPC + host 0.28 /
0.33 ≤ 0.65 ms, medium 1.21 ≤ 7.97 ms, traversal + serialization 0.32 ≤ 0.5 ms). One gate
failed in this run: inline-math size independence 300p/10p = 1.33 (limit 1.25); it compares two
different paragraphs (139 glyphs with fractions vs 170 glyphs) whose ratio was 1.10 and 1.22 in
the two preceding runs of the day, so this is run-to-run noise on a 1 ms figure in a shared VM,
not a size dependence (100p/10p is 1.08 and the medium/long ratios are 0.96–1.17).
