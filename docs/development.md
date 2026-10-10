# Working on rtex

## Repository layout

| Path | What is there |
|---|---|
| `crates/rtex-core` | the library: session, live engine, background passes, layouts, C ABI (`ffi.rs`) |
| `crates/rtex-dl` | the display-list model and its binary and JSON codecs |
| `crates/rtex-verify` | independent checks: PDF content-stream parser, rasterizer, PDF comparison |
| `crates/rtex-cli` | the `rtex` command: fixtures, verification, benchmarks, export, `serve` |
| `tex/` | the TeX and Lua side: live engine loop, capture package, node-list walk, picture cache |
| `tex/experiments/` | the LuaTeX experiments the design rests on ([engine-protocol.md](engine-protocol.md#facts-about-luatex-the-design-relies-on)) |
| `tex/tests/` | Lua unit tests (`texlua tex/tests/run.lua`) |
| `include/rtex.h`, `examples/c/` | C header (kept by hand, checked in CI) and a small C host |
| `fixtures/` | generated books, a corpus of realistic documents, research fixtures |
| `bench/` | benchmark TeX files, the paper's own benchmark (vendored), recorded results |
| `scripts/` | TeX Live installer, ABI check |

## Setting up

You need Rust (stable) and a TeX Live with LuaLaTeX and the packages the tests use. The
installer puts a minimal TeX Live into `build/texlive` without touching the rest of your
system. It takes about 15 minutes, and on Windows it runs from Git Bash.

```sh
scripts/install-texlive.sh
source build/texlive.env          # puts lualatex on PATH and sets RTEX_TEXLIVE_BIN
```

An existing full TeX Live (or MacTeX) works too: `export RTEX_TEXLIVE_BIN=/Library/TeX/texbin`.

## Building and testing

```sh
cargo build --workspace --release
cargo test --workspace --release     # unit tests, plus engine tests that need lualatex
texlua tex/tests/run.lua             # Lua tests
scripts/check-abi.sh                 # include/rtex.h matches the exported symbols (Linux, macOS)
cargo fmt --all && cargo clippy --workspace --all-targets --release -- -D warnings
```

Tests that need LuaLaTeX skip themselves when it is not found, so `cargo test` always works.
Run them in release mode: debug builds are slow enough to trip timing-sensitive tests.

The integration tests in `crates/rtex-core/tests/`:

| File | Covers |
|---|---|
| `session.rs` | edits, splits and merges, stale results, probe mode, borrowed contexts, multi-file projects, counters, user environments, the title block |
| `units.rs` | every unit kind takes the live path; the time budget |
| `engine_robustness.rs` | errors, runaway loops, the watchdog, leaked definitions, large results, request order |
| `convergence.rs` | a converged layout equals a fresh run; the capture package does not change the PDF; biber and degraded pages |
| `bibliography.rs` | biblatex in a fresh session, makeindex and imakeidx, glossaries |
| `standby.rs` | the standby engine gives the same layout as a fresh run |
| `capture_groups.rs` | paragraphs that start inside a group |
| `piccache_live.rs` | cached pictures in the live engine |
| `internal_error.rs` | a Lua error inside the server is answered, not swallowed |
| `export.rs` | export equals a clean build; errors are reported |

## Fixtures and the `rtex` command

```sh
rtex gen-book --pages 100 --variant pure|mixed|units --fonts lm-tfm|pagella|pagella-base|pagella-harf|latin-modern --out DIR
rtex verify  --project DIR [--raster] [--permissive] [--pic-cache]   # see correctness.md
rtex edit    --project DIR --find "some text" --text " inserted"     # one edit through a session
rtex slice   --project DIR                                           # timing of one paragraph
rtex probe   --project DIR                                           # latency breakdown by stage and unit kind
rtex bench   --project DIR… --categories short,medium,…              # see benchmarks.md
python3 bench/compare/compare.py                                    # rtex vs Typst vs a full LaTeX run
rtex export  --project DIR --out out.pdf --check                     # export and compare with a clean build
rtex serve   --project DIR                                           # JSON-lines session
rtex pdf-compare A.pdf B.pdf
rtex dl2json FILE
```

`fixtures/corpus/` has realistic documents (homework, analysis notes, a report, a lab report, a
code document, a multi-file project, counters, a TikZ-heavy document). CI runs `rtex verify` on
each with a minimum number of live units. `fixtures/research/permissive/` holds constructs
outside the allow-list, used to check that probe mode catches every difference.

## CI

`.github/workflows/ci.yml` runs on every push:

| Job | Runs on | Does |
|---|---|---|
| `unit` | Ubuntu | format check, clippy, build, unit tests, ABI check, C example |
| `integration` | Ubuntu, cached TeX Live | Lua tests, all Rust tests, all verification layers on the fixtures, corpus, picture cache, probe mode, export equality |
| `platforms` | macOS, Windows, cached TeX Live | clippy, unit and integration tests, verification on one fixture, export equality |
| `perf` | Ubuntu, after integration | a quick benchmark; results uploaded as an artifact (hardware-dependent limits are warnings) |

## When the live engine fails

Set `debug_dir` (`SessionConfig::debug_dir`, C `"debug_dir"`, `rtex serve --debug-dir`, or the
`RTEX_DEBUG_DIR` environment variable). Every engine failure then leaves a folder
`engine-<time>-g<generation>-par<id>/` there, with:

- `report.json`: why (watchdog, state mismatch, crash, protocol error), the unit, versions;
- `source.tex`, `context.json`, `pics.json`: exactly what was sent;
- the server's driver, preamble copy and TeX log;
- `rtex-serve-g<generation>.trace`: one line per request stage and per font load, flushed
  immediately. A request that hung shows the last stage it reached, even though the killed
  process's log is cut off.

`requests.log` in the same directory gets one line per live compile: unit, status, rows, TeX
time and total time. With `RTEX_TRACE_MACROS=1` the server also runs with `\tracingmacros=1`.
That is much slower, so the watchdog is ten times longer while it is on.

A unit that hangs or crashes the engine is not tried again until the preamble changes. Without
that, every keystroke in it would kill the engine.

## Conventions

- Version: the library, C ABI, display-list format and engine protocol are versioned together
  as 0.0.x. Nothing is frozen before 0.1.
- Commits follow Conventional Commits (`feat:`, `fix:`, `docs:`, `test:`, …).
- A change to what the live engine produces needs `rtex verify` to stay exact on the fixtures
  and the corpus. CI enforces it.
