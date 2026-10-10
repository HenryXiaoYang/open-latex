# realtime-tex (rtex)

English | [简体中文](README.zh-CN.md)

**See your LaTeX document update as you type, in about a millisecond, without leaving LuaLaTeX.**

![rtex in VS Code: the paragraph being edited updates in the preview as you type](docs/images/demo.gif)

rtex is a library for editors. When you type, it re-typesets only the paragraph you are editing,
using a LuaLaTeX process that has already loaded your preamble, and hands the editor the new
lines to draw. A full compile runs in the background and takes care of what really is global:
page breaks, floats, the table of contents, references and bibliographies.

The result is exactly what LuaLaTeX produces: the same line breaks, the same glyphs, at the
same positions to 1/65536 pt. rtex uses an unmodified LuaTeX from TeX Live, and exported PDFs
are identical to a normal LuaLaTeX build.

```
                         ┌─▶ live engine ──────▶ new lines of that paragraph   (≈ 1 ms)
 keystroke ─▶ rtex ──────┤
                         └─▶ background pass ──▶ whole pages, references, TOC   (seconds, after you pause)
```

The approach follows Clemens Lode, [*Real-Time LuaTeX: Recompiling Large Documents in 1 ms*](https://www.tug.org/tug2026/preprints/lode-realtime.pdf)
(TUG 2026).

## Use it in VS Code

The quickest way to use rtex is the **[Realtime TeX Live Preview](https://github.com/HenryXiaoYang/realtime-tex-vsc-plugin)**
extension for VS Code. It shows a live preview next to your editor and installs rtex (and, if
needed, a minimal TeX Live) for you on first use. Open a `.tex` file, click the preview icon in
the editor title bar (or press Ctrl+Alt+V, Cmd+Alt+V on a Mac), and start typing. On Windows, the
extension currently runs rtex inside WSL.

The rest of this page is about rtex itself: the library the extension is built on.

## How fast

Time from keystroke to the updated paragraph in the editor's hands (median of 300 edits, on a
4-vCPU cloud VM; a laptop is usually faster). [Full benchmarks](docs/benchmarks.md).

| Paragraph | 10-page document | 100 pages | 300 pages |
|---|---|---|---|
| one line | 0.50 ms | 0.48 ms | 0.61 ms |
| four lines | 1.04 ms | 1.10 ms | 1.18 ms |
| ten lines | — | 2.39 ms | 1.94 ms |
| a list, a figure, a table, display math | 1.4–1.9 ms | 1.5–1.7 ms | |

Document length does not matter, only the paragraph does. The font setup matters too. With
fontspec's default OpenType shaping, a long paragraph takes up to 4–5× longer than with TFM fonts
or `Renderer=Basic` ([why](docs/live-editing.md#making-it-faster)).

Typed into the same document, a paragraph updates in 1.3–1.6 ms with rtex, 18 ms (10 pages) to 414 ms (300 pages) with Typst 0.15.1, and a full LuaLaTeX run takes 0.5–1.7 s. That full run is what Overleaf repeats on every recompile ([comparison](docs/benchmarks.md#compared-with-typst-and-overleaf)).

## What updates live

Text, math (inline and display, `align` and friends), references and citations, lists,
theorems, figures and tables, headings, footnote marks, your own macros and environments, and
most packages. When rtex cannot prove that a live result would be exact, it says why, and the
paragraph appears with the next background pass instead. That covers preamble edits, the table
of contents, margin notes, and text whose output depends on page state.
[Full list](docs/live-editing.md).

## Try it

You need [Rust](https://rustup.rs) and a TeX Live with LuaLaTeX. The script installs a minimal
TeX Live into `build/texlive` (about 15 minutes; on Windows, run it from Git Bash). An existing
TeX Live or MacTeX works too: set `RTEX_TEXLIVE_BIN` to its `bin` directory.

```sh
git clone https://github.com/HenryXiaoYang/realtime-tex && cd realtime-tex
scripts/install-texlive.sh && source build/texlive.env
cargo build --release

# make a 10-page test book, apply one live edit, and print what came back
target/release/rtex gen-book --pages 10 --out build/fx/book-10
target/release/rtex edit --project build/fx/book-10 --find "Baseline export" --text " (edited)"

# check that live output matches the real PDF, then export
target/release/rtex verify --project build/fx/book-10
target/release/rtex export --project build/fx/book-10 --out build/book-10.pdf --check
```

To edit your own document live, use the
[VS Code extension](https://github.com/HenryXiaoYang/realtime-tex-vsc-plugin), or drive a session
yourself with `rtex serve --project path/to/your/project` (JSON lines on stdin/stdout).

## Build it into your own editor

rtex can be used as a Rust crate, as a C library (`include/rtex.h`), or as a subprocess speaking
JSON lines. The editor sends edits and gets events: *this paragraph now looks like this* and
*here are the new pages*. Each comes with display lists: glyphs from font files at exact
positions, ready to draw. Pages that contain something a display list cannot describe, such as
TikZ drawings, come with a PDF to draw them from. See the [embedding guide](docs/embedding.md);
the [VS Code extension](https://github.com/HenryXiaoYang/realtime-tex-vsc-plugin) is a complete
example of a host.

## Platforms

Linux, macOS and Windows. CI runs the test suite on all three against a real TeX Live. Only
LuaLaTeX is supported (not pdfLaTeX or XeLaTeX), with a LaTeX kernel from 2021 or later.

## Documentation

| | |
|---|---|
| [Live editing](docs/live-editing.md) | what updates as you type, what waits, how to make it faster |
| [How it works](docs/how-it-works.md) | the live engine, background passes, how rtex decides what can go live |
| [Embedding](docs/embedding.md) | Rust, C and JSON-lines APIs, events, configuration, drawing |
| [Display lists](docs/display-list.md) | the drawing format, binary and JSON |
| [Correctness](docs/correctness.md) | how output is checked against LuaTeX and its PDF, current results |
| [Benchmarks](docs/benchmarks.md) | latency, background passes, fonts, comparison with the paper |
| [Engine protocol](docs/engine-protocol.md) | how rtex talks to the live LuaTeX process (for contributors) |
| [Development](docs/development.md) | building, testing, CI, debugging engine failures |
| [Changelog](CHANGELOG.md) | |

## Status

Version 0.0.2. It works and is tested, but the API, the C ABI and the display-list format may
still change before 0.1.

## License

MIT, see [LICENSE](LICENSE). The paper's benchmark, vendored in `bench/upstream/`, keeps its own
MIT license.

## Thanks

- Clemens Lode ([@ClemensLode](https://github.com/ClemensLode)) for the paper this project is built on,
  [*Real-Time LuaTeX: Recompiling Large Documents in 1 ms*](https://www.tug.org/tug2026/preprints/lode-realtime.pdf)
  (TUG 2026).
- [@kenny-21342](https://github.com/kenny-21342).
- The [LuaTeX / LuaLaTeX](https://www.luatex.org/) developers: rtex runs their engine unmodified.
- [Typst](https://github.com/typst/typst), for showing how fast typesetting can feel.
- The [LINUX DO](https://linux.do/) community.
