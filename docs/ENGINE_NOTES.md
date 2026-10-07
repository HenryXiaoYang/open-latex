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
