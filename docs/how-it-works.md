# How rtex works

This page explains what happens between a keystroke and the updated page, and why the result
is exactly what LuaLaTeX would have produced. You do not need to read it to use rtex, but it
helps when you want to know why one paragraph updates instantly and another one waits a
moment.

## The problem

A LaTeX document is compiled as a whole. Changing one word in chapter 7 normally means running
LuaLaTeX over every page again, often more than once, before you see the result. For a
100-page document that is many seconds per keystroke.

But most edits only change one paragraph. The paragraph's line breaks depend on its own text
and on the settings in force where it stands: the font, the line width, the indentation, the
current section and equation numbers. Nothing else in the document changes how *that
paragraph* is typeset. rtex takes advantage of this: it re-typesets only the paragraph you are
editing, in milliseconds, and leaves everything that really is global (page breaks, float
placement, the table of contents, cross-references) to a full compile that runs in the
background.

The design follows Clemens Lode, *Real-Time LuaTeX: Recompiling Large Documents in 1 ms*
(TUGboat 2026). rtex uses an unmodified LuaTeX from TeX Live; nothing is patched.

## Two paths

```
 your editor ──edit──▶ rtex session
                         │
                         ├─▶ live engine (milliseconds)
                         │     one LuaLaTeX process that has already loaded your preamble and
                         │     waits inside \begin{document}. It typesets just the edited
                         │     paragraph and returns its lines as positioned glyphs.
                         │
                         └─▶ background pass (seconds, debounced)
                               a full LuaLaTeX run over a snapshot of your files. It produces
                               the pages, and records everything the live engine needs to know
                               about each paragraph for the next round of edits.
```

Your editor (the *host*) gets two kinds of updates:

- **Paragraph updates** come from the live engine, a millisecond or so after each keystroke.
  They contain the new lines of one paragraph and where on the page to draw them.
- **Layout updates** come from the background pass, a fraction of a second to a few seconds
  after you stop typing. They contain whole pages.

The host draws the last layout and paints paragraph updates over it. When the next layout
arrives, it replaces everything.

## Units: what gets re-typeset

rtex splits each source file into *spans*: stretches of text between blank lines, plus
environments and headings. Each span corresponds to one *unit* that LuaLaTeX typesets as a
whole:

| Unit | Examples |
|---|---|
| paragraph | ordinary text, including inline math, display math (`\[ \]`, `equation`, `align`), footnote marks, `\ref` and `\cite` |
| block environment | lists, `quote`, `center`, theorems and proofs, figures and tables with images, captions and `tabular`, verbatim and listings, `thebibliography`, the title block |
| heading | `\chapter`, `\section`, … |

A unit's output is a set of *rows*: its text lines, display-math rows, list item lines, a
figure's image and caption lines. Rows are what gets placed on the page.

## What the background pass records

Every background pass is a normal LuaLaTeX run with one extra package loaded (`rtex-capture`).
The package does not change the output; CI checks that the PDF is identical with and without
it. While the document is typeset, it records, for every unit:

- **The context**: the state the unit is typeset in. That is the paragraph parameters
  (`\hsize`, `\parindent`, `\baselineskip`, tolerance and friends), the font, the colour, the
  LaTeX counters (section, equation, figure, footnote, …), the `\the<counter>` formats, and the
  meaning of every macro the document body redefines.
- **The rows and where they landed**: page, position and size of every row, after LaTeX has
  broken the pages and placed the floats.
- **The page contents**: a display list per page (see [display-list.md](display-list.md)).

It also runs `biber` or `bibtex` and `makeindex` (or the program imakeidx asks for) when the
document needs them. The pass repeats until the auxiliary files stop changing, which is what
makes cross-references and the table of contents settle. That usually takes one or two runs,
and at most five. See [Convergence](#convergence) below.

## What happens on a keystroke

1. **The edit is applied.** rtex updates its copy of the file and re-splits only the area
   around the edit. Spans keep their ids, so the host knows which paragraph changed.
2. **rtex decides whether the unit can be typeset live** (see the next section). If not, the
   edit simply waits for the next background pass, and the host is told why
   (`BackgroundScheduled`).
3. **The live engine typesets the unit.** It restores the unit's context from the last pass,
   typesets the new text into a box, puts the counters back, and walks the resulting LuaTeX
   node list to produce a display list. This is the same walk the capture package does on the
   real pages, so both produce identical output.
4. **The result is placed.** rtex positions the new rows where the unit's rows were in the last
   layout. If the unit now has more or fewer rows, the result says so (`pagination_stale`) and
   a background pass is scheduled, because the text after it has to move.
5. **The host draws it** and replaces whatever the old version of that unit drew.

A result that arrives after the text changed again is thrown away; only the newest version of
each unit is ever delivered.

## Deciding what can go live

A live compile is only correct if typesetting the unit alone gives exactly what the full run
gives, and if it leaves nothing behind in the live engine that would affect later compiles.
rtex checks this in layers.

**Structure.** The span must map to exactly one unit of the last layout (or be new text next to
one), its braces must balance, and it must not be something whose output is decided by the
page builder. Footnote *text*, marginal notes and floats being moved are such things, and so
are run-in headings like `\paragraph`.

**Vocabulary.** rtex keeps an allow-list of commands that are known to depend only on the state
it restores: text formatting, math, references, citations, common packages (hyperref, siunitx,
listings, booktabs, enumitem, amsthm, …) and macros you define yourself whose bodies use only
such commands. A unit made of allow-listed commands goes live straight away.

**Probe.** A unit that uses anything else is not rejected. The first time you edit it after a
layout, rtex compiles the unit's text *as the last pass saw it* and compares the result, row by
row and glyph by glyph, with what the pass put on the page. If they match, the unit is verified
and your edits to it go live until the next layout. If they differ, the unit stays on the
background path, and the reason is reported. This costs one extra compile (1–5 ms) per unit
per layout. You can switch it off and use the allow-list only (`eligibility: allowlist`).

**Leaks.** After each compile the live engine checks that nothing escaped the unit:
- It compares the meaning of every macro the unit mentions before and after the compile. A
  `\gdef` changes a meaning, so it is caught.
- It takes a fingerprint of the engine state: group level, nesting, catcodes, `\everypar` and
  a few registers.

A unit that leaks is sent to the background path, and the engine is restarted.

**Time.** A live compile has a budget (50 ms by default). A unit that goes over it three times in
a row waits for the background pass until the next layout. The first slow compiles are
forgiven, because they are usually loading a font. A compile that hangs is killed after 5
seconds, and the engine restarts.

## New paragraphs and splits

When you press Enter twice in the middle of a paragraph, or type a new paragraph, the new span
has no unit in the last layout yet. rtex lets it borrow the context of its neighbour (same
font, same parameters, same counters), typesets it live, and places it below the span before
it. Such results are marked `approximate` and `context_stale`, because counters may be one
behind and the vertical spacing is estimated. The next layout replaces them with the real
thing. A new float waits for the background pass, since only the pass knows where it goes.

## Making background passes fast

**A standby engine.** Loading the format and the preamble is most of the cost of a short LuaLaTeX
run. While you type, rtex keeps a second LuaLaTeX ready with the preamble already loaded,
blocked until rtex hands it the body. A pass after the first one then costs about a quarter of
a full run. Line numbers and file names are preserved, so error messages point to the right
place.

**A picture cache.** TikZ and pgfplots pictures are usually the slowest part of a document and
rarely change. After each pass rtex cuts every picture out of the PDF it produced. In the next
pass, a picture whose source, preceding definitions, font, colour and line width are all
unchanged is not drawn again. It is replaced by an image of exactly the same size, taken from
the earlier PDF. Pictures that depend on anything outside themselves are always drawn:
references, counters, `remember picture`, external data files and global assignments.

On a 111-page document with 120 pictures this took a pass from 45 s to 18 s. The live engine
uses the same cache, so editing a sentence that shares a unit with a plot costs the sentence,
not the plot.

**Provisional layouts.** When a document needs several passes and each takes two seconds or
more, rtex delivers each finished pass right away (`Converging`) instead of waiting for the
last one. Only page numbers and references may still change.

## Convergence

Every layout update says how final it is:

| State | Meaning |
|---|---|
| `Converged` | The auxiliary files stopped changing, nothing asked for a rerun, there were no errors, and you have not typed since the snapshot. This is what a clean LuaLaTeX build would produce. |
| `Converging` | Another pass is needed and is already running. |
| `PassLimitReached` | Five passes did not settle (or a bibliography tool is missing). This is never reported as converged. |
| `Stale` | You edited while the pass ran; another pass is already scheduled. |

Separately, the compile status is `Ok`, `CompiledWithErrors` (pages were produced but the log
has errors: the layout is partial), or `Failed` (no pages; the previous layout stays).

The files compared between passes are the `.aux` (including those of `\include`d chapters),
`.toc`, `.lof`, `.lot`, `.out`, `.bcf`, `.bbl`, `.idx` and every `.ind`.

## Keeping results in order

Edits, passes and engine restarts happen concurrently, so every event carries four counters:

| Counter | Goes up when |
|---|---|
| `source_revision` | any file is edited |
| `context_revision` | a layout installs new contexts |
| `engine_generation` | the live engine restarts (preamble change, crash, timeout, leak) |
| `layout_version` | a layout is delivered |

rtex uses them internally to drop results that are out of date. For example, a paragraph
result from a previous engine generation, or for text that has changed since, is never
delivered. Hosts can use them to order what they draw. Editing the preamble restarts the live
engine with the new preamble and schedules a pass. While that happens, edits go to the
background path.

## Where the code is

| Piece | File |
|---|---|
| Session API, shared state | `crates/rtex-core/src/session/mod.rs` |
| Splitting files into spans | `crates/rtex-core/src/document.rs` |
| Live or background, borrowed contexts | `crates/rtex-core/src/session/route.rs`, `eligibility.rs` |
| The live engine process and its channel | `crates/rtex-core/src/engine.rs`, `transport.rs` |
| Turning engine results into updates | `crates/rtex-core/src/session/engine_loop.rs` |
| Background passes, standby engine | `crates/rtex-core/src/session/passes.rs`, `background.rs` |
| Layouts, placements | `crates/rtex-core/src/layout.rs` |
| Picture cache | `crates/rtex-core/src/piccache.rs`, `tex/rtex-pic.lua` |
| Live engine (Lua side) | `tex/rtex-serve.lua`, `tex/latex/rtex-serve-patches.tex` |
| Node list → display list | `tex/rtex-dl.lua`, `tex/rtex-dl-bin.lua` |
| Capture package | `tex/latex/rtex-capture.sty`, `tex/rtex-capture.lua` |

The live engine's wire protocol is described in [engine-protocol.md](engine-protocol.md).
