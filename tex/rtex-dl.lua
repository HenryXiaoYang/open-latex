-- rtex-dl.lua: node-list traversal that replicates LuaTeX's hlist_out/vlist_out position
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
--   {"i", index, x, y_top, width, height}               image (engine resource index)
--   {"M", "save"|"set"|"restore", x, y, data}           pdf_save / pdf_setmatrix / pdf_restore:
--                                                       `set` transforms what follows by the
--                                                       matrix "a b c d" about the point (x, y)
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
-- hyperref's link/destination/annotation whatsits carry no ink (link borders are PDF viewer
-- decorations); the engine still typesets everything around them identically
local SILENT = { [ws_user] = true, [ws_late_lua] = true, [ws_open] = true, [ws_write] = true, [ws_close] = true, [ws_savepos] = true }
for _, name in ipairs({ "pdf_dest", "pdf_annot", "pdf_start_link", "pdf_end_link", "pdf_refobj", "pdf_thread", "pdf_start_thread", "pdf_end_thread", "pdf_action", "pdf_link_data" }) do
  if W[name] then SILENT[W[name]] = true end
end
local function silent_whatsit(sub)
  return SILENT[sub] == true
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
    e = { tfm = tfm, chars = tfm and tfm.characters or {}, widths = {}, idx = {}, ew = {},
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

-- Font descriptors for the "fonts" table of a display list. Cached per font id: font ids are
-- never reused within a process, and font.getfont() builds a fresh (large) table for TFM
-- fonts on every call.
local descriptor_cache = {}
local function font_descriptor(f)
  local d = descriptor_cache[f]
  if d then return d end
  local t = font_entry(f).tfm
  if not t then
    d = { id = f }
  else
    d = {
      id = f, name = t.name, fullname = t.fullname, psname = t.psname, filename = t.filename,
      format = t.format, type = t.type, size = t.size, designsize = t.designsize,
      slant = t.slant, extend = t.extend, squeeze = t.squeeze, subfont = t.subfont,
      encodingbytes = t.encodingbytes, embedding = t.embedding,
    }
  end
  descriptor_cache[f] = d
  return d
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
    attr_unit = opts and opts.attr_unit, is_insert = opts and opts.is_insert,
    glyph_attr = opts and opts.glyph_attr,
    -- picture cache: boxes tagged with attr_pic are reported with their page position
    -- (outermost box per id); images in pic_images are cached pictures (page degraded for
    -- hosts, which then use the PDF)
    attr_pic = opts and opts.attr_pic, pic_images = (opts and opts.pic_images) or M.pic_images, pics = {}, pic_seen = {}, pic_in = 0,
    glyphs = 0, images = 0, inserts = 0,
  }, State)
end
-- Record a box carrying the picture attribute: the picture's output is the union of its
-- outermost tagged boxes (page coordinates; one baseline, else `multi`). Every node made
-- inside the picture carries the attribute (set while the environment runs), so boxes inside
-- a recorded box are skipped (`st.pic_in`), as is the paragraph indent box the picture's
-- \leavevmode made. Returns true when the caller is entering a recorded box.
local HLIST_INDENT = 3
local function note_pic(st, n, x, baseline, w, h, d)
  if not st.attr_pic or st.pic_in > 0 then return false end
  local id = getattribute(n, st.attr_pic)
  if not id or id < 0 then return false end
  if getid(n) == hlist_id and getsubtype(n) == HLIST_INDENT then return false end
  local r = st.pic_seen[id]
  if not r then
    r = { id = id, x = x, y = baseline, right = x + w, top = baseline - h, bottom = baseline + d, multi = false }
    st.pic_seen[id] = r
    st.pics[#st.pics + 1] = r
  else
    if baseline ~= r.y then r.multi = true end
    if x < r.x then r.x = x end
    if x + w > r.right then r.right = x + w end
    if baseline - h < r.top then r.top = baseline - h end
    if baseline + d > r.bottom then r.bottom = baseline + d end
  end
  r.w = r.right - r.x
  r.h = r.y - r.top
  r.d = r.bottom - r.y
  return true
end

local function bstr(s)
  s = tostring(s or "")
  if #s > 65535 then s = s:sub(1, 65535) end
  return pack("<s2", s)
end

-- Binary sink (docs/display-list.md): records are appended to self.buf as they are produced;
-- consecutive glyphs with the same font/baseline/expansion are coalesced into one GLYPHS run.
-- A glyph run is a flat integer array (char, glyph index, x, width per glyph) packed once at
-- flush time with a cached repeated format: cheaper than one string.pack per glyph (E22).
local RUN_FMT = setmetatable({}, { __index = function(t, k) local f = "<" .. ("I4I4i4i4"):rep(k); t[k] = f; return f end })
local RUN_CHUNK = 256  -- glyphs per pack call (bounds the format string and the unpack size)
local unpack = table.unpack
function State:bin_flush_run()
  local run = self.run
  if run then
    local k = self.rn
    local n = k // 4
    self.glyphs = self.glyphs + n
    local head = pack("<I4i4i4I4", self.run_font, self.run_y, self.run_ef, n)
    local body
    if n <= RUN_CHUNK then
      body = pack(RUN_FMT[n], unpack(run, 1, k))
    else
      local parts, np = {}, 0
      for i = 1, k, RUN_CHUNK * 4 do
        local j = i + RUN_CHUNK * 4 - 1
        if j > k then j = k end
        np = np + 1
        parts[np] = pack(RUN_FMT[(j - i + 1) // 4], unpack(run, i, j))
      end
      body = concat(parts, "", 1, np)
    end
    self.buf[#self.buf + 1] = pack("<Bs4", 0x20, head .. body)
    self.run = nil
    self.rn = 0
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
        self.run, self.rn, self.run_font, self.run_y, self.run_ef = {}, 0, f, y, ef
      end
      local n = self.rn
      local run = self.run
      run[n + 1], run[n + 2], run[n + 3], run[n + 4] = item[3] or 0, item[4] or 0xFFFFFFFF, item[5], item[7]
      self.rn = n + 4
      -- (binary mode counts glyphs at flush time)
    elseif t == "r" then self:bin_rec(0x21, pack("<i4i4i4i4", item[2], item[3], item[4], item[5]))
    elseif t == "c" then
      local cmd = item[3]; if type(cmd) ~= "number" then cmd = 255 end
      self:bin_rec(0x22, pack("<BBI2", cmd, 0, item[2] or 0) .. bstr(item[4]))
    elseif t == "l" then self:bin_rec(0x23, pack("<i4", item[2] or 0) .. bstr(item[3]))
    elseif t == "u" then self:bin_rec(0x24, bstr(item[2]) .. bstr(type(item[3]) == "table" and "" or item[3]))
    elseif t == "m" then self:bin_rec(0x25, pack("<Bi4", item[2] == "on" and 1 or 0, item[3]))
    elseif t == "i" then self:bin_rec(0x26, pack("<i4i4i4i4i4", item[2] or 0, item[3], item[4], item[5], item[6]))
    elseif t == "M" then
      local op = item[2] == "save" and 0 or (item[2] == "set" and 1 or 2)
      self:bin_rec(0x28, pack("<Bi4i4", op, item[3], item[4]) .. bstr(item[5]))
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
-- The current glyph run (binary mode) is mirrored in locals while walking; `sync_out`/`sync_in`
-- move it to/from the state around calls that may emit records or recurse.
hlist_out = function(st, box, left, base_v)
  local last_f, last_e = nil, nil
  local ew, ew_f, ew_ef = nil, nil, nil  -- advance-width cache for the current (font, expansion)
  local bin = st.bin
  local g_order, g_sign, g_set = getfield(box, "glue_order"), getfield(box, "glue_sign"), getfield(box, "glue_set")
  local box_w, box_h, box_d = getwhd(box)
  local cur_h = left
  local cur_glue, cur_g = 0.0, 0
  local run, rn, run_font, run_y, run_ef = st.run, st.rn, st.run_font, st.run_y, st.run_ef

  local function sync_out() st.run, st.rn, st.run_font, st.run_y, st.run_ef = run, rn, run_font, run_y, run_ef end
  local function sync_in() run, rn, run_font, run_y, run_ef = st.run, st.rn, st.run_font, st.run_y, st.run_ef end

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

  -- discretionary replace lists are walked in place: the node after the disc is pushed on a
  -- small stack and resumed when the replace list ends
  local dstack, ddepth = nil, 0
  local n = getlist(box)
  while n do
      local id = getid(n)
      local nxt = getnext(n)
      if id == glyph_id then
        local f, c = getfont(n), getchar(n)
        local ef = getexpansion(n) or 0
        local xo, yo = getoffsets(n)
        if f ~= last_f then
          last_e = font_cache[f] or font_entry(f)
          last_f = f
          if not st.fonts[f] then st:usefont(f) end
        end
        local e = last_e
        -- advance width: the engine's integer char width, expanded by ef; the same (font,
        -- char, ef) always yields the same value, so it is computed once per font entry
        if f ~= ew_f or ef ~= ew_ef then
          ew = e.ew[ef]
          if not ew then ew = {}; e.ew[ef] = ew end
          ew_f, ew_ef = f, ef
        end
        local w = ew[c]
        if w == nil then
          w = getwidth(n)
          if ef ~= 0 then w = expand(w, ef) end
          ew[c] = w
        end
        if e.virtual then
          sync_out(); st:emit_virtual(f, c, cur_h + (xo or 0), base_v - (yo or 0), ef); sync_in()
        elseif bin then
          -- hot path: append to the current glyph run (flat integer array)
          local y = base_v - yo
          if not run or f ~= run_font or y ~= run_y or ef ~= run_ef then
            sync_out(); st:bin_flush_run()
            run, rn, run_font, run_y, run_ef = {}, 0, f, y, ef
          end
          local gi = e.idx[c]
          if gi == nil then
            local ch = e.chars[c]
            gi = (ch and ch.index) or 0xFFFFFFFF
            e.idx[c] = gi
          end
          run[rn + 1], run[rn + 2], run[rn + 3], run[rn + 4] = c, gi, cur_h + xo, w
          rn = rn + 4
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
        -- inlined glue_advance (hot: every inter-word space)
        local rule_wd = wd - cur_g
        if g_sign ~= 0 then
          if g_sign == 1 then
            if sto == g_order then
              cur_glue = cur_glue + st_
              local g = g_set * cur_glue
              if g > BILLION then g = BILLION elseif g < -BILLION then g = -BILLION end
              if g >= 0 then cur_g = floor(g + 0.5) else cur_g = ceil(g - 0.5) end
            end
          elseif sho == g_order then
            cur_glue = cur_glue - sh
            local g = g_set * cur_glue
            if g > BILLION then g = BILLION elseif g < -BILLION then g = -BILLION end
            if g >= 0 then cur_g = floor(g + 0.5) else cur_g = ceil(g - 0.5) end
          end
        end
        rule_wd = rule_wd + cur_g
        local sub = getsubtype(n)
        if sub >= 100 then
          -- leaders (a_leaders=100, c_leaders=101, x_leaders=102, g_leaders=103)
          local lb = getleader(n)
          if lb and getid(lb) == rule_id then
            local lw, lh, ld = getwhd(lb)
            if lh == RUNNING then lh = box_h end
            if ld == RUNNING then ld = box_d end
            if rule_wd > 0 and (lh + ld) > 0 then sync_out(); st:emit({ "r", cur_h, base_v - lh, rule_wd, lh + ld }); sync_in() end
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
              sync_out()
              while cur_h + leader_wd <= edge do
                if lid == hlist_id then
                  hlist_out(st, lb, cur_h, base_v + lshift)
                else
                  vlist_out(st, lb, cur_h, base_v + lshift - lh)
                end
                cur_h = cur_h + leader_wd + lx
              end
              sync_in()
              cur_h = edge - 10
              goto continue
            end
          end
        end
        cur_h = cur_h + rule_wd
      elseif id == kern_id then
        -- LuaTeX: kern_width(q) = width(q) + ex_kern(q); for kern nodes the Lua field
        -- `expansion_factor` *is* ex_kern, an amount in sp precomputed by the packer.
        cur_h = cur_h + getkern(n) + (getexpansion(n) or 0)
      elseif id == disc_id then
        local pre, post, replace = getdisc(n)
        if replace then
          ddepth = ddepth + 1
          if not dstack then dstack = {} end
          dstack[ddepth] = nxt
          nxt = replace
        end
      elseif id == penalty_id or id == boundary_id or id == mark_id then
        -- no output
      elseif id == margin_kern_id then
        cur_h = cur_h + getwidth(n)
      elseif id == hlist_id or id == vlist_id then
        local w, h, d = getwhd(n)
        local sh = getshift(n)
        local par = st.attr_par and getattribute(n, st.attr_par)
        local line = par and st.attr_line and getattribute(n, st.attr_line)
        sync_out()
        -- a raised/lowered box (TikZ `baseline`): extents relative to the line's baseline
        local pic = note_pic(st, n, cur_h, base_v, w, h - sh, d + sh)
        if pic then st.pic_in = st.pic_in + 1 end
        if par and par >= 0 and id == hlist_id and not st.cur then
          st:begin_line(n, par, line, cur_h, base_v + sh, w, h, d)
          hlist_out(st, n, cur_h, base_v + sh)
          st:end_line()
        elseif id == hlist_id then
          hlist_out(st, n, cur_h, base_v + sh)
        else
          vlist_out(st, n, cur_h, base_v + sh - h)
        end
        if pic then st.pic_in = st.pic_in - 1 end
        sync_in()
        cur_h = cur_h + w
      elseif id == rule_id then
        local w, h, d = getwhd(n)
        local sub = getsubtype(n)
        if h == RUNNING then h = box_h end
        if d == RUNNING then d = box_d end
        if w == RUNNING then w = 0 end
        sync_out()
        if sub == RULE_IMAGE then
          local idx = getfield(n, "index")
          if st.pic_images and st.pic_images[idx] then
            -- a picture taken from the cache: not an image hosts can draw (a region of an
            -- internal PDF); the page is degraded and rendered from the pass PDF. The detail
            -- gives the picture's rectangle (index, x, top, width, height in sp) so a host
            -- can copy it from its rendering of the page when the unit moves live.
            st:emit({ "u", "cached_picture", string.format("%d %d %d %d %d", idx, cur_h, base_v - h, w, h + d) }); st:flag("pic_cache")
          else
            st:emit({ "i", idx, cur_h, base_v - h, w, h + d }); st.images = st.images + 1
          end
        elseif sub == RULE_EMPTY then
          -- \nullfont / empty rule: occupies space, draws nothing
        elseif sub == RULE_USER or sub == RULE_OUTLINE then
          st:emit({ "u", "rule_subtype", sub }); st:flag("rule_subtype", sub)
        elseif w > 0 and (h + d) > 0 then
          st:emit({ "r", cur_h, base_v - h, w, h + d })
        end
        sync_in()
        cur_h = cur_h + w
      elseif id == math_id then
        local wd, st_, sh, sto, sho = getglue(n)
        local sub = getsubtype(n)
        sync_out(); st:emit({ "m", sub == 0 and "on" or "off", cur_h }); sync_in()
        if wd == 0 and st_ == 0 and sh == 0 then
          cur_h = cur_h + getfield(n, "surround")
        else
          cur_h = cur_h + glue_advance(wd, st_, sh, sto, sho)
        end
      elseif id == whatsit_id then
        local sub = getsubtype(n)
        sync_out()
        if sub == ws_colorstack then
          st:emit({ "c", getfield(n, "stack"), getfield(n, "cmd"), getfield(n, "data") })
        elseif sub == ws_literal then
          st:emit({ "l", getfield(n, "mode"), getfield(n, "data") }); st:flag("literal")
        elseif sub == ws_special then
          st:emit({ "l", -1, getfield(n, "data") }); st:flag("special")
        elseif silent_whatsit(sub) then
          -- bookkeeping whatsits (\write, luaotfload, hyperref) produce no output
        elseif sub == ws_save then
          st:emit({ "M", "save", cur_h, base_v, "" })
        elseif sub == ws_setmatrix then
          st:emit({ "M", "set", cur_h, base_v, getfield(n, "data") or "" })
        elseif sub == ws_restore then
          st:emit({ "M", "restore", cur_h, base_v, "" })
        else
          st:emit({ "u", "whatsit", sub }); st:flag("whatsit", sub)
        end
        sync_in()
      elseif id == local_par_id then
        local bl = getfield(n, "box_left_width") or 0
        local br = getfield(n, "box_right_width") or 0
        if bl ~= 0 or br ~= 0 then st:flag("local_boxes") end
      elseif id == dir_id then
        local dir = getfield(n, "dir")
        if dir and dir ~= "TLT" and dir ~= "+TLT" and dir ~= "-TLT" then sync_out(); st:flag("dir", dir); st:emit({ "u", "dir", dir }); sync_in() end
      elseif id == ins_id then
        st.inserts = st.inserts + 1
        if not st.opts.lines_at_top then sync_out(); st:flag("ins"); st:emit({ "u", "ins", cur_h }); sync_in() end
      elseif id == adjust_id then
        sync_out(); st:flag("adjust"); st:emit({ "u", "adjust", cur_h }); sync_in()
      else
        st:flag("node_" .. (node.type(id) or tostring(id)))
      end
      ::continue::
      if nxt == nil and ddepth > 0 then
        nxt = dstack[ddepth]
        ddepth = ddepth - 1
      end
      n = nxt
  end
  sync_out()
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
      local pic = note_pic(st, n, left + s, cur_v, w, h, d)
      if pic then st.pic_in = st.pic_in + 1 end
      if id == hlist_id then
        -- Rows: hlists reached from the traversal root through vlists only. Paragraph mode
        -- (fast path) takes every such hlist; page mode takes those tagged with a paragraph
        -- or a unit attribute, except lines of insert material (footnote text).
        local is_row = false
        local unit = nil
        -- an empty \hbox{} (page-filling material of \clearpage) is never a row
        local empty = h == 0 and d == 0 and getlist(n) == nil
        if not st.cur and not empty then
          if st.opts.lines_at_top then
            is_row = true
          else
            unit = st.attr_unit and getattribute(n, st.attr_unit)
            if unit and unit < 0 then unit = nil end
            if (par and par >= 0) or unit then
              is_row = true
              -- migrated material (footnote text) stays a tagged line but belongs to no unit
              if par and par >= 0 and st.is_insert and st.is_insert(par, unit) then unit = nil end
            end
          end
        end
        if is_row then
          st:begin_line(n, par or 0, line or (st.nlines or #st.lines) + 1, left + s, cur_v, w, h, d, unit)
          hlist_out(st, n, left + s, cur_v)
          st:end_line()
        else
          hlist_out(st, n, left + s, cur_v)
        end
      else
        vlist_out(st, n, left + s, cur_v - h)
      end
      if pic then st.pic_in = st.pic_in - 1 end
      cur_v = cur_v + d
    elseif id == rule_id then
      local w, h, d = getwhd(n)
      local sub = getsubtype(n)
      if w == RUNNING then w = box_w end
      if h == RUNNING then h = 0 end
      if d == RUNNING then d = 0 end
      if sub == RULE_IMAGE then
        local idx = getfield(n, "index")
        if st.pic_images and st.pic_images[idx] then
          st:emit({ "u", "cached_picture", string.format("%d %d %d %d %d", idx, left, cur_v, w, h + d) }); st:flag("pic_cache")
        else
          st:emit({ "i", idx, left, cur_v, w, h + d }); st.images = st.images + 1
        end
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
      elseif sub == ws_save then
        st:emit({ "M", "save", left, cur_v, "" })
      elseif sub == ws_setmatrix then
        st:emit({ "M", "set", left, cur_v, getfield(n, "data") or "" })
      elseif sub == ws_restore then
        st:emit({ "M", "restore", left, cur_v, "" })
      else
        st:emit({ "u", "whatsit", sub }); st:flag("whatsit", sub)
      end
    elseif id == penalty_id or id == mark_id then
    elseif id == ins_id then
      -- insert material (footnotes) is placed by the page builder; in a paragraph box it is
      -- not drawn here (the host keeps the page's version until the next layout)
      st.inserts = st.inserts + 1
      if not st.opts.lines_at_top then st:flag("ins") end
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
        if not self.bin then self.glyphs = self.glyphs + 1 end
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

function State:begin_line(n, par, line, x, baseline, w, h, d, unit)
  local gs = getfield(n, "glue_set")
  if self.bin then
    self.nlines = self.nlines + 1
    self.cur = true
    self:bin_rec(0x10, pack("<i4i4i4i4i4i4i4dBB", par or 0, line or 0, x, baseline, w, h, d, gs or 0.0,
      getfield(n, "glue_sign") or 0, getfield(n, "glue_order") or 0))
    if unit then self:bin_rec(0x12, pack("<i4i4", unit, 0)) end
    return
  end
  self.cur = { par = par, i = line, x = x, y = baseline, w = w, h = h, d = d, unit = unit,
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
              flags = st.flags, glyphs = st.glyphs, images = st.images, inserts = st.inserts }
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

-- Paragraph box straight to binary display-list bytes (encoding revision 1). Same traversal as
-- M.paragraph; records are written as they are produced, META/FONT/FLAG records last.
-- Returns bytes, line count, glyph count, width, height, depth, flags table.
function M.paragraph_binary(boxnode, initial_color)
  local box = todirect(boxnode)
  local st = new_state({ lines_at_top = true })
  st.bin = true
  st.buf = {}
  st.rn = 0
  st.nlines = 0
  if initial_color and initial_color ~= "" then st:emit({ "c", 0, 0, initial_color }) end
  local w, h, d = getwhd(box)
  vlist_out(st, box, 0, 0)
  st:bin_flush_run()
  local buf = st.buf
  buf[#buf + 1] = pack("<Bs4", 0x01, pack("<i4i4i4i4i4i4i4i4I4I4", w, h, d, 0, 0, 0, 0, st.inserts, st.glyphs, st.images))
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
  return pack("<c4I2I2I4I4", "RTDL", 1, 0, 16 + #body, 0) .. body, st.nlines, st.glyphs, w, h, d, st.flags, st.inserts
end

-- Internals exposed for profiling (rtex-serve.lua `profile` op).
M._hlist_out = function(st, box, left, base_v) return hlist_out(st, box, left, base_v) end
M._new_state_bin = function()
  local st = new_state({ lines_at_top = true })
  st.bin = true; st.buf = {}; st.rn = 0; st.nlines = 0
  return st
end

-- Paragraph box (a \vbox whose top-level hlists are the lines). Origin: top-left of the box.
-- `initial_color` (raw PDF color operators) is the color in force when the paragraph starts.
function M.paragraph(boxnode, initial_color)
  local box = todirect(boxnode)
  local st = new_state({ lines_at_top = true })
  if initial_color and initial_color ~= "" then st.other[1] = { "c", 0, 0, initial_color } end
  local w, h, d = getwhd(box)
  vlist_out(st, box, 0, 0)
  return result(st, "paragraph", { width = w, height = h, depth = d })
end

-- Shipped page box. `attr_par`/`attr_line` identify tagged lines. Origin: page top-left;
-- the box is offset by (1in + \hoffset, 1in + \voffset) like the PDF backend does.
function M.page(boxnode, attr_par, attr_line, page_no, glyph_attr, attr_unit, is_insert, extra)
  local box = todirect(boxnode)
  local st = new_state({ attr_par = attr_par, attr_line = attr_line, glyph_attr = glyph_attr,
                         attr_unit = attr_unit, is_insert = is_insert,
                         attr_pic = extra and extra.attr_pic, pic_images = extra and extra.pic_images })
  local one_inch = 4736286  -- 72.27pt in sp
  local ox = one_inch + tex.hoffset
  local oy = one_inch + tex.voffset
  local w, h, d = getwhd(box)
  vlist_out(st, box, ox, oy)
  return result(st, "page", { page = page_no, width = w, height = h, depth = d,
    page_width = tex.pagewidth, page_height = tex.pageheight, origin = { ox, oy }, pics = st.pics })
end

return M
