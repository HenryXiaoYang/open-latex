# Display list format

A display list is what rtex gives a host to draw: glyphs from font files at exact positions,
rules, images and colour changes. It is read straight from LuaTeX's internal boxes, without
going through a PDF, and it places every glyph exactly where LuaTeX's own PDF writer puts it
([correctness.md](correctness.md)).

Format revision: **1** (rtex 0.0.2). The revision is in the header, and readers must skip
record types they do not know.

## Coordinates and units

- All lengths are TeX **scaled points** (sp): 65536 sp = 1 pt, and 65781.76 sp = 1 PostScript
  point (bp).
- x grows to the right and y grows **downward**.
- A **unit list** (from a live update) has its origin at the top-left corner of the unit's
  box. Its `lines` are the unit's rows, in order. A `ParagraphUpdate`'s `fragments` say where
  each row goes on the page ([embedding.md](embedding.md#paragraphupdate-a-unit-was-re-typeset)).
- A **page list** (from a layout) has its origin at the top-left corner of the page. Each line
  carries the unit and row it belongs to. Material that belongs to no unit (headers, footers,
  footnote rules, page-level colour) is in `other`, which comes before the lines.

A *row* is a line box reached from the unit through vertical boxes only: a text line, a
display-math row, a list item line, a float's image line or caption line, a table's outer box.
Footnote text keeps the paragraph it came from (`par`) but belongs to no unit, so a live update
of the paragraph replaces its rows and not its footnote text.

## Drawing it

**Glyphs.** Draw glyph `glyph_index` of the font file `filename` (subfont `subfont` for
collections) at `(x, baseline_y)`, at size `size_sp`. Then apply three adjustments:
- scale it horizontally by `1 + expansion / 1 000 000` (microtype font expansion; LuaTeX
  writes the same factor into the PDF);
- if `extend` is non-zero, also scale it by `extend / 1000`;
- if `slant` is non-zero, shear it by `slant / 1000`.

`x` and `baseline_y` already include the glyph's offsets. `char_code` is the character, for
text extraction and for TFM/Type 1 fonts, which have no glyph index. `advance` is how far TeX
moved after the glyph. It is informational only, because positions are absolute.

**Rules** are filled rectangles.

**Images** refer to an image resource index. `IMAGE_INFO` records give the file, the page (for
PDF images) and the page count. graphicx draws a bitmap at its natural size inside a matrix
group:
- `MATRIX save`, then `MATRIX set "a b c d"` about a point `(x, y)`;
- the image;
- `MATRIX restore`.

Apply the matrix about that point to the image rectangle.

**Colour** records are LuaTeX colour-stack operations. `data` holds the raw PDF colour operators
(for example `1 0 0 rg 1 0 0 RG`). `set` replaces the top of the stack, and `push`/`pop` nest.
A unit list starts with a `set` when the colour at its start is not black.

**Fonts** have per-process numeric ids. To recognise the same font across lists and processes,
compare file, subfont, size, slant, extend and squeeze (`FontDesc::key()` in Rust).

**Degraded content.** A list that contains any `FLAG`, `LITERAL` or `UNSUPPORTED` record is
*degraded*: something on it could not be described. Draw that page from the layout's
`pdf_fallback` PDF instead. `MATRIX` records do not degrade a page.

A picture taken from the picture cache appears as
`UNSUPPORTED { kind: "cached_picture", detail: "<index> <x> <top> <width> <height>" }`. That is
the picture's rectangle in the list's coordinates, and the page carries the `pic_cache` flag. In
a live result, this tells a host that draws the page from the PDF where the picture now stands.
It can copy that rectangle from its rendering of the layout PDF (same size) before drawing the
unit's rows.

## Binary encoding

Little-endian throughout. `str` is a `u16` byte length followed by UTF-8 bytes.

```
Header, 16 bytes
  'L' 'O' 'D' 'L'      magic
  u16 version          1
  u16 flags            bit 0: 1 = page list, 0 = unit list
  u32 total_length     bytes, including this header
  u32 reserved         0

Records, until END: u8 tag, u32 payload_length, payload
  (skip any tag you do not know, using payload_length)

  0x01 META        i32 width, i32 height, i32 depth, i32 page, i32 page_width, i32 page_height,
                   i32 origin_x, i32 origin_y, u32 glyph_count, u32 image_count
                   In unit lists, origin_y holds the number of inserts (footnotes, marginal
                   notes) the unit produced; their text is not in the list.
  0x02 FONT        u32 font_id, i32 size_sp, u8 kind, u8 reserved, u16 subfont,
                   i32 slant, i32 extend, i32 squeeze, i32 designsize,
                   str filename, str psname, str name, str fullname, str format
                   kind: 0 unknown, 1 OpenType, 2 TrueType, 3 Type 1, 4 Type 3
                   (virtual fonts are expanded into their real fonts and never appear)
                   slant/extend/squeeze in thousandths; 0 means unset
  0x03 FLAG        str key, i32 value                 reason the list is degraded
  0x10 LINE        i32 par, i32 line_index, i32 x, i32 baseline_y, i32 width, i32 height,
                   i32 depth, f64 glue_set, u8 glue_sign, u8 glue_order
  0x12 LINE_UNIT   i32 unit, i32 row                  page lists only, right after LINE
  0x11 LINE_END
  0x20 GLYPHS      u32 font_id, i32 baseline_y, i32 expansion, u32 n,
                   n × { u32 char_code, u32 glyph_index (0xFFFFFFFF = none), i32 x, i32 advance }
  0x21 RULE        i32 x, i32 y_top, i32 width, i32 height
  0x22 COLOR       u8 cmd (0 set, 1 push, 2 pop, 3 current, 255 unknown), u8 reserved,
                   u16 stack, str data
  0x23 LITERAL     i32 mode, str data                 raw PDF literal or \special (mode −1)
  0x24 UNSUPPORTED str kind, str detail
  0x25 MATH        u8 on, i32 x                       inline math boundary, for hit-testing
  0x26 IMAGE       i32 resource_index, i32 x, i32 y_top, i32 width, i32 height
  0x27 IMAGE_INFO  u32 resource_index, u32 page, u32 pages, str file
  0x28 MATRIX      u8 op (0 save, 1 set, 2 restore), i32 x, i32 y, str data ("a b c d")
  0xFF END
```

Decoders: `rtex_dl::DisplayList::from_binary` in Rust, and `rtex_dl_to_json` in the C library.
Inside TeX, the encoder is `tex/rtex-dl-bin.lua`.

## JSON form

`rtex dl2json <file>` and `rtex_dl_to_json` turn the binary form into JSON with the same
content. `rtex serve` sends display lists in this form.

```json
{
  "kind": "paragraph" | "page", "unit": "sp",
  "width": 0, "height": 0, "depth": 0,
  "page": 0, "page_width": 0, "page_height": 0, "origin": [0, 0],
  "fonts":  { "<id>": { "id", "size", "filename", "psname", "subfont", "slant", "extend", … } },
  "images_info": { "<index>": { "file", "page", "pages" } },
  "flags":  { "<reason>": 1 },
  "glyphs": 0, "inserts": 0,
  "other":  [ /* items that belong to no line, as below */ ],
  "lines": [
    { "par": 0, "i": 1, "x": 0, "y": 0, "w": 0, "h": 0, "d": 0,
      "gs": 0.0, "gsign": 0, "gorder": 0,
      "unit": 0, "row": 1,            // page lists only
      "items": [ … ] }
  ]
}
```

| Item | Fields | Meaning |
|---|---|---|
| `["g", font, char, index, x, y, advance, expansion]` | `index` is null for TFM fonts | glyph |
| `["r", x, y_top, w, h]` | | rule |
| `["c", stack, cmd, data]` | `cmd`: 0 set, 1 push, 2 pop, 3 current | colour |
| `["l", mode, data]` | | PDF literal or `\special`; degrades the list |
| `["u", kind, detail]` | | something not representable; degrades the list |
| `["m", "on"\|"off", x]` | | inline math boundary |
| `["i", index, x, y_top, w, h]` | | image |
| `["M", "save"\|"set"\|"restore", x, y, data]` | | transformation |
