-- E4: \ShipoutBox traversal inside shipout/before; attributes from post_linebreak_filter survive.
E4 = { lines = 0, seen = 0, pages = 0 }
E4.attr = luatexbase.new_attribute("rtex_par")
luatexbase.add_to_callback("post_linebreak_filter", function(head)
  for n in node.traverse_id(node.id("hlist"), head) do
    node.set_attribute(n, E4.attr, 42); E4.lines = E4.lines + 1
  end
  return true
end, "e4")
function E4.shipout(boxnum)
  E4.pages = E4.pages + 1
  local b = tex.box[boxnum]
  local function walk(list)
    for n in node.traverse(list) do
      local id = n.id
      if id == node.id("hlist") or id == node.id("vlist") then
        if node.has_attribute(n, E4.attr) == 42 then E4.seen = E4.seen + 1 end
        walk(n.list)
      end
    end
  end
  if b then walk(b.list) end
  texio.write_nl("term and log", string.format(
    "E4 page %d box=%s w=%s h=%s d=%s hoffset=%s voffset=%s pagewidth=%s pageheight=%s tagged_seen=%d tagged_total=%d",
    E4.pages, tostring(b), tostring(b and b.width), tostring(b and b.height), tostring(b and b.depth),
    tostring(tex.hoffset), tostring(tex.voffset), tostring(tex.pagewidth), tostring(tex.pageheight), E4.seen, E4.lines))
end
