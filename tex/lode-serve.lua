-- lode-serve.lua: persistent paragraph server running inside a live LuaLaTeX document.
-- Requests arrive on stdin:
--   C <req> <ctx> <len>\n<len bytes>   compile <len> bytes of paragraph source (no JSON)
--   {...}\n                            any other op as a JSON object (context, ping, stats, ...)
-- Responses are frames on the FIFO named by $LODE_RESP: u32-LE length, u8 kind, payload
-- (kind 0: JSON; kind 1: u32 json_len, JSON header, binary display list). See docs/PROTOCOL.md.
local S = { contexts = {}, errors = {}, requests = 0 }
local json = dofile(kpse.find_file("lode-json.lua", "lua") or "lode-json.lua")
local dl = dofile(kpse.find_file("lode-dl.lua", "lua") or "lode-dl.lua")
local dlbin = dofile(kpse.find_file("lode-dl-bin.lua", "lua") or "lode-dl-bin.lua")
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

local function log(msg) texio.write_nl("log", "lode-serve: " .. msg) end

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
function S.fingerprint()
  local fp = { tex.currentgrouplevel, tex.nest.ptr, tex.interactionmode,
               gettoks("everypar"), tex_get("hsize"), tex_get("parindent"), tex_get("language") }
  local n = 7
  for i = 1, #FP_CATCODES do n = n + 1; fp[n] = getcatcode(FP_CATCODES[i]) end
  for i = 0, 9 do n = n + 1; fp[n] = getcount(i) end
  return table.concat(fp, "|", 1, n)
end
-- Baseline taken once the server is idle at the outer level (init, and again after each
-- font change at the outer level); every compile must end in this state.
S.fp_base = nil
S.font_outer = nil

-- Context replay -------------------------------------------------------------------------------
-- \everypar contents that may be replayed verbatim (anything else makes the paragraph
-- background-only on the host side; the server refuses it too).
local EVERYPAR_ALLOWED = { [""] = true, ["\\leftprotrusion "] = true }

-- Contexts are preprocessed when installed: parameter tables become flat arrays, and the
-- NFSS selection string is built once.
local tex_set, setglue = tex.set, tex.setglue
local function install_context(id, ctx)
  local ints, dims, glues = {}, {}, {}
  for k, v in pairs(ctx.ints or {}) do ints[#ints + 1] = k; ints[#ints + 1] = v end
  for k, v in pairs(ctx.dims or {}) do dims[#dims + 1] = k; dims[#dims + 1] = v end
  for k, v in pairs(ctx.glues or {}) do glues[#glues + 1] = { k, v[1] or 0, v[2] or 0, v[3] or 0, v[4] or 0, v[5] or 0 } end
  ctx.a_ints, ctx.a_dims, ctx.a_glues = ints, dims, glues
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
  ctx.everypar_ok = EVERYPAR_ALLOWED[ctx.everypar or ""] or false
  S.contexts[id] = ctx
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
  local ep = ctx.everypar or ""
  if ep ~= "" then tex.settoks("everypar", ep) end
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
-- The TeX side runs `\loop\lodestep\ifnum\lodecontinue>0 \repeat`.
S.current = nil
local head_tokens, tail_tokens  -- built in init once the \luafunction slots are known

function S.begin_compile(req_id, ctx_id, source)
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
  local lines = {}
  local n = 0
  for line in (source .. "\n"):gmatch("(.-)\n") do n = n + 1; lines[n] = line end
  if n > 0 and lines[n] == "" then lines[n] = nil end
  S.current = { req = req_id, ctx_id = ctx_id, ctx = ctx, font_changed = font_changed, t0 = 0,
                t_read = S.t_read, t_decoded = S.t_decoded }
  tex.print(ftoks .. head_tokens)
  tex.print(lines)
  tex.print(tail_tokens)
  S.current.t_printed = gettime()
end

function S.mark()
  local cur = S.current
  if cur then
    cur.t_mark = gettime()
    if cur.font_changed then
      -- the font-selection tokens ran at the outer level: re-baseline
      S.font_outer = font.current()
      S.fp_base = S.fingerprint()
    end
    cur.t0 = gettime()
  end
end

local HEADER_FMT = '{"op":"result","req":%d,"ctx":%d,"status":"%s","errors":[],"lines":%d,"glyphs":%d,' ..
  '"width":%d,"height":%d,"depth":%d,"t_tex_us":%d,"t_traverse_us":%d,"t_pack_us":0,"font_changed":%s,"dl_bytes":%d,' ..
  '"stages_us":{"decode":%d,"prepare":%d,"to_mark":%d,"mark_to_apply":%d,"apply":%d,"apply_to_finish":%d,"fingerprint":%d}}'

function S.finish()
  local cur = S.current
  S.current = nil
  if not cur then return end
  local t1 = gettime()
  local fp1 = S.fingerprint()
  local t1b = gettime()
  if fp1 ~= S.fp_base or font.current() ~= S.font_outer then
    send{ op = "fatal", req = cur.req, reason = "state_mismatch", before = S.fp_base, after = fp1, errors = S.errors }
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
    send_result({ op = "result", req = cur.req, ctx = cur.ctx_id, status = st, errors = S.errors,
      lines = nlines, glyphs = nglyphs, width = bw, height = bh, depth = bd,
      t_tex_us = t_tex, t_traverse_us = t_trav, t_pack_us = 0, font_changed = cur.font_changed, dl_bytes = #bytes }, bytes)
  else
    send_result_json(format(HEADER_FMT, cur.req, cur.ctx_id, st, nlines, nglyphs, bw, bh, bd, t_tex, t_trav,
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
    tex.runtoks(function() tex.print("\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " \\par\\egroup\\endgroup") end)
  end)
  out.print_source_only = timeit(function()
    tex.runtoks(function() tex.print("\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup") tex.print(lines) tex.print("\\par\\egroup\\endgroup") end)
  end)
  out.full_compile = timeit(function()
    tex.runtoks(function()
      tex.print("\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " ")
      tex.print(lines)
      tex.print("\\par\\egroup\\endgroup")
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
      tex.print("\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " ")
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
  if req.op == "context" then
    install_context(req.id, req.ctx)
    send{ op = "ok", id = req.id }
  elseif req.op == "compile" then
    S.begin_compile(req.req, req.ctx, req.source or "")
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
function S.init(boxnum, countnum)
  S.boxnum = boxnum
  S.countnum = countnum
  local path = os.getenv("LODE_RESP")
  resp = assert(io.open(path, "wb"), "cannot open response FIFO " .. tostring(path))
  resp:setvbuf("full", 1 << 16)
  luatexbase.add_to_callback("show_error_hook", S.on_error, "lode-serve")
  -- \luafunction slots: no chunk compilation per call, unlike \directlua{...}
  local fns = lua.get_functions_table()
  local base = #fns
  fns[base + 1] = function() S.step() end
  fns[base + 2] = function() S.mark() end
  fns[base + 3] = function() S.apply() end
  fns[base + 4] = function() S.finish() end
  S.fn_step, S.fn_mark, S.fn_apply, S.fn_finish = base + 1, base + 2, base + 3, base + 4
  token.set_macro("lodestep", "\\luafunction" .. S.fn_step .. " ", "global")
  head_tokens = "\\luafunction" .. S.fn_mark .. " \\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\luafunction" .. S.fn_apply .. " "
  tail_tokens = "\\par\\egroup\\endgroup\\luafunction" .. S.fn_finish .. " "
  S.fp_base = S.fingerprint()
  S.font_outer = font.current()
  send{ op = "ready", banner = status.banner, luatex_version = status.luatex_version,
        fingerprint = S.fp_base, font_nextid = font.nextid() }
  log("ready")
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
    local req_id, ctx_id, len = line:match("^C (%d+) (%d+) (%d+)$")
    if not req_id then
      send{ op = "error", message = "bad compile frame: " .. line }
      return
    end
    len = tonumber(len)
    local source = len > 0 and stdin:read(len) or ""
    if not source or #source ~= len then log("stdin closed mid-frame"); S.stop(); return end
    S.t_decoded = gettime()
    local hok, herr = pcall(S.begin_compile, tonumber(req_id), tonumber(ctx_id), source)
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
