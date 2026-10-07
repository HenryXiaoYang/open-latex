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
