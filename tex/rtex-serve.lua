-- rtex-serve.lua: persistent paragraph server running inside a live LuaLaTeX document.
-- Requests arrive on stdin:
--   C <req> <ctx> <len>\n<len bytes>   compile <len> bytes of paragraph source (no JSON)
--   {...}\n                            any other op as a JSON object (context, ping, stats, ...)
-- Responses are frames on the FIFO named by $RTEX_RESP: u32-LE length, u8 kind, payload
-- (kind 0: JSON; kind 1: u32 json_len, JSON header, binary display list). See docs/PROTOCOL.md.
local S = { contexts = {}, errors = {}, requests = 0 }
local json = dofile(kpse.find_file("rtex-json.lua", "lua") or "rtex-json.lua")
local dl = dofile(kpse.find_file("rtex-dl.lua", "lua") or "rtex-dl.lua")
local dlbin = dofile(kpse.find_file("rtex-dl-bin.lua", "lua") or "rtex-dl-bin.lua")
local P = dofile(kpse.find_file("rtex-pic.lua", "lua") or "rtex-pic.lua")
local gettime = os.gettimeofday
local pack, format, floor = string.pack, string.format, math.floor
local stdin = io.stdin

local resp

local function send(tbl)
  local s = json.encode(tbl)
  resp:write(pack("<I4B", #s + 1, 0), s)
  resp:flush()
end
local function send_result_json(s, dlbytes)
  resp:write(pack("<I4B", 1 + 4 + #s + #dlbytes, 1), pack("<I4", #s), s, dlbytes)
  resp:flush()
end
local function send_result(tbl, dlbytes)
  send_result_json(json.encode(tbl), dlbytes)
end
local function send_error_result(req, ctx, message)
  send_result({ op = "result", req = req, ctx = ctx, status = "error", errors = { { message = message } },
                lines = 0, glyphs = 0, t_tex_us = 0, t_traverse_us = 0, t_pack_us = 0, dl_bytes = 0 }, "")
end

local function log(msg) texio.write_nl("log", "rtex-serve: " .. msg) end

-- Debug trace ($RTEX_TRACE, set by the session's debug setting): one line per request stage,
-- flushed at once, so the stage a hung request reached survives the process being killed
-- (the TeX log's tail does not).
local trace_file = os.getenv("RTEX_TRACE") and io.open(os.getenv("RTEX_TRACE"), "a") or nil
local function trace(msg)
  if not trace_file then return end
  trace_file:write(format("%.6f %s\n", gettime(), msg))
  trace_file:flush()
end

-- Error collection -------------------------------------------------------------------------
function S.on_error(...)
  S.errors[#S.errors + 1] = { message = status.lasterrorstring, context = status.lasterrorcontext,
                              line = tex.inputlineno }
end

-- Engine-state fingerprint ------------------------------------------------------------------
-- Everything a paragraph compile must leave untouched. The current font is excluded: it is
-- legitimately changed at the outer level by the font-selection tokens, and tracked separately.
local FP_CATCODES = { 92, 123, 125, 36, 38, 35, 94, 95, 37, 126 }
local tex_get, getcatcode, getcount, gettoks = tex.get, tex.getcatcode, tex.getcount, tex.gettoks
local create = token.create
local IFTRUE_MODE = create("iftrue").mode
-- Token objects report the *current* meaning of their control sequence (E24), so the kernel
-- conditionals are created once and only their mode is read per compile.
local FP_IFS = {}
local function ifmode(name)
  local ok, t = pcall(create, name)
  return ok and t and t.mode or -1
end
local function fp_if(i, name)
  local t = FP_IFS[i]
  if t == nil then
    local ok, tok = pcall(create, name)
    t = ok and tok or false
    FP_IFS[i] = t
  end
  return t and t.mode or -1
end
local FP_N = 10 + #FP_CATCODES + 10
-- The fingerprint is an array (compared element-wise against the baseline: no string building
-- on the hot path); S.fp_string renders it for the fatal report.
function S.fingerprint(fp)
  fp = fp or {}
  fp[1], fp[2], fp[3] = tex.currentgrouplevel, tex.nest.ptr, tex.interactionmode
  fp[4], fp[5], fp[6], fp[7] = gettoks("everypar"), tex_get("hsize"), tex_get("parindent"), tex_get("language")
  fp[8], fp[9], fp[10] = fp_if(1, "if@inlabel"), fp_if(2, "if@newlist"), fp_if(3, "if@minipage")
  local n = 10
  for i = 1, #FP_CATCODES do n = n + 1; fp[n] = getcatcode(FP_CATCODES[i]) end
  for i = 0, 9 do n = n + 1; fp[n] = getcount(i) end
  return fp
end
function S.fp_equal(a, b)
  for i = 1, FP_N do if a[i] ~= b[i] then return false end end
  return true
end
function S.fp_string(fp) return table.concat(fp, "|", 1, FP_N) end
local fp_scratch = {}
-- Baseline taken once the server is idle at the outer level (init, and again after each
-- font change at the outer level); every compile must end in this state.
S.fp_base = nil
S.font_outer = nil

-- Context replay -------------------------------------------------------------------------------
-- \everypar contents that may be replayed verbatim (anything else makes the unit
-- background-only on the host side; the server refuses it too): the kernel's own patterns after
-- a heading (\@afterheading), after an environment (\@doendpe), in a box (\@setminipage), and
-- microtype's \leftprotrusion.
local EVERYPAR_PATTERNS = {
  [""] = true,
  ["\\if@nobreak \\@nobreakfalse \\clubpenalty \\@M \\if@afterindent \\else {\\setbox \\z@ \\lastbox }\\fi \\else \\clubpenalty \\@clubpenalty \\everypar {}\\fi "] = true,
  ["\\if@nobreak \\@nobreakfalse \\clubpenalty \\@M \\setbox \\z@ \\lastbox \\else \\clubpenalty \\@clubpenalty \\everypar {}\\fi "] = true,
  ["{\\setbox \\z@ \\lastbox }\\everypar {}\\@endpefalse "] = true,
  ["\\@minipagefalse \\everypar {}"] = true,
}
local function everypar_allowed(ep)
  local e = ep:gsub("^\\leftprotrusion ", ""):gsub("\\leftprotrusion $", "")
  return EVERYPAR_PATTERNS[e] or false
end
local FLOAT_ENVS = { figure = "columnwidth", table = "columnwidth", ["figure*"] = "textwidth", ["table*"] = "textwidth" }
-- LaTeX counters (c@<name>) known at start-up; replayed per unit and restored after each compile
S.counter_names = {}
S.counter_base = {}
S.the_base = {}
-- idle meanings of the macros the capture reports (looked up lazily)
S.macro_base = setmetatable({}, { __index = function(t, name)
  local m = nil
  if token.get_meaning then
    local ok, mm = pcall(token.get_meaning, name)
    if ok then m = mm end
  end
  if m == nil then
    local ok, b = pcall(token.get_macro, name)
    m = ok and b and ("->" .. b) or false
  end
  rawset(t, name, m)
  return m
end })
-- token.get_meaning gives "#1#2->body" (no "macro:" prefix, no \long): rebuild the
-- definition as \def\name#1#2{body}, \long when the server's current meaning is a long macro
-- or the name is undefined here (\newcommand defines long macros); nil when not a macro meaning
function S.def_from_meaning(name, m)
  local params, body = m:match("^(.-)%->(.*)$")
  if not params then return nil end
  local pre = "\\long"
  local ok, t = pcall(create, name)
  if ok and t and t.cmdname == "call" then pre = "" end
  return pre .. "\\def\\" .. name .. params .. "{" .. body .. "}"
end

-- Contexts are preprocessed when installed: parameter tables become flat arrays, and the
-- NFSS selection string is built once.
local tex_set, setglue = tex.set, tex.setglue
local function install_context(id, ctx)
  local ints, dims, glues = {}, {}, {}
  -- Contexts are installed while the server idles at the outer level, and the unit box
  -- inherits that level's parameters, so only integer and dimen parameters that differ from
  -- the idle values need replaying. Glue parameters are always replayed: \selectfont at the
  -- outer level (font changes between requests) rewrites \baselineskip.
  for k, v in pairs(ctx.ints or {}) do
    local ok, cur = pcall(tex_get, k)
    if not ok or cur ~= v then ints[#ints + 1] = k; ints[#ints + 1] = v end
  end
  for k, v in pairs(ctx.dims or {}) do
    local ok, cur = pcall(tex_get, k)
    if not ok or cur ~= v then dims[#dims + 1] = k; dims[#dims + 1] = v end
  end
  for k, v in pairs(ctx.glues or {}) do glues[#glues + 1] = { k, v[1] or 0, v[2] or 0, v[3] or 0, v[4] or 0, v[5] or 0 } end
  ctx.a_ints, ctx.a_dims, ctx.a_glues = ints, dims, glues
  -- tokens after \bgroup: counters (TeX assignments, local to the group; `page` is never
  -- replayed), \if@nobreak and friends, \everypar replay, float-box emulation
  local kind, name = ctx.kind or "par", ctx.name
  local extra = {}
  local names = {}
  for k, v in pairs(ctx.counters or {}) do
    -- only counters that are valid control words (biblatex has `bbx:relatedcount`), that the
    -- server knows, that differ from its idle value, and never the page counter
    if S.counter_base[k] ~= nil and S.counter_base[k] ~= v and k ~= "page" and k:match("^[%a@]+$") then names[#names + 1] = k end
  end
  table.sort(names)
  for _, k in ipairs(names) do extra[#extra + 1] = format("\\c@%s=%d ", k, ctx.counters[k]) end
  -- \the<counter> formats that differ from the idle ones (\appendix, \renewcommand{\thesection})
  local fnames = {}
  for k, v in pairs(ctx.thefmt or {}) do
    if S.the_base[k] ~= nil and S.the_base[k] ~= v and k:match("^[%a@]+$") then fnames[#fnames + 1] = k end
  end
  table.sort(fnames)
  for _, k in ipairs(fnames) do extra[#extra + 1] = format("\\def\\the%s{%s}", k, ctx.thefmt[k]) end
  -- macros the body (re)defines, in the meaning they had at the unit ("macro:#1->body")
  local mnames = {}
  for k, v in pairs(ctx.macros or {}) do
    if k:match("^[%a@]+$") and S.macro_base[k] ~= v then mnames[#mnames + 1] = k end
  end
  table.sort(mnames)
  for _, k in ipairs(mnames) do
    local d = S.def_from_meaning(k, ctx.macros[k])
    if d then extra[#extra + 1] = d end
  end
  extra[#extra + 1] = ctx.nobreak and "\\@nobreaktrue " or "\\@nobreakfalse "
  extra[#extra + 1] = ctx.afterindent and "\\@afterindenttrue " or "\\@afterindentfalse "
  extra[#extra + 1] = ctx.noskipsec and "\\@noskipsectrue " or "\\@noskipsecfalse "
  local ep = ctx.everypar or ""
  if ep ~= "" then extra[#extra + 1] = "\\everypar{" .. ep .. "}" end
  ctx.float = kind == "env" and name and FLOAT_ENVS[name] or nil
  if ctx.float then
    -- what \@xfloat does to the float box content (\@floatboxreset = \reset@font\normalsize\@setminipage)
    extra[#extra + 1] = "\\def\\@captype{" .. name:gsub("%*$", "") .. "}\\hsize\\" .. ctx.float .. "\\@parboxrestore\\@floatboxreset "
  end
  ctx.head_extra = table.concat(extra)
  ctx.tail_extra = ctx.float and "\\par\\vskip\\z@skip" or ""
  local b = ctx.begin
  local n = b and b.nfss
  if n and n.family then
    ctx.nfss_tokens = format("\\fontencoding{%s}\\fontfamily{%s}\\fontseries{%s}\\fontshape{%s}\\fontsize{%s}{%s}\\selectfont",
      n.enc or "TU", n.family, n.series or "m", n.shape or "n", n.size or "10", n.baselineskip or "12pt")
  else
    ctx.nfss_tokens = ""
  end
  ctx.color = b and b.color
  if ctx.color == "0 g 0 G" or ctx.color == "" then ctx.color = nil end
  ctx.everypar_ok = everypar_allowed(ctx.everypar or "")
  S.contexts[id] = ctx
end

-- Strip the \begin{float}[opt] ... \end{float} wrapper: the content is typeset directly in the
-- unit box with the float-box state replayed by head_extra.
local function float_content(source, name)
  local n = name:gsub("%*", "%%*")
  local inner = source:gsub("^%s*\\begin%s*{" .. n .. "}%s*%b[]", ""):gsub("^%s*\\begin%s*{" .. n .. "}", "")
  inner = inner:gsub("\\end%s*{" .. n .. "}%s*$", "")
  return inner
end

-- Called from TeX (\luafunction) inside the paragraph \vbox group, before any text.
function S.apply()
  local cur = S.current
  if not cur then return end
  cur.t_apply = gettime()
  local ctx = cur.ctx
  local a = ctx.a_ints
  for i = 1, #a, 2 do tex_set(a[i], a[i + 1]) end
  a = ctx.a_dims
  for i = 1, #a, 2 do tex_set(a[i], a[i + 1]) end
  a = ctx.a_glues
  for i = 1, #a do local g = a[i]; setglue(g[1], g[2], g[3], g[4], g[5], g[6]) end
  if ctx.parshape and #ctx.parshape > 0 then tex.parshape = ctx.parshape end
  cur.t_applied = gettime()
end

-- The font selected at the server's outer level persists across requests; re-select only when
-- the context's NFSS state differs from the last one selected (saves ~0.3-0.5 ms per request).
local last_nfss = nil

-- Compile -----------------------------------------------------------------------------------------
-- A request is served in two halves without any nested TeX main loop (tex.runtoks leaks one
-- input level per call when invoked from a \directlua that never returns, E14):
--   step()   reads the request and *prints* the tokens that build the paragraph box, ending with
--            \luafunction<finish>; then returns to TeX, which executes them;
--   finish() traverses the box, checks the state fingerprint and sends the result.
-- The TeX side runs `\loop\rtexstep\ifnum\rtexcontinue>0 \repeat`.
S.current = nil
local head_tokens, tail_tokens  -- built in init once the \luafunction slots are known

-- Leak check: the meanings of the control sequences a unit's source mentions, before and
-- after its compile (every distinct \name in the source; a name built with \csname is not
-- seen — documented limitation). A unit that defines or \lets one of them (\newcommand inside a
-- paragraph, \global\let\emph\relax) changes the server for every later compile; the result
-- reports the names and the session demotes the unit and restarts the engine.
local function cs_names(source)
  local seen, names = {}, {}
  for name in source:gmatch("\\(%a+)") do
    if not seen[name] then seen[name] = true; names[#names + 1] = name end
  end
  return names
end
local function meaning_of(name)
  local ok, t = pcall(create, name)
  if not ok or not t then return "?" end
  local cmd = t.cmdname
  if cmd == "call" or cmd == "long_call" or cmd == "outer_call" or cmd == "long_outer_call" then
    local ok2, body = pcall(token.get_macro, name)
    return cmd .. ":" .. (ok2 and tostring(body) or "?")
  end
  return cmd .. ":" .. tostring(t.mode)
end
local function meanings_of(names)
  local m = {}
  for i = 1, #names do m[i] = meaning_of(names[i]) end
  return m
end

-- Picture cache in the live engine: a compile may come with cache entries for the picture
-- environments of its source, in order (null for one not cached). The wrapped begin macro
-- (rtex-pic.tex) takes the next entry for every \begin{<picture env>} it sees, when the
-- font/color/width state matches, and places the cached region instead of drawing. The result
-- reports how many pictures were seen and how many came from the cache.
S.cache_images = {}
function S.pic_arm(env) P.arm(S.picctl, env) end
function S.pic_begin(env)
  local cur = S.current
  local lookup = function(_, state)
    if not cur then return nil end
    cur.pics_seen = cur.pics_seen + 1
    local e = cur.pics and cur.pics[cur.pics_seen] or nil
    if e and e ~= json.null and type(e) == "table" and (e.state or "") == state then
      cur.pics_used = cur.pics_used + 1
      return e
    end
    return nil
  end
  P.begin(S.picctl, env, lookup, nil)
end
function S.pic_write() P.write(S.picctl, S.cache_images) end
function S.pic_end() end

function S.begin_compile(req_id, ctx_id, source, pics)
  local ctx = S.contexts[ctx_id]
  if not ctx then
    send_error_result(req_id, ctx_id, "unknown context " .. tostring(ctx_id))
    return
  end
  if not ctx.everypar_ok then
    send_error_result(req_id, ctx_id, "context has a non-replayable \\everypar")
    return
  end
  S.errors = {}
  local ftoks = ctx.nfss_tokens
  local font_changed = ftoks ~= last_nfss
  if font_changed then last_nfss = ftoks else ftoks = "" end
  if ctx.float then source = float_content(source, ctx.name) end
  local lines = {}
  local n = 0
  for line in (source .. "\n"):gmatch("(.-)\n") do n = n + 1; lines[n] = line end
  if n > 0 and lines[n] == "" then lines[n] = nil end
  S.current = { req = req_id, ctx_id = ctx_id, ctx = ctx, font_changed = font_changed, t0 = 0,
                t_read = S.t_read, t_decoded = S.t_decoded, names = cs_names(source),
                pics = pics, pics_seen = 0, pics_used = 0 }
  S.current.meanings = meanings_of(S.current.names)
  S.images_used = false
  if trace_file then trace(format("begin req %s ctx %s bytes %d lines %d pics %s", tostring(req_id), tostring(ctx_id), #source, n, pics and #pics or 0)) end
  -- our own tokens use @ as a letter (kernel switches, float emulation); the source keeps the
  -- document's catcodes
  tex.print(S.cct, ftoks .. head_tokens .. ctx.head_extra)
  tex.print(lines)
  tex.print(S.cct, ctx.tail_extra .. tail_tokens)
  S.current.t_printed = gettime()
end

-- graphicx hook (driver): image resource index -> file
S.images = {}
function S.image(index, file, page, pages)
  S.images[tostring(index)] = { index = index, file = file, page = tonumber(page) or 1, pages = pages }
  S.images_used = true
end

function S.mark()
  local cur = S.current
  if cur then
    cur.t_mark = gettime()
    if trace_file then trace("mark (context replayed) req " .. tostring(cur.req)) end
    if cur.font_changed then
      -- the font-selection tokens ran at the outer level: re-baseline
      S.font_outer = font.current()
      S.fp_base = S.fingerprint()
    end
    cur.t0 = gettime()
  end
end

local HEADER_FMT = '{"op":"result","req":%d,"ctx":%d,"status":"%s","errors":[],"lines":%d,"glyphs":%d,%s' ..
  '"width":%d,"height":%d,"depth":%d,"t_tex_us":%d,"t_traverse_us":%d,"t_pack_us":0,"font_changed":%s,"dl_bytes":%d,' ..
  '"stages_us":{"decode":%d,"prepare":%d,"to_mark":%d,"mark_to_apply":%d,"apply":%d,"apply_to_finish":%d,"fingerprint":%d}}'

function S.finish()
  local cur = S.current
  S.current = nil
  if not cur then return end
  local t1 = gettime()
  if trace_file then trace(format("finish req %s errors %d", tostring(cur.req), #S.errors)) end
  -- counters the unit advanced globally (\refstepcounter, \stepcounter …) are reported
  -- (value at the end, when it differs from the replayed start value) and go back to the idle
  -- values
  local cb = S.counter_base
  local start = cur.ctx and cur.ctx.counters or {}
  local advanced = nil
  for name, v in pairs(cb) do
    local now = getcount("c@" .. name)
    if now ~= v then
      tex.setcount("global", "c@" .. name, v)
      if name ~= "page" and now ~= (start[name] or v) then
        advanced = advanced or {}
        advanced[name] = now
      end
    end
  end
  cur.advanced = advanced
  -- control sequences of the source whose meaning changed: a definition leaked
  local leaks = nil
  if cur.names then
    for i = 1, #cur.names do
      if meaning_of(cur.names[i]) ~= cur.meanings[i] then
        leaks = leaks or {}
        leaks[#leaks + 1] = cur.names[i]
      end
    end
  end
  cur.leaks = leaks
  local fp1 = S.fingerprint(fp_scratch)
  local t1b = gettime()
  if font.current() ~= S.font_outer or not S.fp_equal(fp1, S.fp_base) then
    send{ op = "fatal", req = cur.req, reason = "state_mismatch", before = S.fp_string(S.fp_base), after = S.fp_string(fp1), errors = S.errors }
    resp:flush()
    os.exit(3)
  end
  local box = tex.box[S.boxnum]
  local bytes, nlines, nglyphs, bw, bh, bd, flags = "", 0, 0, 0, 0, 0, nil
  if box then
    -- traversal and serialization fused: records are written while walking the nodes
    bytes, nlines, nglyphs, bw, bh, bd, flags = dl.paragraph_binary(box, cur.ctx.color)
  end
  local t2 = gettime()
  local st = (#S.errors > 0) and "error" or ((flags and next(flags)) and "ok_degraded" or "ok")
  local t_tex = floor((t1 - cur.t0) * 1e6 + 0.5)
  local t_trav = floor((t2 - t1b) * 1e6 + 0.5)
  if #S.errors > 0 then
    send_result({ op = "result", req = cur.req, ctx = cur.ctx_id, status = st, errors = S.errors, counters = cur.advanced, leaks = cur.leaks,
      lines = nlines, glyphs = nglyphs, width = bw, height = bh, depth = bd,
      t_tex_us = t_tex, t_traverse_us = t_trav, t_pack_us = 0, font_changed = cur.font_changed, dl_bytes = #bytes }, bytes)
  else
    local images = ""
    if S.images_used and next(S.images) then images = '"images":' .. json.encode(S.images) .. ',' end
    if cur.advanced then images = images .. '"counters":' .. json.encode(cur.advanced) .. ',' end
    if cur.leaks then images = images .. '"leaks":' .. json.encode(cur.leaks) .. ',' end
    if cur.pics then images = images .. '"pics_seen":' .. cur.pics_seen .. ',"pics_used":' .. cur.pics_used .. ',' end
    send_result_json(format(HEADER_FMT, cur.req, cur.ctx_id, st, nlines, nglyphs, images, bw, bh, bd, t_tex, t_trav,
      cur.font_changed and "true" or "false", #bytes,
      floor(((cur.t_decoded or 0) - (cur.t_read or 0)) * 1e6 + 0.5), floor(((cur.t_printed or 0) - (cur.t_decoded or 0)) * 1e6 + 0.5),
      floor(((cur.t_mark or 0) - (cur.t_printed or 0)) * 1e6 + 0.5),
      floor(((cur.t_apply or 0) - (cur.t0 or 0)) * 1e6 + 0.5), floor(((cur.t_applied or 0) - (cur.t_apply or 0)) * 1e6 + 0.5),
      floor((t1 - (cur.t_applied or 0)) * 1e6 + 0.5), floor((t1b - t1) * 1e6 + 0.5)), bytes)
  end
  S.requests = S.requests + 1
end

-- Profile the replay overhead (diagnostic only; uses tex.runtoks, which leaks one input level per
-- call, so do not run it thousands of times in a long-lived server).
function S.profile(req)
  local ctx = S.contexts[req.ctx]
  local n = req.n or 50
  local function med(t) table.sort(t); return floor(t[(#t + 1) // 2] * 1e6 + 0.5) end
  local ptrs = {}
  local function timeit(f)
    local r = {}
    local p0 = status.input_ptr
    for _ = 1, n do local a = gettime(); f(); r[#r + 1] = gettime() - a end
    ptrs[#ptrs + 1] = format("%d->%d", p0, status.input_ptr)
    return med(r)
  end
  local lines = {}
  for line in ((req.source or "") .. "\n"):gmatch("(.-)\n") do lines[#lines + 1] = line end
  local out = {}
  S.current = { ctx = ctx }
  out.empty_runtoks = timeit(function() tex.runtoks(function() tex.print("\\relax") end) end)
  out.apply_only = timeit(function() S.apply() end)
  out.fingerprint = timeit(function() S.fingerprint() end)
  out.group_box_empty = timeit(function()
    tex.runtoks(function() tex.print("\\begingroup\\global\\setbox\\rtexbox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " \\par\\egroup\\endgroup") end)
  end)
  out.print_source_only = timeit(function()
    tex.runtoks(function() tex.print("\\begingroup\\global\\setbox\\rtexbox\\vbox\\bgroup") tex.print(lines) tex.print("\\par\\egroup\\endgroup") end)
  end)
  out.full_compile = timeit(function()
    tex.runtoks(function()
      tex.print(S.cct, "\\begingroup\\global\\setbox\\rtexbox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " " .. (ctx.head_extra or ""))
      tex.print(lines)
      tex.print(S.cct, (ctx.tail_extra or "") .. "\\par\\egroup\\endgroup")
    end)
  end)
  S.current = nil
  -- bare walk: node ids and next pointers only, recursing into boxes (lower bound for traversal)
  local D = node.direct
  local getid, getnext, getlist = D.getid, D.getnext, D.getlist
  local hl, vl, gl = node.id("hlist"), node.id("vlist"), node.id("glyph")
  local counts = { nodes = 0, glyphs = 0, boxes = 0 }
  local function bare(list)
    local n = list
    while n do
      local id = getid(n)
      counts.nodes = counts.nodes + 1
      if id == hl or id == vl then counts.boxes = counts.boxes + 1; bare(getlist(n))
      elseif id == gl then counts.glyphs = counts.glyphs + 1 end
      n = getnext(n)
    end
  end
  out.walk_only = timeit(function() counts = { nodes = 0, glyphs = 0, boxes = 0 }; bare(getlist(D.todirect(tex.box[S.boxnum]))) end)
  out.node_counts = counts
  local getfont, getchar, getexpansion, getoffsets, getwidth = D.getfont, D.getchar, D.getexpansion, D.getoffsets, D.getwidth
  local pack = string.pack
  local function fields(list)
    local n = list
    while n do
      local id = getid(n)
      if id == hl or id == vl then fields(getlist(n))
      elseif id == gl then local f, c, ef = getfont(n), getchar(n), getexpansion(n); local xo, yo = getoffsets(n); local w = getwidth(n); local s = pack("<I4I4i4i4", c, 1, w, w) end
      n = getnext(n)
    end
  end
  out.walk_fields_pack = timeit(function() fields(getlist(D.todirect(tex.box[S.boxnum]))) end)
  -- section timing of the binary traversal
  do
    local box = D.todirect(tex.box[S.boxnum])
    local hl2 = node.id("hlist")
    local lines_t, nl = 0, 0
    local gt = os.gettimeofday
    local a = gt()
    for _ = 1, n do
      local st = dl._new_state_bin()
      local m = getlist(box)
      local cur_v = 0
      while m do
        if getid(m) == hl2 then
          local w, h, d = D.getwhd(m)
          cur_v = cur_v + h
          local b = gt()
          st:begin_line(m, 0, 1, 0, cur_v, w, h, d)
          dl._hlist_out(st, m, 0, cur_v)
          st:end_line()
          lines_t = lines_t + (gt() - b)
          nl = nl + 1
          cur_v = cur_v + d
        end
        m = getnext(m)
      end
      st:bin_flush_run()
    end
    out.sections = { per_line_us = math.floor(lines_t / nl * 1e6 + 0.5), lines = nl // n, loop_total_us = math.floor((gt() - a) / n * 1e6 + 0.5) }
  end
  out.traverse = timeit(function() dl.paragraph(tex.box[S.boxnum]) end)
  out.traverse_binary = timeit(function() dl.paragraph_binary(tex.box[S.boxnum]) end)
  collectgarbage("stop")
  out.traverse_binary_nogc = timeit(function() dl.paragraph_binary(tex.box[S.boxnum]) end)
  out.walk_fields_pack_nogc = timeit(function() fields(getlist(D.todirect(tex.box[S.boxnum]))) end)
  out.full_compile_nogc = timeit(function()
    tex.runtoks(function()
      tex.print("\\begingroup\\global\\setbox\\rtexbox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " ")
      tex.print(lines)
      tex.print("\\par\\egroup\\endgroup")
    end)
  end)
  collectgarbage("restart")
  out.lua_heap_kb = math.floor(collectgarbage("count"))
  local r = dl.paragraph(tex.box[S.boxnum])
  out.encode = timeit(function() dlbin.encode(r) end)
  out.n = n
  send{ op = "profile", req = req.req, us = out, input_ptr_track = ptrs, input_ptr = status.input_ptr }
end

function S.stats()
  send{ op = "stats", requests = S.requests, font_nextid = font.nextid(), node_mem = status.node_mem_usage,
        grouplevel = tex.currentgrouplevel, nest = tex.nest.ptr, luastate = collectgarbage("count"),
        input_ptr = status.input_ptr, max_in_stack = status.max_in_stack, save_size = status.save_size,
        cs_count = status.cs_count, buf_size = status.buf_size }
end

function S.dispatch(req)
  if trace_file and req.op ~= "compile" then trace("op " .. tostring(req.op) .. " id " .. tostring(req.id)) end
  if req.op == "context" then
    install_context(req.id, req.ctx)
    send{ op = "ok", id = req.id }
  elseif req.op == "compile" then
    S.begin_compile(req.req, req.ctx, req.source or "")
  elseif req.op == "labels" then
    -- \newlabel / \bibcite definitions from the last pass's aux: \r@name, \b@key
    local n = 0
    for _, kv in ipairs(req.labels or {}) do
      token.set_macro(kv[1], kv[2], "global")
      n = n + 1
    end
    send{ op = "ok", id = n }
  elseif req.op == "ping" then
    send{ op = "pong" }
  elseif req.op == "stats" then
    S.stats()
  elseif req.op == "profile" then
    S.profile(req)
  else
    send{ op = "error", message = "unknown op " .. tostring(req.op) }
  end
end

-- Main loop ----------------------------------------------------------------------------------------
-- init(boxnum, countnum): open the FIFO, register hooks and \luafunction slots, send ready.
-- step(): read and dispatch ONE request, then return to TeX (which executes any printed tokens
-- and re-enters step() through the \loop in the driver). Sets \count<countnum> to 0 to stop.
function S.init(boxnum, countnum, cctnum)
  S.boxnum = boxnum
  S.countnum = countnum
  local path = os.getenv("RTEX_RESP")
  resp = assert(io.open(path, "wb"), "cannot open response FIFO " .. tostring(path))
  resp:setvbuf("full", 1 << 16)
  luatexbase.add_to_callback("show_error_hook", S.on_error, "rtex-serve")
  -- \luafunction slots: no chunk compilation per call, unlike \directlua{...}
  local fns = lua.get_functions_table()
  local base = #fns
  fns[base + 1] = function() S.step() end
  fns[base + 2] = function() S.mark() end
  fns[base + 3] = function() S.apply() end
  fns[base + 4] = function() S.finish() end
  S.fn_step, S.fn_mark, S.fn_apply, S.fn_finish = base + 1, base + 2, base + 3, base + 4
  token.set_macro("rtexstep", "\\luafunction" .. S.fn_step .. " ", "global")
  -- catcode table for the tokens we print around the source: LaTeX's catcodes with @ a letter
  -- (the driver allocates and saves it: \newcatcodetable\rtexcct{\makeatletter\savecatcodetable\rtexcct})
  S.cct = cctnum
  -- picture cache: wrap the picture environments' begin macros (rtex-pic.tex) and mark cached
  -- images for the display-list traversal
  S.picctl = P.new(cctnum)
  dl.pic_images = S.cache_images
  tex.print(cctnum, "\\def\\rtex@picns{rtex_serve}\\def\\rtex@picenvs{" .. table.concat(P.PICTURE_ENVS, ",") ..
            "}\\input{rtex-pic.tex}\\rtex@wrappicall ")
  -- LaTeX counters: idle values restored after every compile
  local ck = token.get_macro("cl@@ckpt") or ""
  for name in ck:gmatch("\\@elt%s*{([^}]*)}") do
    local ok, v = pcall(getcount, "c@" .. name)
    if ok then
      S.counter_names[#S.counter_names + 1] = name; S.counter_base[name] = v
      local okm, body = pcall(token.get_macro, "the" .. name)
      S.the_base[name] = okm and body or nil
    end
  end
  local nobreak_idle = ifmode("if@nobreak") == IFTRUE_MODE
  -- Not \globaldefs=-1 inside the box (tried as a universal leak barrier): LaTeX's own
  -- internals rely on \begingroup…\global…\endgroup to publish results (NFSS font definitions,
  -- enumitem's keyval, hyperref, array), so making every assignment local breaks fonts and
  -- tables. Leaks are caught by the state fingerprint and the counter restore instead.
  head_tokens = "\\luafunction" .. S.fn_mark .. " \\begingroup\\global\\setbox\\rtexbox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " "
  -- the title block's kernel macros (rtex-serve-patches.tex, 5) come back after every compile
  local restore_title = token.is_defined("rtex@restoretitle") and "\\rtex@restoretitle " or ""
  tail_tokens = "\\par\\egroup\\endgroup" .. (nobreak_idle and "\\global\\@nobreaktrue " or "\\global\\@nobreakfalse ") .. restore_title .. "\\luafunction" .. S.fn_finish .. " "
  S.fp_base = S.fingerprint()
  S.font_outer = font.current()
  send{ op = "ready", banner = status.banner, luatex_version = status.luatex_version,
        fingerprint = S.fp_string(S.fp_base), font_nextid = font.nextid() }
  log("ready")
  if trace_file then
    trace("ready; trace " .. tostring(os.getenv("RTEX_TRACE")))
    -- Debug extras, after "ready" and under pcall: nothing here may keep the server from
    -- starting. Font loads with their cost (a first use of a big font can take seconds): the
    -- define_font callback is wrapped whoever registered it (luaotfload, or luatexja which
    -- replaces luaotfload's). Every macro expansion goes to the TeX log (\tracingmacros), so a
    -- loop shows in the log's last flushed block even though the killed process loses the tail.
    local ok, err = pcall(function()
      local descs = luatexbase.callback_descriptions("define_font")
      local name = descs and descs[1]
      if not name then return end
      local prev = luatexbase.remove_from_callback("define_font", name)
      if not prev then return end
      luatexbase.add_to_callback("define_font", function(fname, size, id)
        local t = gettime()
        local f = prev(fname, size, id)
        trace(format("font %s size %s id %s: %.1f ms", tostring(fname), tostring(size), tostring(id), (gettime() - t) * 1000))
        return f
      end, name)
      tex.tracingmacros = 1
      tex.tracingonline = 0
    end)
    if not ok then trace("debug extras not installed: " .. tostring(err)) end
  end
end

function S.stop()
  tex.setcount(S.countnum, 0)
  if resp then resp:close() end
end

local function handle_json(line)
  local ok, req = pcall(json.decode, line)
  S.t_decoded = gettime()
  if not ok then
    send{ op = "error", message = "bad request: " .. tostring(req) }
  elseif req.op == "shutdown" then
    send{ op = "bye" }
    S.stop()
  else
    local hok, herr = pcall(S.dispatch, req)
    if not hok then
      log("handler error: " .. tostring(herr))
      S.current = nil
      if req.op == "compile" then
        send_error_result(req.req, req.ctx, "lua: " .. tostring(herr))
      else
        send{ op = "error", message = "lua: " .. tostring(herr) }
      end
    end
  end
end

function S.step()
  local line = stdin:read("l")
  S.t_read = gettime()
  if not line then log("stdin closed"); S.stop(); return end
  local c = line:byte(1)
  if c == 67 then -- 'C': compile frame
    local req_id, ctx_id, len, plen = line:match("^C (%d+) (%-?%d+) (%d+) ?(%d*)$")
    if not req_id then
      send{ op = "error", message = "bad compile frame: " .. line }
      return
    end
    len = tonumber(len)
    plen = tonumber(plen) or 0
    local source = len > 0 and stdin:read(len) or ""
    if not source or #source ~= len then log("stdin closed mid-frame"); S.stop(); return end
    -- optional picture cache entries (JSON array) after the source
    local pics = nil
    if plen > 0 then
      local pj = stdin:read(plen)
      if not pj or #pj ~= plen then log("stdin closed mid-frame"); S.stop(); return end
      local ok, v = pcall(json.decode, pj)
      if ok and type(v) == "table" then pics = v end
    end
    S.t_decoded = gettime()
    local hok, herr = pcall(S.begin_compile, tonumber(req_id), tonumber(ctx_id), source, pics)
    if not hok then
      log("handler error: " .. tostring(herr))
      S.current = nil
      send_error_result(tonumber(req_id), tonumber(ctx_id), "lua: " .. tostring(herr))
    end
  elseif c == nil then
    return
  else
    handle_json(line)
  end
end

return S
