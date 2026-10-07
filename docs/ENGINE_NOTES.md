# Engine notes — what the M0 experiments established

Experiments live in `tex/experiments/`; run them from that directory with
`lualatex -interaction=nonstopmode -output-directory=../../build/exp eN-*.tex` and
`max_print_line=100000` in the environment (otherwise the log wraps the result lines).

| Id | Question | Result (TeX Live 2026, LuaHBTeX 1.24.0) |
|---|---|---|
| E1 | Are all paragraph-context parameters readable/settable from Lua? | **Yes.** All 20 integer, 6 dimension and 7 glue parameters round-trip through `tex.get/set` and `tex.getglue/setglue`; `tex.parshape` is `nil` when unset. |
| E2 | Does LaTeX's `para/begin` hook pair with `pre_linebreak_filter`? | **Only for top-level paragraphs.** Pairing by `tex.nest.ptr` works for main-vertical-list paragraphs (`groupcode == ""`, nest 1). Paragraphs inside `\footnote`/`\parbox` arrive as `groupcode == "vbox"`; text *after* display math continues the same TeX paragraph and reaches the filter **without** a new `para/begin`; list items are top-level paragraphs with a non-default `\everypar`. Consequence: paragraph identity is driven by `pre_linebreak_filter` (sequence number, `groupcode`, end line) with `para/begin` data (start line, NFSS state) attached when the nest levels match; anything else is background-only. |
| E3 | Which error hooks fire; what does the engine report after an error? | `show_error_hook` and `show_error_message` fire (registered through `luatexbase.add_to_callback`; raw `callback.register` is refused under LaTeX). `status.lasterrorstring` holds the last error text. An unbalanced `{` leaves `tex.currentgrouplevel` at 1 — visible, hence the post-request fingerprint check. Interaction mode 1 = nonstop. |
| E4 | Is `\ShipoutBox` traversable in `shipout/before`, and do attributes set in `post_linebreak_filter` survive? | **Yes.** All 6 tagged line hlists were found in the shipped page box. Page box: 380 pt × 627.4 pt; `\pagewidth` 597.5 pt, `\pageheight` 845.0 pt; `\hoffset = \voffset = 0`. |
| E5 | Can Lua inside `lualatex` write to a FIFO and read stdin, and is stdout clean? | FIFO open succeeds with `openout_any = p`; 1000 framed `write+flush` calls cost **1.13 µs each**; `io.stdin:read` works in batch mode. **stdout is not clean**: the banner and "restricted system commands enabled" are printed even in batch mode, so responses must use the FIFO. |
| E7 | Versions, APIs, node fields | Lua 5.3, `string.pack` present, `luaharfbuzz` present, `node.direct` getters incl. `getexpansion`, `getoffsets`, `getglue`, `effective_glue`. `os.gettimeofday()` tick ≈ 1 µs. Glyph fields include `xoffset yoffset expansion_factor`; `kern` has `expansion_factor`; `math` has glue fields (`width stretch shrink …`); `margin_kern` has `width glyph`; `rule` has `index left right`; `local_par` has `box_left*`/`box_right*`; `whatsit.pdf_colorstack` = `stack cmd data` (note `cmd`, not `command`); `pdf_literal` = `mode data`. `characters[c]` carries `index`, `width`, `height`, `tounicode`. |

Open items carried into M1: E6 (unit of `expansion_factor`, confirmed against the PDF), E8 (glue rounding),
E9 (`tex.print` vs file injection), E10 (long-run stability of the serve loop).

## M1 additions

| Id | Question | Result |
|---|---|---|
| E8 | Which glyph width does the engine use, and do font ids grow under repeated `\selectfont`? | `font.getfont(f).characters[c].width` can be fractional (luaotfload scales in floating point, e.g. 358809.5) while the engine uses the rounded integer; `node.direct.getwidth(glyph)` returns that engine width and agrees with `node.hpack`. Font ids grow once on the first compile (math fonts, 28 → 41) and then stay constant over 40 (and later 400) compiles. |
| E9 | What is `expansion_factor` on kern nodes? | Not a ratio: it is `ex_kern`, the precomputed expansion **amount in sp** (e.g. kern −14352 carries −287 at 2 % shrink). LuaTeX's backend advances by `width + ex_kern` (`pdflistout.c`, `kern_width`). Glyph nodes carry the ratio in millionths (`fix_expand_value(f, e) * 1000`, e.g. ±20000 for 2 %), and the advance is `round_xn_over_d(w, 1000 + ef/1000, 1000)` (`texfont.c`, `calc_char_width`). |
| E10 | Does the traversal reproduce the backend's cursor exactly? | **Yes, 0 sp difference on 369/369 glyphs** across lines with expansion factors −20000, −4000, 0, 3000, 20000, including font kerns, inter-word glue, inline math and italics. Method: a zero-width `late_lua` whatsit is inserted before every glyph in `post_linebreak_filter`; at shipout it runs `pdf.getpos()`, which is the backend's own position. This "backend oracle" is independent of both the traversal and any PDF parsing and is kept as a verification tool (`tex/experiments/e10-oracle.tex`). |
| E11 | Page origin | The shipout box sits at (1in + `\hoffset`, 1in + `\voffset`) from the page's top-left; confirmed against PDF `Tm` operands (117.828 bp / 138.624 bp) to the 3 decimals LuaTeX writes. |
| — | How far is LuaTeX's *PDF* from its own cursor? | The PDF writer emits `TJ` adjustments in integer thousandths of the text-space unit, so glyph origins deviate from the backend cursor by up to one TJ unit (= font size / 1000 bp; measured max 0.0103 bp vs a 0.0109 bp quantum at 10.95 pt), independent of expansion. Glyphs placed directly by `Tm` match to the written precision (10⁻³ bp). Rules are written as stroked lines (`cm`, `w`, `m`, `l`, `S`), not `re` rectangles. |

## M6 additions

| Id | Question | Result |
|---|---|---|
| E12 | Where does the fast-path TeX stage spend its time? | The server's `profile` request times the pieces in place: empty `tex.runtoks` 1 µs, parameter replay (`tex.set` × 33 + `\parshape`) 6 µs, `\begingroup…\vbox…\endgroup` without text 12 µs, state fingerprint 6 µs; a full compile equals `tex.print` of the source alone. The TeX stage is the paragraph's own typesetting. |
| E13 | How much does the font stack cost? | Same paragraph, same microtype settings: TFM Latin Modern 0.28 ms; fontspec TeX Gyre Pagella `Renderer=Basic` 0.29 ms; fontspec default node mode 1.70 ms; `Renderer=HarfBuzz` 2.22 ms; CM/OT1 (the paper's benchmark file) 0.95 ms. luaotfload's node and HarfBuzz modes shape every paragraph in Lua; base mode uses engine-native ligature/kern tables. See BENCHMARKS.md. |
| — | Font selection caching | Selecting the NFSS font globally only when a context's font state changes (instead of inside every request's group) saves ≈ 0.1 ms per request; the server records `t_font_us` and `font_changed` in each result header. |
