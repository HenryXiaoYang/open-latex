-- E8: glyph width sources (font table float vs engine) and font-id growth under repeated \selectfont.
local D = node.direct
local box = tex.box[0]
local n = 0
for g in node.traverse_id(node.id("glyph"), tex.box[0].list) do
  local tfm = font.getfont(g.font)
  local ch = tfm.characters[g.char]
  local hp = node.hpack(node.copy(g))
  local dw = D.getwidth(D.todirect(g))
  texio.write_nl("term and log", string.format("E8 char=%d table_width=%s math.type=%s direct.getwidth=%s hpack=%d",
    g.char, tostring(ch.width), math.type(ch.width), tostring(dw), hp.width))
  node.flush_list(hp)
  n = n + 1
  if n >= 4 then break end
end
