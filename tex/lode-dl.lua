-- lode-dl.lua: node-list traversal that replicates LuaTeX's hlist_out/vlist_out position
-- arithmetic and emits a display list. Shared by the fast server (paragraph boxes) and by
-- the capture package (shipped pages). Coordinates in scaled points, y grows downward.
--
-- Item encodings (compact arrays):
--   {"g", font_id, char, glyph_index, x, y_baseline, width, expansion_factor}
--   {"r", x, y_top, width, height}                      rule
--   {"c", stack, cmd, data}                             pdf_colorstack whatsit
--   {"l", mode, data}                                   pdf_literal whatsit (host passthrough)
--   {"u", kind, detail}                                 unsupported node (page degraded)
--   {"m", "on"|"off", x}                                math boundary marker
local M = {}

local node = node
local D = node.direct
local todirect = D.todirect
local getid, getnext, getlist, getfield, getsubtype = D.getid, D.getnext, D.getlist, D.getfield, D.getsubtype
local getwhd, getwidth = D.getwhd, D.getwidth
local getfont, getchar, getkern, getshift = D.getfont, D.getchar, D.getkern, D.getshift
local getglue, getexpansion, getoffsets = D.getglue, D.getexpansion, D.getoffsets
local getattribute = D.getattribute or D.get_attribute
local getdisc, getdata = D.getdisc, D.getdata
local getleader = D.getleader
local floor, ceil = math.floor, math.ceil

local T = {}
for id, name in pairs(node.types()) do T[name] = id end
local glyph_id, glue_id, kern_id, margin_kern_id = T.glyph, T.glue, T.kern, T.margin_kern
local hlist_id, vlist_id, rule_id, disc_id, math_id = T.hlist, T.vlist, T.rule, T.disc, T.math
local whatsit_id, penalty_id, local_par_id, dir_id = T.whatsit, T.penalty, T.local_par, T.dir
local ins_id, mark_id, adjust_id, boundary_id = T.ins, T.mark, T.adjust, T.boundary
local W = {}
for st, name in pairs(node.whatsits()) do W[name] = st end
local ws_colorstack, ws_literal, ws_special = W.pdf_colorstack, W.pdf_literal, W.special
local ws_late_lua, ws_user = W.late_lua, W.user_defined
local ws_setmatrix, ws_save, ws_restore = W.pdf_setmatrix, W.pdf_save, W.pdf_restore
local ws_open, ws_write, ws_close, ws_savepos = W.open, W.write, W.close, W.save_pos
local function silent_whatsit(sub)
  return sub == ws_user or sub == ws_late_lua or sub == ws_open or sub == ws_write or sub == ws_close or sub == ws_savepos
end

local RUNNING = -1073741824  -- null_flag: running dimension for rules

-- TeX's round(): web2c zround.
local function tex_round(r)
  if r >= 0 then return floor(r + 0.5) else return ceil(r - 0.5) end
end
local BILLION = 1000000000.0
local function vet_glue(g)
  if g > BILLION then return BILLION elseif g < -BILLION then return -BILLION else return g end
end
-- TeX's round_xn_over_d (tex.web §107 variant used by pdfTeX/LuaTeX for expansion).
local function round_xn_over_d(x, n, d)
  local positive = x >= 0
  if not positive then x = -x end
  local t = (x % 32768) * n
  local u = (x // 32768) * n + (t // 32768)
  local v = (u % d) * 32768 + (t % 32768)
  u = 32768 * (u // d) + (v // d)
  v = v % d
  if 2 * v >= d then u = u + 1 end
  if positive then return u else return -u end
end
M.round_xn_over_d = round_xn_over_d
M.tex_round = tex_round

-- Font metrics cache: width per (font, char) and glyph index.
local font_cache = {}
local function font_entry(f)
  local e = font_cache[f]
  if not e then
    local tfm = font.getfont(f)
    e = { tfm = tfm, chars = tfm and tfm.characters or {}, widths = {} }
    font_cache[f] = e
  end
  return e
end
-- Glyph index for the renderer; the advance width comes from the engine (node.direct.getwidth
-- on the glyph node), which is the integer TeX uses, not the possibly fractional font table value.
local function glyph_index(f, c)
  local e = font_entry(f)
  local gi = e.widths[c]
  if gi == nil then
    local ch = e.chars[c]
    gi = ch and ch.index or false
    e.widths[c] = gi
  end
  return gi or nil
end

-- Expansion: LuaTeX's glyph/kern expansion_factor (verified against PDF output in M1/E6).
local EXP_DENOM = 1000000
local function expand(w, ef)
  if ef == 0 then return w end
  return round_xn_over_d(w, EXP_DENOM + ef, EXP_DENOM)
end
M.set_expansion_denominator = function(d) EXP_DENOM = d end

-- Font descriptors for the "fonts" table of a display list.
local function font_descriptor(f)
  local t = font.getfont(f)
  if not t then return { id = f } end
  return {
    id = f, name = t.name, fullname = t.fullname, psname = t.psname, filename = t.filename,
    format = t.format, type = t.type, size = t.size, designsize = t.designsize,
    slant = t.slant, extend = t.extend, squeeze = t.squeeze, subfont = t.subfont,
    encodingbytes = t.encodingbytes, embedding = t.embedding,
  }
end

---------------------------------------------------------------------------------------------
-- Traversal state. One state per traversal; items are appended to the current line or to
-- "other" (page material outside tagged lines).
---------------------------------------------------------------------------------------------
local State = {}
State.__index = State

local function new_state(opts)
  return setmetatable({
    opts = opts or {}, fonts = {}, lines = {}, other = {}, cur = nil, flags = {},
    attr_par = opts and opts.attr_par, attr_line = opts and opts.attr_line,
    glyph_attr = opts and opts.glyph_attr,
    glyphs = 0,
  }, State)
end

function State:emit(item)
  local tgt = self.cur and self.cur.items or self.other
  tgt[#tgt + 1] = item
end
function State:flag(kind, detail)
  local f = self.flags
  f[kind] = (f[kind] or 0) + 1
  if detail and not f[kind .. "_detail"] then f[kind .. "_detail"] = detail end
end
function State:usefont(f)
  if not self.fonts[f] then self.fonts[f] = font_descriptor(f) end
end

local hlist_out, vlist_out

-- Walk an hlist's content list. Returns final cur_h. `walk` is re-entered for disc replace lists.
hlist_out = function(st, box, left, base_v)
  local g_order, g_sign, g_set = getfield(box, "glue_order"), getfield(box, "glue_sign"), getfield(box, "glue_set")
  local box_w, box_h, box_d = getwhd(box)
  local cur_h = left
  local cur_glue, cur_g = 0.0, 0

  local function glue_advance(wd, st_, sh, sto, sho)
    local rule_wd = wd - cur_g
    if g_sign ~= 0 then
      if g_sign == 1 then
        if sto == g_order then
          cur_glue = cur_glue + st_
          cur_g = tex_round(vet_glue(g_set * cur_glue))
        end
      elseif sho == g_order then
        cur_glue = cur_glue - sh
        cur_g = tex_round(vet_glue(g_set * cur_glue))
      end
    end
    return rule_wd + cur_g
  end

  local function walk(list)
    local n = list
    while n do
      local id = getid(n)
      if id == glyph_id then
        local f, c = getfont(n), getchar(n)
        local ef = getexpansion(n) or 0
        local xo, yo = getoffsets(n)
        local w = floor(getwidth(n) + 0.5)
        local gi = glyph_index(f, c)
        if ef ~= 0 then w = expand(w, ef) end
        st:usefont(f)
        st.glyphs = st.glyphs + 1
        local item = { "g", f, c, gi, cur_h + (xo or 0), base_v - (yo or 0), w, ef }
        if st.glyph_attr then item[9] = getattribute(n, st.glyph_attr) end
        st:emit(item)
        cur_h = cur_h + w
      elseif id == glue_id then
        local wd, st_, sh, sto, sho = getglue(n)
        local adv = glue_advance(wd, st_, sh, sto, sho)
        local sub = getsubtype(n)
        if sub >= 100 then st:flag("leaders", sub); st:emit({ "u", "leaders", cur_h }) end
        cur_h = cur_h + adv
      elseif id == kern_id then
        -- LuaTeX: kern_width(q) = width(q) + ex_kern(q); for kern nodes the Lua field
        -- `expansion_factor` *is* ex_kern, an amount in sp precomputed by the packer.
        cur_h = cur_h + getkern(n) + (getexpansion(n) or 0)
      elseif id == margin_kern_id then
        cur_h = cur_h + getwidth(n)
      elseif id == hlist_id or id == vlist_id then
        local w, h, d = getwhd(n)
        local s = getshift(n)
        local par = st.attr_par and getattribute(n, st.attr_par)
        local line = par and st.attr_line and getattribute(n, st.attr_line)
        if par and par >= 0 and id == hlist_id and not st.cur then
          st:begin_line(n, par, line, cur_h, base_v + s, w, h, d)
          hlist_out(st, n, cur_h, base_v + s)
          st:end_line()
        elseif id == hlist_id then
          hlist_out(st, n, cur_h, base_v + s)
        else
          vlist_out(st, n, cur_h, base_v + s - h)
        end
        cur_h = cur_h + w
      elseif id == rule_id then
        local w, h, d = getwhd(n)
        if h == RUNNING then h = box_h end
        if d == RUNNING then d = box_d end
        if w == RUNNING then w = 0 end
        if w > 0 and (h + d) > 0 then st:emit({ "r", cur_h, base_v - h, w, h + d }) end
        cur_h = cur_h + w
      elseif id == disc_id then
        local pre, post, replace = getdisc(n)
        if replace then walk(replace) end
      elseif id == math_id then
        local wd, st_, sh, sto, sho = getglue(n)
        local sub = getsubtype(n)
        st:emit({ "m", sub == 0 and "on" or "off", cur_h })
        if wd == 0 and st_ == 0 and sh == 0 then
          cur_h = cur_h + getfield(n, "surround")
        else
          cur_h = cur_h + glue_advance(wd, st_, sh, sto, sho)
        end
      elseif id == whatsit_id then
        local sub = getsubtype(n)
        if sub == ws_colorstack then
          st:emit({ "c", getfield(n, "stack"), getfield(n, "cmd"), getfield(n, "data") })
        elseif sub == ws_literal then
          st:emit({ "l", getfield(n, "mode"), getfield(n, "data") }); st:flag("literal")
        elseif sub == ws_special then
          st:emit({ "l", -1, getfield(n, "data") }); st:flag("special")
        elseif silent_whatsit(sub) then
          -- bookkeeping whatsits (\write, luaotfload, hyperref) produce no output
        elseif sub == ws_setmatrix or sub == ws_save or sub == ws_restore then
          st:emit({ "u", "pdf_matrix", cur_h }); st:flag("pdf_matrix")
        else
          st:emit({ "u", "whatsit", sub }); st:flag("whatsit", sub)
        end
      elseif id == penalty_id or id == boundary_id or id == mark_id then
        -- no output
      elseif id == local_par_id then
        local bl = getfield(n, "box_left_width") or 0
        local br = getfield(n, "box_right_width") or 0
        if bl ~= 0 or br ~= 0 then st:flag("local_boxes") end
      elseif id == dir_id then
        local dir = getfield(n, "dir")
        if dir and dir ~= "TLT" and dir ~= "+TLT" and dir ~= "-TLT" then st:flag("dir", dir); st:emit({ "u", "dir", dir }) end
      elseif id == ins_id then
        st:flag("ins"); st:emit({ "u", "ins", cur_h })
      elseif id == adjust_id then
        st:flag("adjust"); st:emit({ "u", "adjust", cur_h })
      else
        st:flag("node_" .. (node.type(id) or tostring(id)))
      end
      n = getnext(n)
    end
  end
  walk(getlist(box))
  return cur_h
end

vlist_out = function(st, box, left, top)
  local g_order, g_sign, g_set = getfield(box, "glue_order"), getfield(box, "glue_sign"), getfield(box, "glue_set")
  local box_w = getwidth(box)
  local cur_v = top
  local cur_glue, cur_g = 0.0, 0
  local n = getlist(box)
  while n do
    local id = getid(n)
    if id == hlist_id or id == vlist_id then
      local w, h, d = getwhd(n)
      local s = getshift(n)
      cur_v = cur_v + h
      local par = st.attr_par and getattribute(n, st.attr_par)
      local line = par and st.attr_line and getattribute(n, st.attr_line)
      if id == hlist_id then
        if par and par >= 0 and not st.cur then
          st:begin_line(n, par, line, left + s, cur_v, w, h, d)
          hlist_out(st, n, left + s, cur_v)
          st:end_line()
        elseif st.opts.lines_at_top and not st.cur and st.depth == 0 then
          -- paragraph mode: every top-level hlist of the vbox is a line
          st:begin_line(n, 0, #st.lines + 1, left + s, cur_v, w, h, d)
          st.depth = 1
          hlist_out(st, n, left + s, cur_v)
          st.depth = 0
          st:end_line()
        else
          hlist_out(st, n, left + s, cur_v)
        end
      else
        vlist_out(st, n, left + s, cur_v - h)
      end
      cur_v = cur_v + d
    elseif id == rule_id then
      local w, h, d = getwhd(n)
      if w == RUNNING then w = box_w end
      if h == RUNNING then h = 0 end
      if d == RUNNING then d = 0 end
      if w > 0 and (h + d) > 0 then st:emit({ "r", left, cur_v, w, h + d }) end
      cur_v = cur_v + h + d
    elseif id == glue_id then
      local wd, st_, sh, sto, sho = getglue(n)
      local rule_ht = wd - cur_g
      if g_sign ~= 0 then
        if g_sign == 1 then
          if sto == g_order then cur_glue = cur_glue + st_; cur_g = tex_round(vet_glue(g_set * cur_glue)) end
        elseif sho == g_order then
          cur_glue = cur_glue - sh; cur_g = tex_round(vet_glue(g_set * cur_glue))
        end
      end
      rule_ht = rule_ht + cur_g
      local sub = getsubtype(n)
      if sub >= 100 then st:flag("leaders", sub) end
      cur_v = cur_v + rule_ht
    elseif id == kern_id then
      cur_v = cur_v + getkern(n)
    elseif id == whatsit_id then
      local sub = getsubtype(n)
      if sub == ws_colorstack then
        st:emit({ "c", getfield(n, "stack"), getfield(n, "cmd"), getfield(n, "data") })
      elseif sub == ws_literal then
        st:emit({ "l", getfield(n, "mode"), getfield(n, "data") }); st:flag("literal")
      elseif sub == ws_special then
        st:emit({ "l", -1, getfield(n, "data") }); st:flag("special")
      elseif silent_whatsit(sub) then
      else
        st:emit({ "u", "whatsit", sub }); st:flag("whatsit", sub)
      end
    elseif id == penalty_id or id == mark_id then
    elseif id == ins_id then
      st:flag("ins")
    else
      st:flag("node_" .. (node.type(id) or tostring(id)))
    end
    n = getnext(n)
  end
  return cur_v
end

function State:begin_line(n, par, line, x, baseline, w, h, d)
  local gs = getfield(n, "glue_set")
  self.cur = { par = par, i = line, x = x, y = baseline, w = w, h = h, d = d,
               gs = gs, gsign = getfield(n, "glue_sign"), gorder = getfield(n, "glue_order"), items = {} }
end
function State:end_line()
  self.lines[#self.lines + 1] = self.cur
  self.cur = nil
end

local function result(st, kind, extra)
  local fonts = {}
  for id, d in pairs(st.fonts) do fonts[tostring(id)] = d end
  local r = { kind = kind, unit = "sp", fonts = fonts, lines = st.lines, other = st.other,
              flags = st.flags, glyphs = st.glyphs }
  if extra then for k, v in pairs(extra) do r[k] = v end end
  return r
end

-- Paragraph box (a \vbox whose top-level hlists are the lines). Origin: top-left of the box.
function M.paragraph(boxnode)
  local box = todirect(boxnode)
  local st = new_state({ lines_at_top = true })
  st.depth = 0
  local w, h, d = getwhd(box)
  vlist_out(st, box, 0, 0)
  return result(st, "paragraph", { width = w, height = h, depth = d })
end

-- Shipped page box. `attr_par`/`attr_line` identify tagged lines. Origin: page top-left;
-- the box is offset by (1in + \hoffset, 1in + \voffset) like the PDF backend does.
function M.page(boxnode, attr_par, attr_line, page_no, glyph_attr)
  local box = todirect(boxnode)
  local st = new_state({ attr_par = attr_par, attr_line = attr_line, glyph_attr = glyph_attr })
  local one_inch = 4736286  -- 72.27pt in sp
  local ox = one_inch + tex.hoffset
  local oy = one_inch + tex.voffset
  local w, h, d = getwhd(box)
  vlist_out(st, box, ox, oy)
  return result(st, "page", { page = page_no, width = w, height = h, depth = d,
    page_width = tex.pagewidth, page_height = tex.pageheight, origin = { ox, oy } })
end

return M
