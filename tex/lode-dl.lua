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
local pack, concat = string.pack, table.concat

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
local RS = node.subtypes("rule")
local RULE_IMAGE, RULE_EMPTY, RULE_USER, RULE_OUTLINE = 2, 3, 4, 9
for k, v in pairs(RS) do
  if v == "image" then RULE_IMAGE = k elseif v == "empty" then RULE_EMPTY = k
  elseif v == "user" then RULE_USER = k elseif v == "outline" then RULE_OUTLINE = k end
end

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
    e = { tfm = tfm, chars = tfm and tfm.characters or {}, widths = {},
          virtual = tfm and tfm.type == "virtual" or false,
          fonts = tfm and tfm.fonts or nil }
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
    glyphs = 0, images = 0,
  }, State)
end

local function bstr(s)
  s = tostring(s or "")
  if #s > 65535 then s = s:sub(1, 65535) end
  return pack("<s2", s)
end

-- Binary sink (docs/DISPLAY_LIST.md): records are appended to self.buf as they are produced;
-- consecutive glyphs with the same font/baseline/expansion are coalesced into one GLYPHS run.
function State:bin_flush_run()
  local run = self.run
  if run then
    self.buf[#self.buf + 1] = pack("<Bs4", 0x20, pack("<I4i4i4I4", self.run_font, self.run_y, self.run_ef, #run) .. concat(run))
    self.run = nil
  end
end
function State:bin_rec(tag, payload)
  self:bin_flush_run()
  self.buf[#self.buf + 1] = pack("<Bs4", tag, payload)
end

function State:emit(item)
  if self.bin then
    local t = item[1]
    if t == "g" then
      local f, y, ef = item[2], item[6], item[8] or 0
      if not self.run or f ~= self.run_font or y ~= self.run_y or ef ~= self.run_ef then
        self:bin_flush_run()
        self.run, self.run_font, self.run_y, self.run_ef = {}, f, y, ef
      end
      local run = self.run
      run[#run + 1] = pack("<I4I4i4i4", item[3] or 0, item[4] or 0xFFFFFFFF, item[5], item[7])
    elseif t == "r" then self:bin_rec(0x21, pack("<i4i4i4i4", item[2], item[3], item[4], item[5]))
    elseif t == "c" then
      local cmd = item[3]; if type(cmd) ~= "number" then cmd = 255 end
      self:bin_rec(0x22, pack("<BBI2", cmd, 0, item[2] or 0) .. bstr(item[4]))
    elseif t == "l" then self:bin_rec(0x23, pack("<i4", item[2] or 0) .. bstr(item[3]))
    elseif t == "u" then self:bin_rec(0x24, bstr(item[2]) .. bstr(type(item[3]) == "table" and "" or item[3]))
    elseif t == "m" then self:bin_rec(0x25, pack("<Bi4", item[2] == "on" and 1 or 0, item[3]))
    elseif t == "i" then self:bin_rec(0x26, pack("<i4i4i4i4i4", item[2] or 0, item[3], item[4], item[5], item[6]))
    end
    return
  end
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
  local last_f, last_e = nil, nil
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
        if ef ~= 0 then w = expand(w, ef) end
        if f ~= last_f then
          last_e = font_entry(f)
          last_f = f
          if not st.fonts[f] then st:usefont(f) end
        end
        local e = last_e
        if e.virtual then
          st:emit_virtual(f, c, cur_h + (xo or 0), base_v - (yo or 0), ef)
        elseif st.bin then
          -- hot path: pack straight into the current glyph run
          st.glyphs = st.glyphs + 1
          local y = base_v - yo
          local run = st.run
          if not run or f ~= st.run_font or y ~= st.run_y or ef ~= st.run_ef then
            st:bin_flush_run()
            run = {}
            st.run, st.run_font, st.run_y, st.run_ef = run, f, y, ef
          end
          local ch = e.chars[c]
          run[#run + 1] = pack("<I4I4i4i4", c, (ch and ch.index) or 0xFFFFFFFF, cur_h + xo, w)
        else
          local gi = glyph_index(f, c)
          st.glyphs = st.glyphs + 1
          local item = { "g", f, c, gi, cur_h + (xo or 0), base_v - (yo or 0), w, ef }
          if st.glyph_attr then item[9] = getattribute(n, st.glyph_attr) end
          st:emit(item)
        end
        cur_h = cur_h + w
      elseif id == glue_id then
        local wd, st_, sh, sto, sho = getglue(n)
        local rule_wd = glue_advance(wd, st_, sh, sto, sho)
        local sub = getsubtype(n)
        if sub >= 100 then
          -- leaders (a_leaders=100, c_leaders=101, x_leaders=102, g_leaders=103)
          local lb = getleader(n)
          if lb and getid(lb) == rule_id then
            local lw, lh, ld = getwhd(lb)
            if lh == RUNNING then lh = box_h end
            if ld == RUNNING then ld = box_d end
            if rule_wd > 0 and (lh + ld) > 0 then st:emit({ "r", cur_h, base_v - lh, rule_wd, lh + ld }) end
          elseif lb then
            local leader_wd = getwidth(lb)
            if leader_wd > 0 and rule_wd > 0 then
              rule_wd = rule_wd + 10
              local edge = cur_h + rule_wd
              local lx = 0
              local save_h = cur_h
              if sub == 100 then
                -- aligned leaders: boxes at multiples of leader_wd from the enclosing box's left edge
                cur_h = left + leader_wd * ((cur_h - left) // leader_wd)
                if cur_h < save_h then cur_h = cur_h + leader_wd end
              else
                local lq = rule_wd // leader_wd
                local lr = rule_wd % leader_wd
                if sub == 101 then
                  cur_h = cur_h + lr // 2
                else
                  lx = lr // (lq + 1)
                  cur_h = cur_h + (lr - (lq - 1) * lx) // 2
                end
              end
              local lid = getid(lb)
              local lw, lh, ld = getwhd(lb)
              local lshift = getshift(lb)
              while cur_h + leader_wd <= edge do
                if lid == hlist_id then
                  hlist_out(st, lb, cur_h, base_v + lshift)
                else
                  vlist_out(st, lb, cur_h, base_v + lshift - lh)
                end
                cur_h = cur_h + leader_wd + lx
              end
              cur_h = edge - 10
              rule_wd = 0
              cur_h = save_h + (edge - 10 - save_h)
              -- the loop above already advanced cur_h to edge - 10; nothing left to add
              goto continue
            end
          end
        end
        cur_h = cur_h + rule_wd
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
        local sub = getsubtype(n)
        if h == RUNNING then h = box_h end
        if d == RUNNING then d = box_d end
        if w == RUNNING then w = 0 end
        if sub == RULE_IMAGE then
          st:emit({ "i", getfield(n, "index"), cur_h, base_v - h, w, h + d }); st.images = st.images + 1
        elseif sub == RULE_EMPTY then
          -- \nullfont / empty rule: occupies space, draws nothing
        elseif sub == RULE_USER or sub == RULE_OUTLINE then
          st:emit({ "u", "rule_subtype", sub }); st:flag("rule_subtype", sub)
        elseif w > 0 and (h + d) > 0 then
          st:emit({ "r", cur_h, base_v - h, w, h + d })
        end
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
      ::continue::
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
      local sub = getsubtype(n)
      if w == RUNNING then w = box_w end
      if h == RUNNING then h = 0 end
      if d == RUNNING then d = 0 end
      if sub == RULE_IMAGE then
        st:emit({ "i", getfield(n, "index"), left, cur_v, w, h + d }); st.images = st.images + 1
      elseif sub == RULE_EMPTY then
      elseif sub == RULE_USER or sub == RULE_OUTLINE then
        st:emit({ "u", "rule_subtype", sub }); st:flag("rule_subtype", sub)
      elseif w > 0 and (h + d) > 0 then
        st:emit({ "r", left, cur_v, w, h + d })
      end
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
      if sub >= 100 then
        local lb = getleader(n)
        if lb and getid(lb) == rule_id then
          local lw = getwidth(lb)
          if lw == RUNNING then lw = box_w end
          if lw > 0 and rule_ht > 0 then st:emit({ "r", left, cur_v, lw, rule_ht }) end
        elseif lb then
          local lw, lh, ld = getwhd(lb)
          local leader_ht = lh + ld
          if leader_ht > 0 and rule_ht > 0 then
            rule_ht = rule_ht + 10
            local edge = cur_v + rule_ht
            local lx = 0
            local save_v = cur_v
            if sub == 100 then
              cur_v = top + leader_ht * ((cur_v - top) // leader_ht)
              if cur_v < save_v then cur_v = cur_v + leader_ht end
            else
              local lq = rule_ht // leader_ht
              local lr = rule_ht % leader_ht
              if sub == 101 then cur_v = cur_v + lr // 2
              else lx = lr // (lq + 1); cur_v = cur_v + (lr - (lq - 1) * lx) // 2 end
            end
            local lshift = getshift(lb)
            while cur_v + leader_ht <= edge do
              cur_v = cur_v + lh
              if getid(lb) == hlist_id then hlist_out(st, lb, left + lshift, cur_v)
              else vlist_out(st, lb, left + lshift, cur_v - lh) end
              cur_v = cur_v + ld + lx
            end
            cur_v = edge - 10
            goto vcontinue
          end
        end
      end
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
    ::vcontinue::
    n = getnext(n)
  end
  return cur_v
end

-- Expand a virtual-font character into real-font glyphs/rules (LuaTeX font `commands`).
-- Nested virtual fonts are expanded recursively (depth-limited).
function State:emit_virtual(f, c, x, y, ef, depth)
  depth = depth or 0
  local e = font_entry(f)
  local ch = e.chars[c]
  local cmds = ch and ch.commands
  if not cmds or depth > 4 then
    self:flag("virtual_unexpanded"); self:emit({ "u", "virtual", f }); return
  end
  local fonts = e.fonts or {}
  local cur_font = fonts[1] and fonts[1].id or f
  local px, py = x, y
  local stack = {}
  for _, cmd in ipairs(cmds) do
    local op = cmd[1]
    if op == "font" then
      local fe = fonts[cmd[2]]
      cur_font = fe and fe.id or cur_font
    elseif op == "char" or op == "slot" then
      local cc = op == "char" and cmd[2] or cmd[3]
      if op == "slot" then local fe = fonts[cmd[2]]; cur_font = fe and fe.id or cur_font end
      local fe2 = font_entry(cur_font)
      local w = (fe2.chars[cc] and fe2.chars[cc].width) or 0
      w = floor(w + 0.5)
      if ef ~= 0 then w = expand(w, ef) end
      if fe2.virtual and cur_font ~= f then
        self:emit_virtual(cur_font, cc, px, py, ef, depth + 1)
      else
        self:usefont(cur_font)
        self.glyphs = self.glyphs + 1
        self:emit({ "g", cur_font, cc, glyph_index(cur_font, cc), px, py, w, ef })
      end
      px = px + w
    elseif op == "right" then px = px + floor(cmd[2] + 0.5)
    elseif op == "down" then py = py + floor(cmd[2] + 0.5)
    elseif op == "push" then stack[#stack + 1] = { px, py }
    elseif op == "pop" then local t = stack[#stack]; if t then px, py = t[1], t[2]; stack[#stack] = nil end
    elseif op == "rule" then
      local h, w = floor(cmd[2] + 0.5), floor(cmd[3] + 0.5)
      if w > 0 and h > 0 then self:emit({ "r", px, py - h, w, h }) end
      px = px + w
    elseif op == "special" or op == "pdf" or op == "lua" or op == "image" or op == "node" then
      self:flag("virtual_" .. op); self:emit({ "u", "virtual_" .. op, cur_font })
    end
  end
end

function State:begin_line(n, par, line, x, baseline, w, h, d)
  local gs = getfield(n, "glue_set")
  if self.bin then
    self.nlines = self.nlines + 1
    self.cur = true
    self:bin_rec(0x10, pack("<i4i4i4i4i4i4i4dBB", par or 0, line or 0, x, baseline, w, h, d, gs or 0.0,
      getfield(n, "glue_sign") or 0, getfield(n, "glue_order") or 0))
    return
  end
  self.cur = { par = par, i = line, x = x, y = baseline, w = w, h = h, d = d,
               gs = gs, gsign = getfield(n, "glue_sign"), gorder = getfield(n, "glue_order"), items = {} }
end
function State:end_line()
  if self.bin then
    self:bin_rec(0x11, "")
    self.cur = nil
    return
  end
  self.lines[#self.lines + 1] = self.cur
  self.cur = nil
end

local function result(st, kind, extra)
  local fonts = {}
  for id, d in pairs(st.fonts) do fonts[tostring(id)] = d end
  local r = { kind = kind, unit = "sp", fonts = fonts, lines = st.lines, other = st.other,
              flags = st.flags, glyphs = st.glyphs, images = st.images }
  if extra then for k, v in pairs(extra) do r[k] = v end end
  return r
end

local function font_kind(d)
  if d.type == "virtual" then return 5 end
  local fmt = d.format
  if fmt == "opentype" then return 1 elseif fmt == "truetype" then return 2
  elseif fmt == "type1" then return 3 elseif fmt == "type3" then return 4 end
  return 0
end

-- Paragraph box straight to binary display-list bytes (format v1). Same traversal as
-- M.paragraph; records are written as they are produced, META/FONT/FLAG records last.
-- Returns bytes, line count, glyph count, width, height, depth, flags table.
function M.paragraph_binary(boxnode, initial_color)
  local box = todirect(boxnode)
  local st = new_state({ lines_at_top = true })
  st.depth = 0
  st.bin = true
  st.buf = {}
  st.nlines = 0
  if initial_color and initial_color ~= "" then st:emit({ "c", 0, 0, initial_color }) end
  local w, h, d = getwhd(box)
  vlist_out(st, box, 0, 0)
  st:bin_flush_run()
  local buf = st.buf
  buf[#buf + 1] = pack("<Bs4", 0x01, pack("<i4i4i4i4i4i4i4i4I4I4", w, h, d, 0, 0, 0, 0, 0, st.glyphs, st.images))
  local ids = {}
  for id in pairs(st.fonts) do ids[#ids + 1] = id end
  table.sort(ids)
  for _, id in ipairs(ids) do
    local fd = st.fonts[id]
    buf[#buf + 1] = pack("<Bs4", 0x02, pack("<I4i4BBI2i4i4i4i4", fd.id or id, floor((fd.size or 0) + 0.5), font_kind(fd), 0,
      fd.subfont or 0, floor((fd.slant or 0) + 0.5), floor((fd.extend or 0) + 0.5), floor((fd.squeeze or 0) + 0.5),
      floor((fd.designsize or 0) + 0.5)) .. bstr(fd.filename) .. bstr(fd.psname) .. bstr(fd.name) .. bstr(fd.fullname) .. bstr(fd.format))
  end
  for k, v in pairs(st.flags) do
    buf[#buf + 1] = pack("<Bs4", 0x03, bstr(k) .. pack("<i4", type(v) == "number" and floor(v) or 1))
  end
  buf[#buf + 1] = pack("<Bs4", 0xFF, "")
  local body = concat(buf)
  return pack("<c4I2I2I4I4", "LODL", 1, 0, 16 + #body, 0) .. body, st.nlines, st.glyphs, w, h, d, st.flags
end

-- Paragraph box (a \vbox whose top-level hlists are the lines). Origin: top-left of the box.
-- `initial_color` (raw PDF color operators) is the color in force when the paragraph starts.
function M.paragraph(boxnode, initial_color)
  local box = todirect(boxnode)
  local st = new_state({ lines_at_top = true })
  st.depth = 0
  if initial_color and initial_color ~= "" then st.other[1] = { "c", 0, 0, initial_color } end
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
