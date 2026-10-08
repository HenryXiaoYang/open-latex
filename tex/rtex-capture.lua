-- rtex-capture.lua: Lua side of rtex-capture.sty.
--
-- Units: the regions the fast server can re-typeset in isolation. A unit is a top-level
-- paragraph (kind "par"; it may contain display math, footnotes and a trailing block
-- environment), a block environment started in vertical mode (kind "env": lists, quote,
-- center, floats, theorem-like environments) or a sectioning command (kind "heading"). Every
-- node created while a unit is open carries the `rtex_unit` attribute, so at shipout the unit's
-- rows (hlists reached through vlists only) are found wherever the page builder put them.
local C = { seq = 0, stack = {}, begins = {}, paras = {}, pages = {}, page = 0, units = {}, uid = 0, cur = nil, env_depth = 0 }
local json = dofile(kpse.find_file("rtex-json.lua", "lua") or "rtex-json.lua")
local dl = dofile(kpse.find_file("rtex-dl.lua", "lua") or "rtex-dl.lua")

local INT_PARAMS = { "looseness", "tolerance", "pretolerance", "hyphenpenalty", "exhyphenpenalty",
  "adjdemerits", "doublehyphendemerits", "finalhyphendemerits", "linepenalty", "lastlinefit",
  "language", "lefthyphenmin", "righthyphenmin", "uchyph", "interlinepenalty", "clubpenalty",
  "widowpenalty", "adjustspacing", "protrudechars", "hangafter" }
local DIM_PARAMS = { "hsize", "parindent", "emergencystretch", "lineskiplimit", "hangindent", "mathsurround" }
local GLUE_PARAMS = { "leftskip", "rightskip", "parfillskip", "baselineskip", "lineskip", "spaceskip", "xspaceskip" }
C.INT_PARAMS, C.DIM_PARAMS, C.GLUE_PARAMS = INT_PARAMS, DIM_PARAMS, GLUE_PARAMS

-- Block environments that open a unit when begun in vertical mode. Extra names (theorem-like
-- environments) come from $RTEX_UNIT_ENVS.
C.BLOCK_ENVS = { "itemize", "enumerate", "description", "quote", "quotation", "verse", "center",
  "flushleft", "flushright", "figure", "figure*", "table", "table*", "abstract", "tabbing",
  "proof", "verbatim", "verbatim*", "lstlisting", "Verbatim", "alltt", "spacing", "thebibliography",
  "tikzpicture", "circuitikz", "pgfpicture" }
C.HEADINGS = { "part", "chapter", "section", "subsection", "subsubsection", "paragraph", "subparagraph" }
local TWO_PAR_HEADINGS = { part = true, chapter = true }
local UNSET = -0x7FFFFFFF

local function macro(name)
  local ok, v = pcall(token.get_macro, name)
  if ok then return v end
  return nil
end
local IFTRUE_MODE = token.create("iftrue").mode
local function iftrue(name)
  local ok, t = pcall(token.create, name)
  return ok and t and t.mode == IFTRUE_MODE or false
end

-- LaTeX counters (from \cl@@ckpt, the \include checkpoint list every \newcounter extends).
local counter_names = {}
local function read_counters()
  local v = {}
  for i = 1, #counter_names do
    local n = counter_names[i]
    local ok, c = pcall(tex.getcount, "c@" .. n)
    if ok then v[n] = c end
  end
  return v
end
local prev_counters = {}
-- delta against the previous unit's snapshot (units are written in document order)
local function counter_delta()
  local now = read_counters()
  local d = {}
  for k, v in pairs(now) do if prev_counters[k] ~= v then d[k] = v end end
  prev_counters = now
  return d
end
-- Macros the document body (re)defines (\renewcommand{\arraystretch}{1.6} before a table,
-- \def\H{4} before a picture): their meaning at each unit is captured like the counters and
-- replayed by the server, so a setting changed mid-document reaches the fast path at the right
-- place. The names come from scanning the document's source files for definitions.
local tracked_macros = { "arraystretch", "baselinestretch" }
local prev_macros = {}
local function meaning(name)
  if token.get_meaning then
    local ok, m = pcall(token.get_meaning, name)
    if ok and m then return m end
  end
  local ok, b = pcall(token.get_macro, name)
  return ok and b and ("->" .. b) or nil
end
local function scan_definitions(path, seen, depth)
  if depth > 6 then return end
  local f = io.open(path, "r")
  if not f then return end
  local text = f:read("*a")
  f:close()
  -- strip comments (unescaped % to end of line)
  text = text:gsub("\\%%", "\0"):gsub("%%[^\n]*", ""):gsub("%z", "\\%%")
  for name in text:gmatch("\\re?newcommand%*?%s*{?\\(%a+)") do seen[name] = true end
  for name in text:gmatch("\\providecommand%*?%s*{?\\(%a+)") do seen[name] = true end
  for name in text:gmatch("\\[gex]?def%s*\\(%a+)") do seen[name] = true end
  for name in text:gmatch("\\let%s*\\(%a+)") do seen[name] = true end
  for name in text:gmatch("\\setlength%s*\\(%a+)") do seen[name] = true end
  for sub in text:gmatch("\\input%s*{([^}]*)}") do
    local sp = sub:match("%.tex$") and sub or (sub .. ".tex")
    scan_definitions(sp, seen, depth + 1)
  end
  for sub in text:gmatch("\\include%s*{([^}]*)}") do
    scan_definitions(sub .. ".tex", seen, depth + 1)
  end
end
local function macro_delta()
  local d = {}
  for i = 1, #tracked_macros do
    local n = tracked_macros[i]
    local m = meaning(n)
    if m and prev_macros[n] ~= m then d[n] = m; prev_macros[n] = m end
  end
  return d
end
-- \the<counter> formats (\thesection …): \appendix, \renewcommand{\thesection}{…} and
-- \pagenumbering change them mid-document; the server replays them per unit like the counters.
local prev_thefmt = {}
local function thefmt_delta()
  local d = {}
  for i = 1, #counter_names do
    local n = counter_names[i]
    local ok, body = pcall(token.get_macro, "the" .. n)
    if ok and body and prev_thefmt[n] ~= body then d[n] = body; prev_thefmt[n] = body end
  end
  return d
end

local function params()
  local ints, dims, glues = {}, {}, {}
  for _, k in ipairs(INT_PARAMS) do ints[k] = tex.get(k) end
  for _, k in ipairs(DIM_PARAMS) do dims[k] = tex.get(k) end
  for _, k in ipairs(GLUE_PARAMS) do glues[k] = { tex.getglue(k) } end
  return ints, dims, glues
end
local function nfss()
  return { enc = macro("f@encoding"), family = macro("f@family"), series = macro("f@series"),
           shape = macro("f@shape"), size = macro("f@size"), baselineskip = macro("f@baselineskip") }
end

function C.setup(opts)
  C.jobname = opts.jobname
  C.attr_par = luatexbase.new_attribute("rtex_par")
  C.attr_line = luatexbase.new_attribute("rtex_line")
  C.attr_unit = luatexbase.new_attribute("rtex_unit")
  luatexbase.add_to_callback("pre_linebreak_filter", C.pre_linebreak, "rtex-capture")
  luatexbase.add_to_callback("post_linebreak_filter", C.post_linebreak, "rtex-capture")
  local extra = os.getenv("RTEX_UNIT_ENVS") or ""
  for name in extra:gmatch("[^,%s]+") do C.BLOCK_ENVS[#C.BLOCK_ENVS + 1] = name end
end

-- Font and color state between top-level units. A paragraph that starts inside a group
-- ({\em word} ..., {\bfseries ...}) fires para/begin with that group's font selected; the
-- state the fast server must replay is the one outside the group, i.e. the state left behind by
-- the previous unit (or \begin{document}).
local function snapshot_outer()
  return { level = tex.currentgrouplevel, nfss = nfss(), color = macro("current@color"), font = font.current() }
end
C.outer = nil

-- Called at \begin{document} (counters are all defined by then).
function C.begin_document()
  local ck = macro("cl@@ckpt") or ""
  for n in ck:gmatch("\\@elt%s*{([^}]*)}") do counter_names[#counter_names + 1] = n end
  prev_counters = {}
  prev_thefmt = {}
  prev_macros = {}
  local seen = {}
  scan_definitions(tex.jobname .. ".tex", seen, 0)
  for name in pairs(seen) do
    local known = false
    for i = 1, #tracked_macros do if tracked_macros[i] == name then known = true end end
    -- only macros (not counters, lengths or primitives): \c@…, \the…, \if… are handled elsewhere
    if not known and not name:match("^the") and not name:match("^if") then tracked_macros[#tracked_macros + 1] = name end
  end
  C.outer = snapshot_outer()
end

-- Units ------------------------------------------------------------------------------------------
local function unit_open(kind, name, set_attr)
  C.uid = C.uid + 1
  local ints, dims, glues = params()
  local u = { uid = C.uid, kind = kind, name = name, file = status.filename, begin_line = tex.inputlineno,
              nest = tex.nest.ptr, seqs = {}, placements = {}, counters = counter_delta(), thefmt = thefmt_delta(), macros = macro_delta(),
              everypar = tex.gettoks("everypar"), nobreak = iftrue("if@nobreak"),
              afterindent = iftrue("if@afterindent"), noskipsec = iftrue("if@noskipsec"),
              nfss = nfss(), color = macro("current@color"),
              ints = ints, dims = dims, glues = glues, parshape = tex.parshape,
              attr_set = set_attr }
  C.units[#C.units + 1] = u
  C.cur = u
  -- Global: a paragraph often starts inside a group ({\em word} ..., \begingroup, tabularx's
  -- final typesetting) that closes before its lines are built; a local assignment would be
  -- restored by then and the lines would carry no unit. unit_close resets it.
  if set_attr then tex.setattribute("global", C.attr_unit, C.uid) end
  return u
end
local function unit_close()
  local u = C.cur
  if not u then return end
  u.end_line = tex.inputlineno
  -- counters the unit advanced (values at its end that differ from its start): the session
  -- compares them with what a fast compile of the unit advances
  local now = read_counters()
  local adv = {}
  for k, v in pairs(now) do if prev_counters[k] ~= v and k ~= "page" then adv[k] = v end end
  if next(adv) then u.advanced = adv end
  tex.setattribute("global", C.attr_unit, UNSET)
  C.cur = nil
  if C.outer and tex.currentgrouplevel == C.outer.level then C.outer = snapshot_outer() end
end

-- para/begin hook: start of a paragraph (horizontal mode just entered).
function C.parbegin()
  local nest = tex.nest.ptr
  local ep = tex.gettoks("everypar")
  local cur = C.cur
  -- a top-level paragraph starting inside a group: font and color from outside the group
  local outer = (nest == 1 and C.outer and tex.currentgrouplevel > C.outer.level) and C.outer or nil
  if nest == 1 and not C.in_output then
    if not cur then
      cur = unit_open("par", nil, true)
      if outer then cur.nfss = outer.nfss; cur.color = outer.color end
    end
  end
  local b = C.begins
  -- drop stale entries from deeper nesting levels that never reached a line break
  while #b > 0 and b[#b].nest > nest do b[#b] = nil end
  b[#b + 1] = {
    line = tex.inputlineno, nest = nest, file = status.filename,
    font = outer and outer.font or font.current(),
    nfss = outer and outer.nfss or nfss(),
    color = outer and outer.color or macro("current@color"),
    mathversion = macro("math@version"),
    nobreak = iftrue("if@nobreak"),
    unit = cur and cur.uid or nil,
  }
end

-- para/after hook: back in vertical mode after \par.
function C.parafter()
  local cur = C.cur
  if not cur or tex.nest.ptr ~= 0 or C.env_depth > 0 then return end
  if cur.kind == "par" or (cur.kind == "heading" and not TWO_PAR_HEADINGS[cur.name]) then unit_close() end
  -- a heading command that ended this paragraph (no blank line before \section): its unit
  -- opens now, after the paragraph's last lines were built and attributed
  local pending = C.pending_heading
  if pending and not C.cur then
    C.pending_heading = nil
    unit_open("heading", pending, true)
  end
end

-- env/<block>/begin and /after hooks (inside and after the environment group).
function C.envbegin(name)
  local cur = C.cur
  if cur and cur.kind == "heading" and tex.nest.ptr == 0 then unit_close(); cur = nil end
  if not cur and tex.nest.ptr == 0 then
    unit_open("env", name, true)
  end
  C.env_depth = C.env_depth + 1
end
function C.envafter(name)
  C.env_depth = C.env_depth - 1
  if C.env_depth < 0 then C.env_depth = 0 end
  local cur = C.cur
  if C.env_depth == 0 and cur and (cur.kind == "env" or cur.kind == "par") and tex.nest.ptr == 0 then unit_close() end
end

-- cmd/@outputpage/before hook: header and footer boxes are built inside the output routine's
-- group while a unit may still be open; without the unit attribute they are not unit rows.
function C.output_begin()
  C.in_output = true
  tex.setattribute("global", C.attr_unit, UNSET)
end
-- cmd/@outputpage/after hook: back to the open unit (the attribute is global).
function C.output_end()
  C.in_output = false
  local cur = C.cur
  tex.setattribute("global", C.attr_unit, cur and cur.uid or UNSET)
end

-- cmd/<heading>/before hook.
function C.heading(name)
  local cur = C.cur
  -- a heading an environment produces (thebibliography's \section*{\refname}) is part of
  -- that unit
  if cur and cur.kind == "env" and C.env_depth > 0 then return end
  -- a heading right after a paragraph's last line (no blank line): the paragraph is still
  -- open and \@startsection's own \par will end it. Closing the unit now would leave the
  -- paragraph's last lines without a unit, so the heading unit opens in parafter instead.
  if cur and cur.kind == "par" and tex.nest.ptr > 0 then
    C.pending_heading = name
    return
  end
  if cur then unit_close() end
  unit_open("heading", name, true)
end

-- cmd/@afterheading/before hook: the kernel runs \@afterheading once the heading's own
-- paragraphs are typeset (\chapter and \part have two), so the heading unit ends here.
function C.afterheading()
  local cur = C.cur
  if cur and cur.kind == "heading" then unit_close() end
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
  p.ints, p.dims, p.glues = params()
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
  local cur = C.cur
  -- paragraphs built inside the output routine (fancyhdr's running head and foot are
  -- paragraphs in boxes) belong to the page, not to the unit that happened to be open
  if cur and not C.in_output then
    p.unit = cur.uid
    cur.seqs[#cur.seqs + 1] = seq
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
  local unit = p and p.unit
  for n in node.traverse(head) do
    if n.id == hl then
      i = i + 1
      node.set_attribute(n, C.attr_par, seq)
      node.set_attribute(n, C.attr_line, i)
      -- the line boxes are built here, possibly after the group the paragraph started in
      if unit then node.set_attribute(n, C.attr_unit, unit) end
    end
  end
  if p then p.lines = i end
  return true
end

C.images = {}
function C.image(index, file, page, pages)
  C.images[tostring(index)] = { index = index, file = file, page = tonumber(page) or 1, pages = pages }
end

-- Rows of a paragraph unit that come from a deeper paragraph (nest >= 2) reached through
-- vlists only are migrated material: footnote text (\insert), \vadjust, \marginpar. Inside
-- environment units (floats, minipages) deeper paragraphs are the unit's own content.
local function is_insert(seq, unit)
  local p = C.paras[seq]
  if not p then return false end
  if p.groupcode == "insert" or p.groupcode == "adjust" then return true end
  local u = unit and C.units[unit]
  if u and (u.kind == "par" or u.kind == "heading") and p.nest >= 2 then return true end
  return false
end

function C.shipout(boxnum)
  C.page = C.page + 1
  local b = tex.box[boxnum]
  if not b then return end
  local page = dl.page(b, C.attr_par, C.attr_line, C.page, nil, C.attr_unit, is_insert)
  if next(C.images) then page.images_info = C.images end
  C.pages[#C.pages + 1] = page
  for _, line in ipairs(page.lines) do
    local u = line.unit and C.units[line.unit]
    if u then
      local pl = u.placements
      local row = #pl + 1
      line.row = row
      pl[row] = { page = C.page, row = row, x = line.x, y = line.y, w = line.w, h = line.h, d = line.d,
                  par = line.par, line = line.i }
    end
  end
end

local function out_path(name)
  local dir = os.getenv("RTEX_CAPTURE_DIR") or os.getenv("TEXMF_OUTPUT_DIRECTORY") or "."
  return dir .. "/" .. name
end

function C.finish()
  if C.cur then unit_close() end
  local paras = {}
  for seq = 1, C.seq do
    local p = C.paras[seq]
    if p then paras[#paras + 1] = p end
  end
  for _, u in ipairs(C.units) do
    u.rows = #u.placements
    u.attr_set = nil
  end
  local out = {
    version = 2, jobname = C.jobname, pages = C.page, images = C.images,
    engine = status.banner, luatex_version = status.luatex_version,
    counters = counter_names,
    paragraphs = paras, units = C.units,
  }
  local f = assert(io.open(out_path(C.jobname .. ".rtex.json"), "w"))
  f:write(json.encode(out))
  f:close()
  for i, page in ipairs(C.pages) do
    local pf = assert(io.open(out_path(string.format("%s.rtex-page%d.json", C.jobname, i)), "w"))
    pf:write(json.encode(page))
    pf:close()
  end
  texio.write_nl("term and log", string.format("rtex-capture: %d paragraphs, %d units, %d pages written", #paras, #C.units, C.page))
end

return C
