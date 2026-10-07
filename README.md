# open-latex (lode)

An embeddable real-time LaTeX compilation library. It keeps an **unmodified LuaTeX** process
alive, recompiles only the paragraph being edited (≈ 1 ms class latency), extracts a
display list (glyph ids, font references, positions, rules, colors, microtypographic
adjustments) directly from LuaTeX's node lists — bypassing PDF generation — and reconciles
pagination, floats, footnotes, counters and cross-references in a background full compile
that reports explicit, versioned convergence status. Hosts (editors) render the display list
with their own renderer; exported PDFs match a clean LuaLaTeX build.

The architecture follows Clemens Lode, *Real-Time LuaTeX: Recompiling Large Documents in 1 ms*
(TUGboat 2026 / TUG 2026).

## Layout

| Path | Contents |
|---|---|
| `crates/lode-core` | Rust core: project model, persistent paragraph server, background compiler, versioned layout store, C ABI |
| `crates/lode-dl` | Display-list model and codecs (binary + JSON) |
| `crates/lode-verify` | Independent fidelity checks (PDF content streams, rasterized comparison) |
| `crates/lode-cli` | Headless driver: fixtures, benchmarks, verification, export |
| `tex/` | Lua + LaTeX side: serve loop, node traversal, capture package, experiments |
| `bench/` | Replica of the paper's benchmarks and results |
| `docs/` | Architecture, formats, protocol, versioning, convergence, benchmarks |

## Getting started

```bash
scripts/install-texlive.sh          # minimal TeX Live 2026 into build/texlive (≈ 15 min)
source build/texlive.env
cargo build --workspace
cargo run -p lode-cli -- gen-book --pages 10 --out build/fx/book-10-pure
```

```bash
cargo run --release -p lode-cli -- verify --project build/fx/book-10-pure --raster   # three fidelity layers
cargo run --release -p lode-cli -- slice  --project build/fx/book-10-pure            # timing of one paragraph
cargo test --workspace                                                              # unit + engine tests
texlua tex/tests/run.lua                                                            # Lua-side unit tests
```

See `docs/FIDELITY.md` for how output is verified, `docs/BENCHMARKS.md` for measured numbers,
`docs/PROTOCOL.md` for the engine protocol and `docs/ENGINE_NOTES.md` for what the engine
experiments established.
