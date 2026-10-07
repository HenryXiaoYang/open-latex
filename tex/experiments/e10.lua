-- E10: backend position oracle. Insert zero-width late_lua whatsits before every glyph of the
-- tagged lines; at shipout the backend executes them and pdf.getpos() reports its cursor.
-- Compare with lode-dl's traversal of the same page box.
local dl = dofile(kpse.find_file("lode-dl.lua", "lua") or "../lode-dl.lua")
E10 = { oracle = {}, n = 0, attr = luatexbase.new_attribute("lode_par"), attr_line = luatexbase.new_attribute("lode_line"), seq = 0 }
function E10.record(id)
  local x, y = pdf.getpos()
  E10.oracle[id] = { x = x, y = y }
end
luatexbase.add_to_callback("post_linebreak_filter", function(head, groupcode)
  if groupcode ~= "" then return true end
  E10.seq = E10.seq + 1
  local li = 0
  for line in node.traverse_id(node.id("hlist"), head) do
    li = li + 1
    node.set_attribute(line, E10.attr, E10.seq)
    node.set_attribute(line, E10.attr_line, li)
    local n = line.list
    while n do
      local nxt = n.next
      if n.id == node.id("glyph") then
        E10.n = E10.n + 1
        local w = node.new("whatsit", "late_lua")
        w.data = string.format("E10.record(%d)", E10.n)
        -- remember which oracle id belongs to this glyph via an attribute
        node.set_attribute(n, E10.attr_line, E10.n)
        line.list = node.insert_before(line.list, n, w)
      end
      n = nxt
    end
  end
  return true
end, "e10")
function E10.shipout(boxnum)
  -- traverse first (positions computed), oracle fills during actual shipout afterwards
  E10.page = dl.page(tex.box[boxnum], E10.attr, nil, 1, E10.attr_line)
  -- stash glyph oracle ids in traversal order: re-walk lines to pair glyphs with ids

end
function E10.report()
  local page = E10.page
  local ph = tex.pageheight
  local k, bad, total, shown = 0, 0, 0, 0
  local maxdx = 0
  for _, line in ipairs(page.lines) do
    local prev_item
    for _, it in ipairs(line.items) do
      if it[1] == "g" then
        k = k + 1
        local id = it[9]
        local o = id and E10.oracle[id]
        if o then
          total = total + 1
          local dx = o.x - it[5]
          local dy = (ph - o.y) - it[6]
          if math.abs(dx) > maxdx then maxdx = math.abs(dx) end
          if dx ~= 0 or dy ~= 0 then
            bad = bad + 1
            if shown < 15 then
              shown = shown + 1
              texio.write_nl("term and log", string.format("E10 line %s glyph char=%d x_dl=%d x_backend=%d dx=%d dy=%d ef=%d gs=%.6f sign=%d",
                tostring(line.i), it[3], it[5], o.x, dx, dy, it[8], line.gs, line.gsign))
            end
          end
        end
      end
    end
  end
  texio.write_nl("term and log", string.format("E10 compared %d glyphs: %d differ, max |dx| = %d sp (oracle entries %d)", total, bad, maxdx, E10.n))
  -- dump oracle (backend cursor) positions for external comparison with the PDF
  local f = io.open((os.getenv("TEXMF_OUTPUT_DIRECTORY") or ".") .. "/e10-oracle.json", "w")
  f:write("[")
  local first = true
  for _, line in ipairs(page.lines) do
    for _, it in ipairs(line.items) do
      if it[1] == "g" and it[9] and E10.oracle[it[9]] then
        local o = E10.oracle[it[9]]
        f:write(string.format('%s{"c":%d,"x":%d,"y":%d,"ef":%d,"line":%s}', first and "" or ",", it[3], o.x, o.y, it[8], tostring(line.i or 0)))
        first = false
      end
    end
  end
  f:write("]")
  f:close()
end
