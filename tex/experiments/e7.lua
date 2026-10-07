-- E7: versions, string.pack, node.direct getters, timer resolution, node field lists.
local d = node.direct
local have = {}
for _, k in ipairs{"getid","getnext","getlist","getwidth","getfield","getfont","getchar","getattribute",
                   "getglue","getshift","getkern","getexpansion","getoffsets","getwhd","getdisc","getsubtype",
                   "effective_glue","getboth","getdata","getwidth","getheight","getdepth","getleader"} do
  have[#have + 1] = k .. "=" .. tostring(d[k] ~= nil)
end
local t0 = os.gettimeofday(); local t1 = t0; local n = 0
while t1 == t0 do t1 = os.gettimeofday(); n = n + 1 end
local function sorted_kv(t)
  local r = {}
  for k, v in pairs(t) do r[#r + 1] = tostring(v) .. "=" .. tostring(k) end
  table.sort(r); return r
end
local w = texio.write_nl
w("term and log", string.format("E7 banner=%s luatex_version=%s lua=%s pack=%s hb=%s",
  status.banner, tostring(status.luatex_version), _VERSION, tostring(string.pack ~= nil), tostring(luaharfbuzz ~= nil)))
w("term and log", "E7 node.direct: " .. table.concat(have, " "))
w("term and log", string.format("E7 gettimeofday tick=%.3f us (%d spins)", (t1 - t0) * 1e6, n))
w("term and log", "E7 node types: " .. table.concat(sorted_kv(node.types()), " "))
for _, t in ipairs{"glyph", "hlist", "vlist", "glue", "kern", "math", "margin_kern", "disc", "rule", "local_par", "dir", "penalty", "boundary"} do
  local ok, f = pcall(node.fields, t)
  w("term and log", "E7 " .. t .. " fields: " .. (ok and table.concat(f, " ") or tostring(f)))
end
for _, st in ipairs{"pdf_colorstack", "pdf_literal", "special", "user_defined", "pdf_refobj", "pdf_setmatrix", "pdf_save", "pdf_restore", "late_lua"} do
  local ok, f = pcall(node.fields, "whatsit", st)
  w("term and log", "E7 whatsit." .. st .. " fields: " .. (ok and table.concat(f, " ") or tostring(f)))
end
w("term and log", "E7 whatsit subtypes: " .. table.concat(sorted_kv(node.whatsits()), " "))
w("term and log", "E7 glue subtypes: " .. table.concat(sorted_kv(node.subtypes("glue")), " "))
w("term and log", "E7 kern subtypes: " .. table.concat(sorted_kv(node.subtypes("kern")), " "))
w("term and log", "E7 glyph subtypes: " .. table.concat(sorted_kv(node.subtypes("glyph")), " "))
w("term and log", "E7 hlist subtypes: " .. table.concat(sorted_kv(node.subtypes("hlist")), " "))
w("term and log", "E7 math subtypes: " .. table.concat(sorted_kv(node.subtypes("math")), " "))
local fc = font.getfont(font.current())
local keys = {}
for k, v in pairs(fc) do if type(v) ~= "table" then keys[#keys + 1] = k .. "=" .. tostring(v) end end
table.sort(keys)
w("term and log", "E7 current font scalar fields: " .. table.concat(keys, " "))
local ch = fc.characters and (fc.characters[string.byte("A")] or fc.characters[65])
if ch then
  local ck = {}
  for k, v in pairs(ch) do ck[#ck + 1] = k .. "=" .. tostring(v) end
  table.sort(ck)
  w("term and log", "E7 char A fields: " .. table.concat(ck, " "))
end
