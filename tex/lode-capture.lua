-- lode-capture.lua: Lua side of lode-capture.sty.
local C = { seq = 0, stack = {}, begins = {}, paras = {}, pages = {}, page = 0, placements = {} }
local json = dofile(kpse.find_file("lode-json.lua", "lua") or "lode-json.lua")
local dl = dofile(kpse.find_file("lode-dl.lua", "lua") or "lode-dl.lua")

local INT_PARAMS = { "looseness", "tolerance", "pretolerance", "hyphenpenalty", "exhyphenpenalty",
  "adjdemerits", "doublehyphendemerits", "finalhyphendemerits", "linepenalty", "lastlinefit",
  "language", "lefthyphenmin", "righthyphenmin", "uchyph", "interlinepenalty", "clubpenalty",
  "widowpenalty", "adjustspacing", "protrudechars", "hangafter" }
local DIM_PARAMS = { "hsize", "parindent", "emergencystretch", "lineskiplimit", "hangindent", "mathsurround" }
local GLUE_PARAMS = { "leftskip", "rightskip", "parfillskip", "baselineskip", "lineskip", "spaceskip", "xspaceskip" }
C.INT_PARAMS, C.DIM_PARAMS, C.GLUE_PARAMS = INT_PARAMS, DIM_PARAMS, GLUE_PARAMS

local function macro(name)
  local ok, v = pcall(token.get_macro, name)
  if ok then return v end
  return nil
end

function C.setup(opts)
  C.jobname = opts.jobname
  C.attr_par = luatexbase.new_attribute("lode_par")
  C.attr_line = luatexbase.new_attribute("lode_line")
  luatexbase.add_to_callback("pre_linebreak_filter", C.pre_linebreak, "lode-capture")
  luatexbase.add_to_callback("post_linebreak_filter", C.post_linebreak, "lode-capture")
end

-- para/begin hook: start of a paragraph (horizontal mode just entered).
function C.parbegin()
  local nest = tex.nest.ptr
  local b = C.begins
  -- drop stale entries from deeper nesting levels that never reached a line break
  while #b > 0 and b[#b].nest > nest do b[#b] = nil end
  b[#b + 1] = {
    line = tex.inputlineno, nest = nest, file = status.filename,
    font = font.current(),
    nfss = { enc = macro("f@encoding"), family = macro("f@family"), series = macro("f@series"),
             shape = macro("f@shape"), size = macro("f@size"), baselineskip = macro("f@baselineskip") },
    color = macro("current@color"),
    mathversion = macro("math@version"),
  }
end

local function scan_flags(head)
  local flags, glyphs = {}, 0
  for n in node.traverse(head) do
    local t = n.id
    if t == node.id("glyph") then glyphs = glyphs + 1
    elseif t == node.id("whatsit") then
      local st = node.whatsits()[n.subtype] or tostring(n.subtype)
      flags["whatsit_" .. st] = (flags["whatsit_" .. st] or 0) + 1
    elseif t == node.id("ins") or t == node.id("mark") or t == node.id("adjust") or t == node.id("dir")
        or t == node.id("hlist") or t == node.id("vlist") or t == node.id("rule") or t == node.id("math") then
      local name = node.type(t)
      flags[name] = (flags[name] or 0) + 1
    end
  end
  flags.glyphs = glyphs
  return flags
end

function C.pre_linebreak(head, groupcode)
  C.seq = C.seq + 1
  local seq = C.seq
  local nest = tex.nest.ptr
  local p = { seq = seq, groupcode = groupcode, nest = nest, end_line = tex.inputlineno, file = status.filename }
  local ints, dims, glues = {}, {}, {}
  for _, k in ipairs(INT_PARAMS) do ints[k] = tex.get(k) end
  for _, k in ipairs(DIM_PARAMS) do dims[k] = tex.get(k) end
  for _, k in ipairs(GLUE_PARAMS) do glues[k] = { tex.getglue(k) } end
  p.ints, p.dims, p.glues = ints, dims, glues
  p.parshape = tex.parshape
  p.everypar = tex.gettoks("everypar")
  p.flags = scan_flags(head)
  -- pair with the most recent para/begin at the same nesting level
  local b = C.begins
  local top = b[#b]
  if top and top.nest == nest then
    p.begin = top
    b[#b] = nil
  end
  C.paras[seq] = p
  C.stack[#C.stack + 1] = seq
  return true
end

function C.post_linebreak(head, groupcode)
  local seq = C.stack[#C.stack]
  C.stack[#C.stack] = nil
  local p = C.paras[seq]
  local i = 0
  local hl = node.id("hlist")
  for n in node.traverse(head) do
    if n.id == hl then
      i = i + 1
      node.set_attribute(n, C.attr_par, seq)
      node.set_attribute(n, C.attr_line, i)
    end
  end
  if p then p.lines = i end
  return true
end

function C.shipout(boxnum)
  C.page = C.page + 1
  local b = tex.box[boxnum]
  if not b then return end
  local page = dl.page(b, C.attr_par, C.attr_line, C.page)
  C.pages[#C.pages + 1] = page
  for _, line in ipairs(page.lines) do
    local pl = C.placements[line.par]
    if not pl then pl = {}; C.placements[line.par] = pl end
    pl[#pl + 1] = { page = C.page, i = line.i, x = line.x, y = line.y, w = line.w, h = line.h, d = line.d }
  end
end

local function out_path(name)
  local dir = os.getenv("LODE_CAPTURE_DIR") or os.getenv("TEXMF_OUTPUT_DIRECTORY") or "."
  return dir .. "/" .. name
end

function C.finish()
  local paras = {}
  for seq = 1, C.seq do
    local p = C.paras[seq]
    if p then
      p.placements = C.placements[seq]
      paras[#paras + 1] = p
    end
  end
  local out = {
    version = 1, jobname = C.jobname, pages = C.page,
    engine = status.banner, luatex_version = status.luatex_version,
    paragraphs = paras,
  }
  local f = assert(io.open(out_path(C.jobname .. ".lode.json"), "w"))
  f:write(json.encode(out))
  f:close()
  for i, page in ipairs(C.pages) do
    local pf = assert(io.open(out_path(string.format("%s.lode-page%d.json", C.jobname, i)), "w"))
    pf:write(json.encode(page))
    pf:close()
  end
  texio.write_nl("term and log", string.format("lode-capture: %d paragraphs, %d pages written", #paras, C.page))
end

return C
