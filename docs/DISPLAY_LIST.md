# Display list format (rtex 0.0.2, binary encoding revision 1)

A display list is what rtex hands a host to draw: positioned glyphs, rules, images, color
operations and markers, in scaled points (sp; 65536 sp = 1 pt; 65781.76 sp = 1 bp), y growing
downward. Two framings exist:

- **unit** display lists (fast-path results, still called "paragraph" lists in the API): origin
  at the top-left of the unit's box; `lines` are the unit's **rows** in order. A row is an
  hlist reached from the box through vlists only: a text line, a display-math row, a list item
  line, a float's image line or caption line, a table row's outer box. Hosts position rows with
  the `fragments` of the `ParagraphUpdate` (per-row `xs`/`baselines` in page coordinates).
- **page** display lists (background layouts): origin at the top-left of the page; `lines` carry
  the owning unit (`unit`, with the 1-based `row` index) and, for text lines, the capture
  paragraph (`par`, `i`); `other` holds page material that belongs to no unit (headers, footers,
  footnote rules, page-level color ops). Footnote text lines keep their `par` but belong to no
  unit: a fast result for the paragraph replaces its rows, not its footnote text.

Within a row, items are in content order. `other` precedes the lines.

## Binary encoding (revision 1; the header carries the revision number)

Little-endian throughout. `str` = `u16 length` + UTF-8 bytes.

```
Header (16 bytes)
  'L' 'O' 'D' 'L'      magic
  u16 version          1
  u16 flags            bit 0: 1 = page, 0 = paragraph
  u32 total_length     bytes including this header
  u32 reserved         0
Records: u8 tag, u32 payload_length, payload   (unknown tags must be skipped)
  0x01 META     i32 width, i32 height, i32 depth, i32 page, i32 page_width, i32 page_height,
                i32 origin_x, i32 origin_y, u32 glyph_count, u32 image_count
  0x02 FONT     u32 font_id, i32 size_sp, u8 kind, u8 reserved, u16 subfont, i32 slant, i32 extend,
                i32 squeeze, i32 designsize, str filename, str psname, str name, str fullname, str format
                kind: 0 unknown, 1 opentype, 2 truetype, 3 type1, 4 type3, 5 virtual (never emitted: expanded)
                slant/extend/squeeze in thousandths (extend 1000 = none; 0 = unset)
  0x03 FLAG     str key, i32 value            degradation flags (page is Degraded when any is present)
  0x10 LINE     i32 par, i32 line_index, i32 x, i32 baseline_y, i32 width, i32 height, i32 depth,
                f64 glue_set, u8 glue_sign, u8 glue_order
  0x12 LINE_UNIT i32 unit, i32 row          (page lists; directly after LINE) owning unit and row index
  0x11 LINE_END
  0x20 GLYPHS   u32 font_id, i32 baseline_y, i32 expansion, u32 n,
                n × { u32 char_code, u32 glyph_index (0xFFFFFFFF = none), i32 x, i32 advance }
  0x21 RULE     i32 x, i32 y_top, i32 width, i32 height
  0x22 COLOR    u8 cmd (0 set, 1 push, 2 pop, 3 current, 255 unknown), u8 reserved, u16 stack, str data
  0x23 LITERAL  i32 mode, str data           raw pdf_literal / \special (mode −1); page is Degraded
  0x24 UNSUPPORTED str kind, str detail      something not representable; page is Degraded
  0x25 MATH     u8 on, i32 x                 inline math boundary (hit-testing aid)
  0x26 IMAGE    i32 resource_index, i32 x, i32 y_top, i32 width, i32 height
  0x27 IMAGE_INFO u32 resource_index, u32 page, u32 pages, str file     source file of an image index
  0x28 MATRIX   u8 op (0 save, 1 set, 2 restore), i32 x, i32 y, str data
                PDF transformation state (graphicx scaling/rotation): `set` applies the matrix
                "a b c d" about the point (x, y) to everything up to the matching `restore`
  0xFF END

In unit lists the META record's `origin_y` slot carries the number of insert nodes (footnotes,
marginal notes) the unit produced: their text is not in the list and keeps the page's version
until the next layout (`ParagraphUpdate.reasons` lists `inserts`).
```

## Rendering rules

- **Glyphs.** Draw glyph `glyph_index` of the font file `filename` (subfont `subfont` for
  collections) at `(x, baseline_y)`, scaled to `size_sp`, horizontally scaled by
  `1 + expansion / 1 000 000` (microtype font expansion; LuaTeX encodes it the same way in the
  PDF text matrix), then by `extend / 1000` when non-zero, with `slant / 1000` as horizontal
  shear. `char_code` is the Unicode/char code for text extraction and TFM fonts (`glyph_index`
  absent). `x` already includes `xoffset`; `baseline_y` includes `yoffset`. `advance` is the
  width TeX advanced by after this glyph (informational; positions are absolute).
- **Rules** are filled rectangles. LuaTeX draws them in the PDF as stroked lines of the same
  geometry; both render identically.
- **Images** reference the engine's image resource index; `IMAGE_INFO` records (page lists and
  fast results alike) give the file, page and page count. graphicx draws bitmap images at their
  natural size inside a `MATRIX save` / `set` / `restore` group: apply the matrix about its
  point to the image rectangle (`verify.rs` shows the composition). A picture the background
  pass took from the picture cache is an image too: the JSON capture marks it in `images_info`
  (`cached_picture: true`, `bbox` = the region of the earlier pass PDF, in sp of PDF user space)
  and the page carries the `pic_cache` flag, so it is degraded and hosts render it from the pass
  PDF; the binary `IMAGE_INFO` record does not carry these two fields.
- **Color** records are LuaTeX `pdf_colorstack` operations with the raw PDF color operators in
  `data` (e.g. `1 0 0 rg 1 0 0 RG`); `set` replaces the stack top, `push`/`pop` nest. A paragraph
  display list starts with a `set` of the color in force at its start when it is not black.
- **Fonts** are identified across processes by `FontDesc::key()` (file, subfont, size, slant,
  extend, squeeze); font ids are per-process.
- Pages with any FLAG, LITERAL or UNSUPPORTED record are *Degraded*: draw the PDF fallback page
  the `LayoutUpdate` names instead. MATRIX records do not degrade a page.

## JSON mirror

`rtex dl2json` / `rtex_dl_to_json` convert the binary form to the JSON shape used by
`rtex-dl.lua` and `rtex_dl::DisplayList` (`{"kind","unit","fonts","lines":[{"par","i","x","y","w",
"h","d","gs","gsign","gorder","items":[["g",font,char,index,x,y,w,ef],["r",x,y_top,w,h],
["c",stack,cmd,data],["l",mode,data],["u",kind,detail],["m","on"|"off",x],["i",index,x,y_top,w,h],
["M","save"|"set"|"restore",x,y,data]]}],
"other":[…],"flags":{…},"glyphs":n,"inserts":n,"images_info":{index:{file,page,pages}},"width","height","depth","page","page_width","page_height","origin"}`;
page lines also carry `"unit"` and `"row"`).
Both encodings carry the same information; the binary one is what the engine emits.
