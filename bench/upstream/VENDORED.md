# Vendored upstream benchmark

`luatex-benchmark/` is a verbatim copy of https://github.com/texlode/luatex-benchmark at commit
9b86f576ed977853d6a36c0f41b14a037297b150 (2026-09-23), MIT licensed by its
authors (see `luatex-benchmark/LICENSE`). It is the paper's own reproduction of the in-engine
line-breaking numbers (Table 1 / Table 2) and the Typst comparison; nothing in it depends on rtex.

`rtex bench` runs `systematic-benchmark.tex` from here to derive the hardware factor `h`
(our own replica in `bench/tex/` adds single-edit timings and CSV output but uses different
paragraph texts). Run them directly with:

```sh
cd bench/upstream/luatex-benchmark
lualatex paragraph-benchmark.tex
lualatex systematic-benchmark.tex
lualatex stability-benchmark.tex
```
