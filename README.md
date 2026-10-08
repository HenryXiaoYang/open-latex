# realtime-tex (rtex)

An embeddable real-time LaTeX compilation library. It keeps an **unmodified LuaTeX** process
alive, recompiles only the paragraph being edited (≈ 1 ms class latency), extracts a
display list (glyph ids, font references, positions, rules, colors, microtypographic
adjustments) directly from LuaTeX's node lists — bypassing PDF generation — and reconciles
pagination, floats, footnotes, counters and cross-references in a background full compile
that reports explicit, versioned convergence status. Hosts (editors) render the display list
with their own renderer; exported PDFs match a clean LuaLaTeX build.

The architecture follows Clemens Lode, *Real-Time LuaTeX: Recompiling Large Documents in 1 ms*
(TUGboat 2026 / TUG 2026).

## Measured (this container, Xeon 2.1 GHz VM; `docs/BENCHMARKS.md`)

| | 10 pages | 100 pages | 300 pages | paper |
|---|---|---|---|---|
| short paragraph, per keystroke (amortized) | 0.42 ms (0.19) | 0.34 ms (0.17) | 0.46 ms (0.22) | 0.79 ms |
| medium paragraph, per keystroke | 0.80 ms | 1.04 ms | 0.99 ms | 6.11 ms |
| long paragraph (10–12 lines), per keystroke | — | 1.79 ms | 1.78 ms | — |

Per keystroke on the 100-page `units` book, individual-edit medians (every kind is also
document-size independent: 10 vs 100 pages within the ±20 % jitter of a shared VM):

| paragraph with display math (5 rows) | paragraph with a footnote (3 rows) | list (3 items) | figure (image + caption) | table (booktabs) | heading |
|---|---|---|---|---|---|
| 1.16 ms | 1.45 ms | 1.32 ms | 1.57 ms | 1.32 ms | 0.67 ms |

Paper-like font setup (TFM Latin Modern + microtype); OpenType fonts in luaotfload base mode
are equally fast, fontspec's default node mode pays Lua shaping in the TeX stage. Output equals
LuaTeX's own positions to the scaled point (`docs/FIDELITY.md`); exported PDFs are byte-equal to
an independent clean build on the fixtures.

## Status

| Area | State |
|---|---|
| Fast path | persistent LuaTeX server, allow-list eligibility over **units** (paragraphs with display math, footnotes, refs; lists, quotes, theorems, figures, tables; headings), context replay incl. counters and labels, binary display list, state fingerprint + watchdog, per-unit 5 ms budget |
| Fidelity | display lists equal the engine's own cursor to the scaled point (backend oracle), every eligible unit kind checked row-exact against the shipped page, independent PDF content-stream check incl. image transforms, rendered comparison — `docs/FIDELITY.md` |
| Background | debounced instrumented passes with biber/bibtex, versioned layouts, explicit convergence states, degraded-page PDF fallback — `docs/CONVERGENCE.md`, `docs/VERSIONING.md` |
| Export | clean build loop with honest status; byte-equal to an independent LuaLaTeX build on the fixtures |
| API | Rust `Session` + C ABI (`include/rtex.h`) + JSON-lines `rtex serve` — `docs/API.md` |
| Benchmarks | paper replica, per-stage round trips on 10/100/300-page books in three font setups, hardware-qualified gates — `docs/BENCHMARKS.md` |

## License

MIT, see `LICENSE`. The vendored upstream benchmark in `bench/upstream/` keeps its own MIT license.

## Layout

| Path | Contents |
|---|---|
| `crates/rtex-core` | Rust core: project model, persistent paragraph server, background compiler, versioned layout store, C ABI |
| `crates/rtex-dl` | Display-list model and codecs (binary + JSON) |
| `crates/rtex-verify` | Independent fidelity checks (PDF content streams, rasterized comparison) |
| `crates/rtex-cli` | Headless driver: fixtures, benchmarks, verification, export |
| `tex/` | Lua + LaTeX side: serve loop, node traversal, capture package, experiments |
| `bench/` | Replica of the paper's benchmarks and results |
| `docs/` | Architecture, formats, protocol, versioning, convergence, benchmarks |

## Getting started

```bash
scripts/install-texlive.sh          # minimal TeX Live 2026 into build/texlive (≈ 15 min)
source build/texlive.env
cargo build --workspace
cargo run -p rtex-cli -- gen-book --pages 10 --out build/fx/book-10-pure
```

```bash
cargo run --release -p rtex-cli -- verify --project build/fx/book-10-pure --raster   # three fidelity layers
cargo run --release -p rtex-cli -- slice  --project build/fx/book-10-pure            # timing of one paragraph
cargo run --release -p rtex-cli -- edit   --project build/fx/book-10-pure --find "glyph for"   # one edit through the Session
cargo run --release -p rtex-cli -- export --project build/fx/book-10-pure --out build/out.pdf --check
cargo run --release -p rtex-cli -- bench  --project fixtures/book-10-pure-lmtfm fixtures/book-100-pure-lmtfm fixtures/book-300-pure-lmtfm
cargo test --workspace                                                              # unit + engine tests
texlua tex/tests/run.lua                                                            # Lua-side unit tests
```

Fixtures: `rtex gen-book --pages N --variant pure|mixed --fonts lm-tfm|pagella-base|pagella|pagella-harf|latin-modern`.
For real-time editing, load OpenType fonts with `Renderer=Basic` (or use TFM fonts): fontspec's
default node-mode shaping costs several times the line-breaking time per paragraph (`docs/BENCHMARKS.md`).

C hosts: `cargo build --release -p rtex-core` builds `librtex_core.so`; see `include/rtex.h`,
`examples/c/edit_loop.c` and `docs/API.md`. Display lists: `docs/DISPLAY_LIST.md`.

See `docs/FIDELITY.md` for how output is verified, `docs/BENCHMARKS.md` for measured numbers,
`docs/PROTOCOL.md` for the engine protocol and `docs/ENGINE_NOTES.md` for what the engine
experiments established.

## Continuous integration

`.github/workflows/ci.yml`: a `unit` job (build, unit tests, C ABI header/symbol check, C example
compile), an `integration` job with a cached TeX Live install (Lua tests, all gated Rust tests,
the three fidelity layers on the 10-page fixture, export equality) and a `perf` job on pushes
that runs the smoke benchmark and uploads the results; performance gates on shared runners are
reported as warnings because they are hardware-qualified.
