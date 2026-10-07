-- E9: expansion_factor on kern nodes in expanded lines, and hpack width vs sum of parts.
local D = node.direct
luatexbase.add_to_callback("post_linebreak_filter", function(head)
  local ln = 0
  for line in node.traverse_id(node.id("hlist"), head) do
    ln = ln + 1
    local parts = {}
    local sumw, nglyph, nkern, nkern_ef = 0, 0, 0, 0
    local ef_set = {}
    for n in node.traverse(line.list) do
      if n.id == node.id("glyph") then
        nglyph = nglyph + 1; ef_set[n.expansion_factor] = true
      elseif n.id == node.id("kern") then
        nkern = nkern + 1
        if n.expansion_factor and n.expansion_factor ~= 0 then nkern_ef = nkern_ef + 1 end
        if nkern <= 3 then parts[#parts+1] = string.format("kern subtype=%d kern=%d ef=%s", n.subtype, n.kern, tostring(n.expansion_factor)) end
      elseif n.id == node.id("glue") and nkern <= 3 and #parts < 8 then
        parts[#parts+1] = string.format("glue subtype=%d w=%d st=%d sh=%d", n.subtype, n.width, n.stretch, n.shrink)
      end
    end
    local efs = {}
    for k in pairs(ef_set) do efs[#efs+1] = tostring(k) end
    texio.write_nl("term and log", string.format("E9 line %d glue_set=%.9g sign=%d glyphs=%d kerns=%d kerns_with_ef=%d efs=%s | %s",
      ln, line.glue_set, line.glue_sign, nglyph, nkern, nkern_ef, table.concat(efs, ","), table.concat(parts, "; ")))
    if ln >= 3 then break end
  end
  return true
end, "e9")
