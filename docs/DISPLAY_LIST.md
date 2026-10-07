# Display list format (lode 0.0.2, binary encoding revision 1)

A display list is what lode hands a host to draw: positioned glyphs, rules, images, color
operations and markers, in scaled points (sp; 65536 sp = 1 pt; 65781.76 sp = 1 bp), y growing
downward. Two framings exist:

- **paragraph** display lists (fast-path results): origin at the top-left of the paragraph box;
  `lines` are the paragraph's lines in order. Hosts position them with the `fragments` of the
  `ParagraphUpdate` (per-line baselines in page coordinates).
- **page** display lists (background layouts): origin at the top-left of the page; `lines` carry
  the owning paragraph (`par`, the capture sequence number) and `other` holds page material that
  belongs to no paragraph (headers, footers, floats' rules, page-level color ops).

Within a line, items are in content order. `other` precedes the lines.

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
  0x11 LINE_END
  0x20 GLYPHS   u32 font_id, i32 baseline_y, i32 expansion, u32 n,
                n × { u32 char_code, u32 glyph_index (0xFFFFFFFF = none), i32 x, i32 advance }
  0x21 RULE     i32 x, i32 y_top, i32 width, i32 height
  0x22 COLOR    u8 cmd (0 set, 1 push, 2 pop, 3 current, 255 unknown), u8 reserved, u16 stack, str data
  0x23 LITERAL  i32 mode, str data           raw pdf_literal / \special (mode −1); page is Degraded
  0x24 UNSUPPORTED str kind, str detail      something not representable; page is Degraded
  0x25 MATH     u8 on, i32 x                 inline math boundary (hit-testing aid)
  0x26 IMAGE    i32 resource_index, i32 x, i32 y_top, i32 width, i32 height
  0xFF END
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
- **Images** reference the engine's image resource index; the layout's `images` map (capture
  JSON `images`, keyed by index) gives the file, page and page count.
- **Color** records are LuaTeX `pdf_colorstack` operations with the raw PDF color operators in
  `data` (e.g. `1 0 0 rg 1 0 0 RG`); `set` replaces the stack top, `push`/`pop` nest. A paragraph
  display list starts with a `set` of the color in force at its start when it is not black.
- **Fonts** are identified across processes by `FontDesc::key()` (file, subfont, size, slant,
  extend, squeeze); font ids are per-process.
- Pages with any FLAG, LITERAL or UNSUPPORTED record are *Degraded*: draw the PDF fallback page
  the `LayoutUpdate` names instead.

## JSON mirror

`lode dl2json` / `lode_dl_to_json` convert the binary form to the JSON shape used by
`lode-dl.lua` and `lode_dl::DisplayList` (`{"kind","unit","fonts","lines":[{"par","i","x","y","w",
"h","d","gs","gsign","gorder","items":[["g",font,char,index,x,y,w,ef],["r",x,y_top,w,h],
["c",stack,cmd,data],["l",mode,data],["u",kind,detail],["m","on"|"off",x],["i",index,x,y_top,w,h]]}],
"other":[…],"flags":{…},"glyphs":n,"width","height","depth","page","page_width","page_height","origin"}`).
Both encodings carry the same information; the binary one is what the engine emits.
