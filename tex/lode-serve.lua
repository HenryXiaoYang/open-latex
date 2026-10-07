-- lode-serve.lua: persistent paragraph server running inside a live LuaLaTeX document.
-- Requests: JSON lines on stdin. Responses: u32-LE length-prefixed JSON frames on the FIFO
-- named by $LODE_RESP. See docs/PROTOCOL.md.
local S = { contexts = {}, errors = {}, requests = 0 }
local json = dofile(kpse.find_file("lode-json.lua", "lua") or "lode-json.lua")
local dl = dofile(kpse.find_file("lode-dl.lua", "lua") or "lode-dl.lua")
local dlbin = dofile(kpse.find_file("lode-dl-bin.lua", "lua") or "lode-dl-bin.lua")
local gettime = os.gettimeofday
local pack = string.pack

local resp

-- Frame: u32 length (of everything after it), u8 kind, payload.
-- kind 0: JSON object. kind 1: u32 json_len, JSON header, binary display list.
local function send(tbl)
  local s = json.encode(tbl)
  resp:write(pack("<I4B", #s + 1, 0), s)
  resp:flush()
end
local function send_result(tbl, dlbytes)
  local s = json.encode(tbl)
  resp:write(pack("<I4B", 1 + 4 + #s + #dlbytes, 1), pack("<I4", #s), s, dlbytes)
  resp:flush()
end

local function log(msg) texio.write_nl("log", "lode-serve: " .. msg) end

-- Error collection -------------------------------------------------------------------------
function S.on_error(...)
  S.errors[#S.errors + 1] = { message = status.lasterrorstring, context = status.lasterrorcontext,
                              line = tex.inputlineno }
end

-- Engine-state fingerprint ------------------------------------------------------------------
local FP_CATCODES = { 92, 123, 125, 36, 38, 35, 94, 95, 37, 126 }
function S.fingerprint()
  local fp = { tex.currentgrouplevel, tex.nest.ptr, font.current(), tex.interactionmode,
               tex.gettoks("everypar"), tex.get("hsize"), tex.get("parindent"), tex.get("language") }
  for _, c in ipairs(FP_CATCODES) do fp[#fp + 1] = tex.getcatcode(c) end
  for i = 0, 9 do fp[#fp + 1] = tex.getcount(i) end
  return table.concat(fp, "|", 1, #fp)
end

-- Context replay -------------------------------------------------------------------------------
-- Called from TeX inside the paragraph \vbox group, before any text.
-- \everypar contents that may be replayed verbatim (anything else makes the paragraph
-- background-only on the host side; the server refuses it too).
local EVERYPAR_ALLOWED = { [""] = true, ["\\leftprotrusion "] = true }

function S.apply(ctx_id)
  local ctx = S.contexts[ctx_id]
  if not ctx then return end
  for k, v in pairs(ctx.ints or {}) do tex.set(k, v) end
  for k, v in pairs(ctx.dims or {}) do tex.set(k, v) end
  for k, v in pairs(ctx.glues or {}) do tex.setglue(k, v[1] or 0, v[2] or 0, v[3] or 0, v[4] or 0, v[5] or 0) end
  if ctx.parshape and #ctx.parshape > 0 then tex.parshape = ctx.parshape end
  local ep = ctx.everypar or ""
  if EVERYPAR_ALLOWED[ep] then tex.settoks("everypar", ep) end
end

local function nfss_tokens(ctx)
  local b = ctx.begin
  local n = b and b.nfss
  if not n or not n.family then return "" end
  return string.format("\\fontencoding{%s}\\fontfamily{%s}\\fontseries{%s}\\fontshape{%s}\\fontsize{%s}{%s}\\selectfont",
    n.enc or "TU", n.family, n.series or "m", n.shape or "n", n.size or "10", n.baselineskip or "12pt")
end

-- The font selected at the server's outer level persists across requests; re-select only when
-- the context's NFSS state differs from the last one selected (saves ~0.3-0.5 ms per request).
local last_nfss = nil
-- Returns the font-selection tokens to print before the request's group (or "" when the
-- previously selected font is already the right one).
local function font_tokens(ctx)
  local toks = nfss_tokens(ctx)
  if toks == last_nfss then return "" end
  last_nfss = toks
  return toks
end

-- Compile -----------------------------------------------------------------------------------------
-- A request is served in two halves without any nested TeX main loop (tex.runtoks leaks one
-- input level per call when invoked from a \directlua that never returns, E14):
--   step()   reads the request and *prints* the tokens that build the paragraph box, ending with
--            \directlua{lode_serve.finish()}; then returns to TeX, which executes them;
--   finish() traverses the box, checks the state fingerprint and sends the result.
-- The TeX side runs `\loop\directlua{lode_serve.step()}\ifnum\lodecontinue>0 \repeat`.
S.current = nil

function S.begin_compile(req)
  local ctx = S.contexts[req.ctx]
  if not ctx then
    send_result({ op = "result", req = req.req, status = "error", errors = { { message = "unknown context " .. tostring(req.ctx) } },
                  lines = 0, glyphs = 0, t_tex_us = 0, t_traverse_us = 0, t_pack_us = 0, dl_bytes = 0 }, "")
    return
  end
  if not EVERYPAR_ALLOWED[ctx.everypar or ""] then
    send_result({ op = "result", req = req.req, status = "error", errors = { { message = "context has a non-replayable \\everypar" } },
                  lines = 0, glyphs = 0, t_tex_us = 0, t_traverse_us = 0, t_pack_us = 0, dl_bytes = 0 }, "")
    return
  end
  S.errors = {}
  local tf0 = gettime()
  local ftoks = font_tokens(ctx)
  local tf1 = gettime()
  local lines = {}
  for line in (req.source .. "\n"):gmatch("(.-)\n") do lines[#lines + 1] = line end
  if #lines > 0 and lines[#lines] == "" then lines[#lines] = nil end
  S.current = { req = req, ctx = ctx, t_font = tf1 - tf0, font_changed = ftoks ~= "", t0 = gettime(), fp0 = nil }
  -- the fingerprint is taken after the font selection has executed, at the start of the group
  tex.print(ftoks .. "\\directlua{lode_serve.mark()}\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\directlua{lode_serve.apply(" .. req.ctx .. ")}")
  tex.print(lines)
  tex.print("\\par\\egroup\\endgroup\\directlua{lode_serve.finish()}")
end

function S.mark()
  if S.current then
    S.current.fp0 = S.fingerprint()
    S.current.t0 = gettime()
  end
end

function S.finish()
  local cur = S.current
  S.current = nil
  if not cur then return end
  local req, ctx = cur.req, cur.ctx
  local t1 = gettime()
  local fp1 = S.fingerprint()
  if cur.fp0 and fp1 ~= cur.fp0 then
    send{ op = "fatal", req = req.req, reason = "state_mismatch", before = cur.fp0, after = fp1, errors = S.errors }
    resp:flush()
    os.exit(3)
  end
  local box = tex.box[S.boxnum]
  local result, t2
  if box then
    local color = ctx.begin and ctx.begin.color
    if color == "0 g 0 G" then color = nil end
    result = dl.paragraph(box, color)
    t2 = gettime()
  else
    t2 = t1
  end
  local t3 = gettime()
  local bytes = result and dlbin.encode(result) or ""
  local t4 = gettime()
  local payload = {
    op = "result", req = req.req, ctx = req.ctx,
    status = (#S.errors > 0) and "error" or ((result and next(result.flags)) and "ok_degraded" or "ok"),
    errors = S.errors,
    lines = result and #result.lines or 0, glyphs = result and result.glyphs or 0,
    width = result and result.width, height = result and result.height, depth = result and result.depth,
    t_tex_us = math.floor((t1 - cur.t0) * 1e6 + 0.5),
    t_traverse_us = math.floor((t2 - t1) * 1e6 + 0.5),
    t_pack_us = math.floor((t4 - t3) * 1e6 + 0.5),
    t_font_us = math.floor(cur.t_font * 1e6 + 0.5),
    font_changed = cur.font_changed,
    dl_bytes = #bytes,
  }
  send_result(payload, bytes)
  S.requests = S.requests + 1
end

-- Profile the replay overhead (diagnostic only; uses tex.runtoks, which leaks one input level per
-- call, so do not run it thousands of times in a long-lived server).
function S.profile(req)
  local ctx = S.contexts[req.ctx]
  local n = req.n or 50
  local function med(t) table.sort(t); return math.floor(t[(#t + 1) // 2] * 1e6 + 0.5) end
  local ptrs = {}
  local function timeit(f)
    local r = {}
    local p0 = status.input_ptr
    for _ = 1, n do local a = gettime(); f(); r[#r + 1] = gettime() - a end
    ptrs[#ptrs + 1] = string.format("%d->%d", p0, status.input_ptr)
    return med(r)
  end
  local lines = {}
  for line in ((req.source or "") .. "\n"):gmatch("(.-)\n") do lines[#lines + 1] = line end
  local out = {}
  out.empty_runtoks = timeit(function() tex.runtoks(function() tex.print("\\relax") end) end)
  out.apply_only = timeit(function() S.apply(req.ctx) end)
  out.fingerprint = timeit(function() S.fingerprint() end)
  out.group_box_empty = timeit(function()
    tex.runtoks(function() tex.print("\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\directlua{lode_serve.apply(" .. req.ctx .. ")}\\par\\egroup\\endgroup") end)
  end)
  out.print_source_only = timeit(function()
    tex.runtoks(function() tex.print("\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup") tex.print(lines) tex.print("\\par\\egroup\\endgroup") end)
  end)
  out.full_compile = timeit(function()
    tex.runtoks(function()
      tex.print("\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\directlua{lode_serve.apply(" .. req.ctx .. ")}")
      tex.print(lines)
      tex.print("\\par\\egroup\\endgroup")
    end)
  end)
  out.traverse = timeit(function() dl.paragraph(tex.box[S.boxnum]) end)
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
    S.contexts[req.id] = req.ctx
    send{ op = "ok", id = req.id }
  elseif req.op == "compile" then
    S.begin_compile(req)
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
-- init(boxnum, countnum): open the FIFO, register hooks, send ready.
-- step(): read and dispatch ONE request, then return to TeX (which executes any printed tokens
-- and re-enters step() through the \loop in the driver). Sets \count<countnum> to 0 to stop.
function S.init(boxnum, countnum)
  S.boxnum = boxnum
  S.countnum = countnum
  local path = os.getenv("LODE_RESP")
  resp = assert(io.open(path, "wb"), "cannot open response FIFO " .. tostring(path))
  resp:setvbuf("full", 1 << 16)
  luatexbase.add_to_callback("show_error_hook", S.on_error, "lode-serve")
  send{ op = "ready", banner = status.banner, luatex_version = status.luatex_version,
        fingerprint = S.fingerprint(), font_nextid = font.nextid() }
  log("ready")
end

function S.stop()
  tex.setcount(S.countnum, 0)
  if resp then resp:close() end
end

function S.step()
  local line = io.stdin:read("*l")
  if not line then log("stdin closed"); S.stop(); return end
  if line == "" then return end
  local ok, req = pcall(json.decode, line)
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
        send_result({ op = "result", req = req.req, ctx = req.ctx, status = "error",
                      errors = { { message = "lua: " .. tostring(herr) } }, lines = 0, glyphs = 0,
                      t_tex_us = 0, t_traverse_us = 0, t_pack_us = 0, dl_bytes = 0 }, "")
      else
        send{ op = "error", message = "lua: " .. tostring(herr) }
      end
    end
  end
end

return S
