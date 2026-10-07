-- texlua tex/tests/run.lua : unit tests for the Lua side that need no TeX run.
package.path = "tex/?.lua;" .. package.path
local failures = 0
local function check(name, cond, detail)
  if cond then print("ok   " .. name) else failures = failures + 1; print("FAIL " .. name .. (detail and (": " .. tostring(detail)) or "")) end
end

-- JSON round trip
local json = dofile("tex/lode-json.lua")
local v = { a = 1, b = { 1, 2, 3 }, c = "x\"y\\z\n", d = true, e = 2.5, f = {} }
local s = json.encode(v)
local back = json.decode(s)
check("json roundtrip scalars", back.a == 1 and back.c == v.c and back.d == true and back.e == 2.5)
check("json roundtrip arrays", #back.b == 3 and back.b[3] == 3)
check("json decode unicode escape", json.decode('"\\u00e9\\ud83d\\ude00"') == "é😀")
check("json decode nested", json.decode('{"x":[{"y":null},{"y":-3e2}]}').x[2].y == -300)

-- lode-dl arithmetic (stub the node library enough to load the module)
node = node or {}
node.types = node.types or function() return { [0] = "hlist", [1] = "vlist", [2] = "rule", [29] = "glyph", [12] = "glue", [13] = "kern", [28] = "margin_kern", [7] = "disc", [11] = "math", [8] = "whatsit", [14] = "penalty", [9] = "local_par", [10] = "dir", [3] = "ins", [4] = "mark", [5] = "adjust", [6] = "boundary" } end
node.whatsits = node.whatsits or function() return { [0] = "open", [1] = "write", [2] = "close", [8] = "late_lua", [22] = "pdf_literal", [28] = "pdf_colorstack", [3] = "special", [6] = "save_pos", [7] = "user_defined", [29] = "pdf_setmatrix", [30] = "pdf_save", [31] = "pdf_restore" } end
node.subtypes = node.subtypes or function() return { [0] = "normal", [1] = "box", [2] = "image", [3] = "empty", [4] = "user", [9] = "outline" } end
node.direct = node.direct or setmetatable({}, { __index = function() return function() end end })
node.type = node.type or function(id) return node.types()[id] end
font = font or { getfont = function() return nil end }
tex = tex or { hoffset = 0, voffset = 0, pagewidth = 0, pageheight = 0 }
local dl = dofile("tex/lode-dl.lua")
check("tex_round positive", dl.tex_round(2.5) == 3 and dl.tex_round(2.4999) == 2)
check("tex_round negative", dl.tex_round(-2.5) == -3 and dl.tex_round(-2.4999) == -2)
-- round_xn_over_d against exact rational rounding
local function ref(x, n, d) local q = x * n / d; return q >= 0 and math.floor(q + 0.5) or -math.floor(-q + 0.5) end
local okc = true
for _, c in ipairs{ {431102, 1020000, 1000000}, {431102, 980000, 1000000}, {705419, 983000, 1000000}, {123456789, 1001000, 1000000}, {-358810, 1017000, 1000000}, {1, 1, 3}, {2, 1, 4} } do
  local a, b = dl.round_xn_over_d(c[1], c[2], c[3]), ref(c[1], c[2], c[3])
  if a ~= b then okc = false; print("  mismatch", c[1], c[2], c[3], a, b) end
end
check("round_xn_over_d matches exact rounding", okc)
-- expansion formula: ef in millionths, 2% stretch of 431102 sp
check("expansion 2%", dl.round_xn_over_d(431102, 1020000, 1000000) == 439724)

if failures > 0 then print(failures .. " failure(s)"); os.exit(1) else print("all Lua tests passed") end
