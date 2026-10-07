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
