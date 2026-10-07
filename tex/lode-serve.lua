-- lode-serve.lua: persistent paragraph server running inside a live LuaLaTeX document.
-- Requests: JSON lines on stdin. Responses: u32-LE length-prefixed JSON frames on the FIFO
-- named by $LODE_RESP. See docs/PROTOCOL.md.
local S = { contexts = {}, errors = {}, requests = 0 }
local json = dofile(kpse.find_file("lode-json.lua", "lua") or "lode-json.lua")
local dl = dofile(kpse.find_file("lode-dl.lua", "lua") or "lode-dl.lua")
local gettime = os.gettimeofday
local pack = string.pack

local resp

local function send(tbl)
  local s = json.encode(tbl)
  resp:write(pack("<I4", #s), s)
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
function S.apply(ctx_id)
  local ctx = S.contexts[ctx_id]
  if not ctx then return end
  for k, v in pairs(ctx.ints or {}) do tex.set(k, v) end
  for k, v in pairs(ctx.dims or {}) do tex.set(k, v) end
  for k, v in pairs(ctx.glues or {}) do tex.setglue(k, v[1] or 0, v[2] or 0, v[3] or 0, v[4] or 0, v[5] or 0) end
  if ctx.parshape and #ctx.parshape > 0 then tex.parshape = ctx.parshape end
end

local function nfss_tokens(ctx)
  local b = ctx.begin
  local n = b and b.nfss
  if not n or not n.family then return "" end
  return string.format("\\fontencoding{%s}\\fontfamily{%s}\\fontseries{%s}\\fontshape{%s}\\fontsize{%s}{%s}\\selectfont",
    n.enc or "TU", n.family, n.series or "m", n.shape or "n", n.size or "10", n.baselineskip or "12pt")
end

-- Compile ----------------------------------------------------------------------------------------
function S.compile(req)
  local ctx = S.contexts[req.ctx]
  if not ctx then
    send{ op = "result", req = req.req, status = "error", errors = { { message = "unknown context " .. tostring(req.ctx) } } }
    return
  end
  S.errors = {}
  local fp0 = S.fingerprint()
  local lines = {}
  for line in (req.source .. "\n"):gmatch("(.-)\n") do lines[#lines + 1] = line end
  if #lines > 0 and lines[#lines] == "" then lines[#lines] = nil end
  local head = "\\begingroup\\global\\setbox\\lodebox\\vbox\\bgroup\\directlua{lode_serve.apply(" .. req.ctx .. ")}" .. nfss_tokens(ctx)
  local tail = "\\par\\egroup\\endgroup"
  local t0 = gettime()
  local ok, err = pcall(tex.runtoks, function()
    tex.print(head)
    tex.print(lines)
    tex.print(tail)
  end)
  local t1 = gettime()
  local fp1 = S.fingerprint()
  if not ok then
    S.errors[#S.errors + 1] = { message = "lua: " .. tostring(err) }
  end
  if fp1 ~= fp0 then
    send{ op = "fatal", req = req.req, reason = "state_mismatch", before = fp0, after = fp1, errors = S.errors }
    resp:flush()
    os.exit(3)
  end
  local box = tex.box[S.boxnum]
  local result, t2
  if box then
    result = dl.paragraph(box)
    t2 = gettime()
  else
    t2 = t1
  end
  local payload = {
    op = "result", req = req.req, ctx = req.ctx,
    status = (#S.errors > 0) and "error" or ((result and next(result.flags)) and "ok_degraded" or "ok"),
    errors = S.errors, dl = result,
    t_tex_us = math.floor((t1 - t0) * 1e6 + 0.5),
    t_traverse_us = math.floor((t2 - t1) * 1e6 + 0.5),
  }
  local t3 = gettime()
  local s = json.encode(payload)
  local t4 = gettime()
  -- append serialization time by patching the tail of the JSON (cheap, avoids re-encoding)
  s = s:sub(1, -2) .. string.format(',"t_pack_us":%d}', math.floor((t4 - t3) * 1e6 + 0.5))
  resp:write(pack("<I4", #s), s)
  resp:flush()
  S.requests = S.requests + 1
end

function S.stats()
  send{ op = "stats", requests = S.requests, font_nextid = font.nextid(), node_mem = status.node_mem_usage,
        grouplevel = tex.currentgrouplevel, nest = tex.nest.ptr, luastate = collectgarbage("count") }
end

-- Main loop ----------------------------------------------------------------------------------------
function S.run(boxnum)
  S.boxnum = boxnum
  local path = os.getenv("LODE_RESP")
  resp = assert(io.open(path, "wb"), "cannot open response FIFO " .. tostring(path))
  resp:setvbuf("full", 1 << 16)
  luatexbase.add_to_callback("show_error_hook", S.on_error, "lode-serve")
  send{ op = "ready", banner = status.banner, luatex_version = status.luatex_version,
        fingerprint = S.fingerprint(), font_nextid = font.nextid() }
  log("ready")
  while true do
    local line = io.stdin:read("*l")
    if not line then log("stdin closed"); break end
    if line ~= "" then
      local ok, req = pcall(json.decode, line)
      if not ok then
        send{ op = "error", message = "bad request: " .. tostring(req) }
      elseif req.op == "context" then
        S.contexts[req.id] = req.ctx
        send{ op = "ok", id = req.id }
      elseif req.op == "compile" then
        S.compile(req)
      elseif req.op == "ping" then
        send{ op = "pong" }
      elseif req.op == "stats" then
        S.stats()
      elseif req.op == "shutdown" then
        send{ op = "bye" }
        break
      else
        send{ op = "error", message = "unknown op " .. tostring(req.op) }
      end
    end
  end
  resp:close()
end

return S
