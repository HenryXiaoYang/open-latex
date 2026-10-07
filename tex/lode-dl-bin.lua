-- lode-dl-bin.lua: binary display-list writer (encoding revision 1, docs/DISPLAY_LIST.md).
-- Input: the table produced by lode-dl.lua. Output: a Lua string.
local M = {}
local pack, concat = string.pack, table.concat

local function str(s)
  s = tostring(s or "")
  if #s > 65535 then s = s:sub(1, 65535) end
  return pack("<s2", s)
end

local function font_kind(d)
  local fmt, typ = d.format, d.type
  if typ == "virtual" then return 5 end
  if fmt == "opentype" then return 1 elseif fmt == "truetype" then return 2
  elseif fmt == "type1" then return 3 elseif fmt == "type3" then return 4 end
  return 0
end

local function rec(buf, tag, payload)
  buf[#buf + 1] = pack("<Bs4", tag, payload)
end

-- Write items (a line's items or the `other` list) into buf, grouping glyph runs.
local function write_items(buf, items)
  local run_font, run_y, run_ef, run = nil, nil, nil, nil
  local function flush()
    if run then
      local parts = { pack("<I4i4i4I4", run_font, run_y, run_ef, #run) }
      for i = 1, #run do parts[#parts + 1] = run[i] end
      rec(buf, 0x20, concat(parts))
      run, run_font = nil, nil
    end
  end
  for i = 1, #items do
    local it = items[i]
    local t = it[1]
    if t == "g" then
      local f, y, ef = it[2], it[6], it[8] or 0
      if not run or f ~= run_font or y ~= run_y or ef ~= run_ef then
        flush()
        run, run_font, run_y, run_ef = {}, f, y, ef
      end
      run[#run + 1] = pack("<I4I4i4i4", it[3] or 0, it[4] or 0xFFFFFFFF, it[5], it[7])
    else
      flush()
      if t == "r" then rec(buf, 0x21, pack("<i4i4i4i4", it[2], it[3], it[4], it[5]))
      elseif t == "c" then
        local cmd = it[3]; if type(cmd) ~= "number" then cmd = 255 end
        rec(buf, 0x22, pack("<BBI2", cmd, 0, it[2] or 0) .. str(it[4]))
      elseif t == "l" then rec(buf, 0x23, pack("<i4", it[2] or 0) .. str(it[3]))
      elseif t == "u" then rec(buf, 0x24, str(it[2]) .. str(type(it[3]) == "table" and "" or it[3]))
      elseif t == "m" then rec(buf, 0x25, pack("<Bi4", it[2] == "on" and 1 or 0, it[3]))
      elseif t == "i" then rec(buf, 0x26, pack("<i4i4i4i4i4", it[2] or 0, it[3], it[4], it[5], it[6]))
      end
    end
  end
  flush()
end

function M.encode(dl)
  local buf = {}
  local is_page = dl.kind == "page"
  local origin = dl.origin or { 0, 0 }
  rec(buf, 0x01, pack("<i4i4i4i4i4i4i4i4I4I4", dl.width or 0, dl.height or 0, dl.depth or 0, dl.page or 0,
    dl.page_width or 0, dl.page_height or 0, origin[1] or 0, origin[2] or 0, dl.glyphs or 0, dl.images or 0))
  local ids = {}
  for id in pairs(dl.fonts or {}) do ids[#ids + 1] = tonumber(id) or id end
  table.sort(ids, function(a, b) return tostring(a) < tostring(b) end)
  for _, id in ipairs(ids) do
    local d = dl.fonts[id] or dl.fonts[tostring(id)]
    rec(buf, 0x02, pack("<I4i4BBI2i4i4i4i4", d.id or tonumber(id) or 0, math.floor((d.size or 0) + 0.5), font_kind(d), 0,
      d.subfont or 0, math.floor((d.slant or 0) + 0.5), math.floor((d.extend or 0) + 0.5), math.floor((d.squeeze or 0) + 0.5),
      math.floor((d.designsize or 0) + 0.5)) .. str(d.filename) .. str(d.psname) .. str(d.name) .. str(d.fullname) .. str(d.format))
  end
  if dl.flags then
    for k, v in pairs(dl.flags) do
      rec(buf, 0x03, str(k) .. pack("<i4", type(v) == "number" and math.floor(v) or 1))
    end
  end
  write_items(buf, dl.other or {})
  for _, line in ipairs(dl.lines or {}) do
    rec(buf, 0x10, pack("<i4i4i4i4i4i4i4dBB", line.par or 0, line.i or 0, line.x, line.y, line.w, line.h, line.d,
      line.gs or 0.0, line.gsign or 0, line.gorder or 0))
    write_items(buf, line.items)
    rec(buf, 0x11, "")
  end
  rec(buf, 0xFF, "")
  local body = concat(buf)
  local header = pack("<c4I2I2I4I4", "LODL", 1, is_page and 1 or 0, 16 + #body, 0)
  return header .. body
end

return M
