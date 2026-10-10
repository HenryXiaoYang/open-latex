# Live engine protocol

This is a reference for people working on rtex itself. Hosts never speak this protocol; they use
the session API ([embedding.md](embedding.md)), and the session talks to the engine.

The live engine is one `lualatex` process. It has loaded the project's preamble and runs a loop
inside `\begin{document}`. Requests go in on one channel and framed responses come back on
another. stdout cannot be used, because LuaTeX prints its banner there even in batch mode.

## Starting the engine

The host writes a driver file, `<work>/rtex-serve-g<generation>.tex`:

1. `\input` of `rtex-preamble.tex`: the project's preamble, i.e. everything before
   `\begin{document}`, with preamble `\input` files inlined and body setup statements appended.
2. A few allocations (`\rtexbox`, a loop counter, a catcode table with `@` as a letter), then
   `\begin{document}`. Every `AtBeginDocument` hook (fontspec, microtype, hyperref) has run by
   now.
3. `\input{rtex-serve-patches}`: the server-only shortcuts (see below).
4. `\directlua` loading `rtex-serve.lua` and calling `rtex_serve.init(...)`, which opens the
   channel and sends the `ready` frame.
5. The serve loop: `\loop\rtexstep\ifnum\rtexcontinue>0 \repeat`, then `\end{document}`.

It runs it as:

```
lualatex -interaction=batchmode --output-directory=<work> <work>/rtex-serve-g<generation>.tex
```

with the project directory as the working directory and this environment:

| Variable | Purpose |
|---|---|
| `RTEX_RESP` | where the server writes responses: a FIFO on Unix, a named pipe `\\.\pipe\rtex-<pid>-<n>-resp` on Windows |
| `RTEX_REQ` | Windows only: the request pipe `\\.\pipe\rtex-<pid>-<n>-req` |
| `TEXINPUTS`, `LUAINPUTS` | prepend rtex's `tex/latex//` and `tex//` |
| `openout_any=a` | allow writing to the response channel by absolute path |
| `max_print_line=100000` | keep log lines unwrapped for the diagnostics parser |
| `RTEX_TRACE` | optional: a file that gets one flushed line per request stage |

Before `\begin{document}` the driver copies the latest layout's `.aux` and `.bbl` next to
itself as `rtex-serve-g<generation>.aux`/`.bbl`. Packages that decide their mode from the aux
(natbib, hyperref) and biblatex's citations then start in the document's state.

### Channels

On **Unix**, requests go to the server's stdin and responses come back on a FIFO. The host opens
the FIFO's read end once, non-blocking, before the server gets there. It polls until the first
frame arrives, then switches to blocking reads. Closing and reopening the FIFO would lose
frames.

On **Windows**, the host creates two named pipes before spawning the server. Each has one
instance, accepts local clients only, and is not inheritable. The server opens the request pipe
in binary mode (`io.open(path, "rb")`) and then the response pipe. Stdin is not used because it
is in text mode on Windows: CR LF in a source would become LF, which breaks the byte counts,
and Ctrl-Z would end the input.

On both platforms, a reader thread on the host drains the response channel with blocking reads
and passes whole frames to the engine thread over a queue. Nothing polls. On macOS, `poll` did
not report a frame that arrived while it waited, and a FIFO there holds less than a large
display list. The server blocked on its write while the host waited out the watchdog. Draining
continuously means the server can always finish writing.

## Requests

A **compile** is a raw frame:

```
C <req> <ctx> <len>\n<len bytes of source>
C <req> <ctx> <len> <plen>\n<len bytes of source><plen bytes of JSON>
```

`<ctx>` names a context sent earlier. The optional JSON lists picture cache entries for picture
environments in the source, each with the 1-based source `line` its `\begin` is on. A picture
with an entry is placed from the cache, and one without is drawn.

Everything else is one JSON object per line:

| `op` | Fields | Reply |
|---|---|---|
| `context` | `id`, `ctx` (below) | `{"op":"ok","id":…}` |
| `labels` | `labels`: `[name, body]` pairs from the last pass's aux (`r@<label>`, `b@<key>`) | `{"op":"ok","id":count}` |
| `compile` | `req`, `ctx`, `source` (legacy JSON form of the frame above) | a `result` frame |
| `profile` | `ctx`, `source`, `n` | in-engine micro-timings (diagnostic) |
| `ping` | | `{"op":"pong"}` |
| `stats` | | requests served, font ids, node memory, group level, nesting, Lua memory |
| `shutdown` | | `{"op":"bye"}`, then the process ends |

### The context object

One per unit, built by the host from the latest capture (`EngineUnit::context_json`):

```json
{ "kind": "par" | "env" | "heading", "name": null | "itemize" | "figure" | "section" | …,
  "ints":  {"tolerance": 200, "language": 0, …},
  "dims":  {"hsize": 23592960, "parindent": 1114112, …},
  "glues": {"parfillskip": [0, 65536, 0, 2, 0], "baselineskip": [891290, 0, 0, 0, 0], …},
  "parshape": null,
  "everypar": "", "nobreak": false, "afterindent": true, "noskipsec": false,
  "counters": {"chapter": 1, "section": 2, "equation": 3, …},
  "thefmt":   {"section": "\\@Alph \\c@section ", …},
  "macros":   {"arraystretch": "->1.6", "ifglo@cpu@flag": "\\iftrue", …},
  "begin": { "nfss": {"enc": "TU", "family": "…", "series": "m", "shape": "n", "size": "10.95",
                      "baselineskip": "13.6pt"},
             "color": "0 g 0 G" } }
```

- `everypar` must be one of the kernel's own patterns: empty, `\leftprotrusion`, or what
  `\@afterheading`, `\@doendpe` or `\@setminipage` leave. The host refuses anything else.
- `macros` holds the meanings of macros the document body redefines, as `\meaning` prints them,
  and the server replays them as local definitions. A `\newif` switch (`\iftrue`/`\iffalse`,
  for example glossaries' first-use flags) is set locally for the compile and reset globally to
  its idle value afterwards.
- `thefmt` holds `\the<counter>` bodies, replayed when they differ from the server's.
- `counters` are the LaTeX counters at the unit's start. `page` is never replayed.

### What a compile does

1. If the font in the context differs from the last one, select it at the outer level with
   `\fontencoding…\fontsize…\selectfont`. Font ids differ between processes, so they are never
   used.
2. `\begingroup\global\setbox\rtexbox\vbox\bgroup`.
3. Set the parameters (only those that differ from the server's idle values), `\parshape`, the
   counters, the kernel switches, `\everypar` and the replayed macros. A float gets the state a
   real float box has (`\@captype`, `\hsize\columnwidth`, `\@parboxrestore`, `\@floatboxreset`),
   and its `\begin{figure}…\end{figure}` lines are stripped.
4. Feed the source lines with `tex.print`, then `\par\egroup\endgroup`.
5. Restore every counter to its idle value. Check for leaks: compare the meaning of every
   control sequence the source mentions, and the engine state fingerprint. Walk the box to make
   the display list, and send the result.

Each request is served in two halves without a nested TeX main loop. `step()` reads the request
and *prints* the tokens that build the box, ending with a `\luafunction` that runs `finish()`.
TeX executes them, and the `\loop` comes back to `step()`. A first version called `tex.runtoks`
from a `\directlua` that never returned, and leaked one input level per request, which killed
the server after about 10 000 requests.

## Responses

Frames are a `u32` little-endian length (of everything after it), a `u8` kind and the payload.

- Kind 0: a JSON object.
- Kind 1: a compile result: a `u32` JSON length, the JSON header, then the binary display list
  ([display-list.md](display-list.md)).

### `ready` (first frame)

```json
{"op":"ready","banner":"…","luatex_version":124,"fingerprint":"…","font_nextid":41}
```

### `result`

```json
{ "op": "result", "req": 7, "ctx": 25,
  "status": "ok" | "ok_degraded" | "error",
  "errors": [{"message": "Undefined control sequence", "context": "…", "line": 2}],
  "lines": 3, "glyphs": 166, "width": 22609920, "height": 0, "depth": 0, "dl_bytes": 5055,
  "t_tex_us": 1437, "t_traverse_us": 279, "t_pack_us": 0, "font_changed": false,
  "stages_us": {"decode": 9, "prepare": 8, "apply": 5, "fingerprint": 13, …} }
```

| Field | Meaning |
|---|---|
| `status` | `error` means TeX reported an error. A display list may still be present (TeX recovers in batch mode) but is not trusted. |
| `errors[].line` | counted from the first printed line, which is the replay head. Source line = `line − 1`. |
| `t_tex_us`, `t_traverse_us` | box build, and traversal plus encoding (`t_pack_us` is always 0) |
| `images` | `{index: {file, page, pages}}` for images the unit used |
| `counters` | counters whose value at the end differs from the start. The session compares this with what the layout saw and schedules a pass when it differs. |
| `leaks` | control sequences the source mentions whose meaning changed (`\gdef`, `\global\let`). The session demotes the unit and restarts the engine. |
| `pics_seen`, `pics_used` | pictures the compile began, and how many came from the cache. A `pics_seen` that differs from the session's count means a picture made by a macro, so the session repeats the compile without the cache. |
| `internal` | a Lua error in the server's own code, with traceback. It is not a source error. The unit waits for the pass, and the engine keeps running. |

### `fatal`

```json
{"op":"fatal","req":7,"reason":"state_mismatch","before":"…","after":"…","errors":[…]}
```

The engine state fingerprint after the request differs from the idle baseline. The fingerprint
covers the group level, nesting, interaction mode, `\everypar`, the catcodes of the special
characters, `\count0–9` and a few others, plus the current font. The server exits with status 3
and the host starts a new generation.

## Server-only shortcuts

`tex/latex/rtex-serve-patches.tex` removes work whose result cannot be seen inside a unit box,
or memoizes deterministic work. Each patch first checks that the macro it replaces still has
the definition it was written against (`\ifx` against a copy), and is skipped otherwise.

| Patch | Saves |
|---|---|
| `\markright`/`\markboth` insert no marks (only their `\nobreak` after a heading remains): the box is never shipped, so no running head reads them | about 150 µs per `\section` |
| `\glb@settings` memoizes the math font setup per (math version, size). The kernel rebuilds about 40 fonts whenever math is entered at a new size, which a footnote mark does twice | about 0.7 ms per footnote paragraph with microtype |
| graphics: the two file-existence probes per `\includegraphics` are remembered for files that exist, and log-only messages are dropped | about 170 µs per figure |

`rtex verify` compares every kind of unit against a full run without these patches.

## Facts about LuaTeX the design relies on

These were established by the experiments in `tex/experiments/` (TeX Live 2026, LuaHBTeX 1.24):

- Every paragraph parameter can be read and set from Lua (`tex.get`/`tex.set`,
  `tex.getglue`/`tex.setglue`).
- LaTeX's `para/begin` hook pairs with `pre_linebreak_filter` only for top-level paragraphs.
  Text after display math continues the same TeX paragraph without a new `para/begin`, and
  paragraphs inside `\footnote` or `\parbox` arrive with `groupcode == "vbox"`. Paragraph
  identity is therefore driven by `pre_linebreak_filter`.
- `show_error_hook` fires in batch mode, and an unbalanced `{` is visible as a raised group
  level, hence the fingerprint check.
- `\ShipoutBox` can be walked in `shipout/before`, and attributes set in `post_linebreak_filter`
  survive to shipout.
- The shipout box sits at (1in + `\hoffset`, 1in + `\voffset`) from the page's top-left corner.
- Glyph `expansion_factor` is in millionths, and the advance is
  `round_xn_over_d(width, 1000 + ef/1000, 1000)`. On kerns, `expansion_factor` is the expansion
  *amount* in sp, not a ratio.
- The walk reproduces the backend's own cursor to the scaled point: 0 sp difference on every
  glyph, measured with a `late_lua` oracle ([correctness.md](correctness.md)). LuaTeX's PDF
  differs from its cursor by up to one `TJ` unit (font size / 1000 bp), because `TJ`
  adjustments are integers.
- Font ids grow once on the first compile (math fonts load) and then stay constant over
  hundreds of compiles. Node memory stays at the size of one paragraph.
- With fontspec's default node mode, luaotfload shapes every paragraph in Lua. That is several
  times the cost of line breaking. Base mode (`Renderer=Basic`) and TFM fonts use the engine's
  own tables ([benchmarks.md](benchmarks.md)).
