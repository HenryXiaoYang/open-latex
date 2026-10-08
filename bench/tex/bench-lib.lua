-- bench-lib.lua: shared timing/statistics helpers replicating the methodology of
-- texlode/luatex-benchmark (paper: "Real-Time LuaTeX", Tables 1-2).
--   * each sample = mean over INNER consecutive in-session compiles (default 100)
--   * the first WARMUP samples are discarded (default 5), SAMPLES kept (default 30)
--   * percentile index = floor(n * frac) + 1 on the sorted samples
local M = {}
local gettime = os.gettimeofday

M.INNER   = tonumber(os.getenv("RTEX_BENCH_INNER"))   or 100
M.WARMUP  = tonumber(os.getenv("RTEX_BENCH_WARMUP"))  or 5
M.SAMPLES = tonumber(os.getenv("RTEX_BENCH_SAMPLES")) or 30

local t0 = 0
local results = {}   -- name -> list of per-sample ms (incl. warmup)
local singles = {}   -- name -> list of individual compile times (ms)

function M.start() t0 = gettime() end

-- Record one sample; `inner` compiles happened between start() and stop().
function M.stop(name, inner)
  local ms = (gettime() - t0) * 1000 / inner
  results[name] = results[name] or {}
  local r = results[name]
  r[#r + 1] = ms
end

-- Individual (non-amortized) single-compile timing.
function M.single_stop(name)
  local ms = (gettime() - t0) * 1000
  singles[name] = singles[name] or {}
  local s = singles[name]
  s[#s + 1] = ms
end

local function at(sorted, frac)
  local n = #sorted
  local i = math.floor(n * frac) + 1
  if i > n then i = n end
  return sorted[i]
end

function M.stats(list, warmup)
  local t = {}
  for i = (warmup or 0) + 1, #list do t[#t + 1] = list[i] end
  table.sort(t)
  if #t == 0 then return nil end
  return { median = at(t, 0.5), p5 = at(t, 0.05), p95 = at(t, 0.95), n = #t,
           min = t[1], max = t[#t] }
end

local out_lines = {}
local function emit(s)
  texio.write_nl("term and log", s)
  out_lines[#out_lines + 1] = s
end

function M.report(name, warmup)
  local s = M.stats(results[name] or {}, warmup or M.WARMUP)
  if not s then emit(string.format("%-14s (no samples)", name)) return end
  emit(string.format("%-14s median=%7.3f ms P5=%7.3f P95=%7.3f (n=%d)",
    name, s.median, s.p5, s.p95, s.n))
end

function M.report_singles(name)
  local s = M.stats(singles[name] or {}, 0)
  if not s then return end
  emit(string.format("%-14s single-edit median=%7.3f ms P95=%7.3f max=%7.3f (n=%d)",
    name, s.median, s.p95, s.max, s.n))
end

-- Stability windows over the raw sample list (paper: 1-50, 226-275, 451-500 compiles).
function M.window_report(name, windows)
  local r = results[name] or {}
  local parts = {}
  for _, w in ipairs(windows) do
    local acc, n = 0, 0
    for i = w[1], math.min(w[2], #r) do acc = acc + r[i]; n = n + 1 end
    parts[#parts + 1] = string.format("%d-%d: %.3f ms", w[1], w[2], n > 0 and acc / n or 0/0)
  end
  emit(string.format("%-14s %s", name, table.concat(parts, "  ")))
end

-- Write machine-readable CSV next to the job.
function M.write_csv(path)
  path = os.getenv("RTEX_BENCH_CSV") or path
  local f = assert(io.open(path, "w"))
  f:write("category,kind,median_ms,p5_ms,p95_ms,n\n")
  local names = {}
  for k in pairs(results) do names[#names + 1] = k end
  table.sort(names)
  for _, k in ipairs(names) do
    local s = M.stats(results[k], M.WARMUP)
    if s then f:write(string.format("%s,amortized,%.4f,%.4f,%.4f,%d\n", k, s.median, s.p5, s.p95, s.n)) end
    local ss = M.stats(singles[k] or {}, 0)
    if ss then f:write(string.format("%s,single,%.4f,%.4f,%.4f,%d\n", k, ss.median, ss.p5, ss.p95, ss.n)) end
  end
  f:close()
end

function M.engine_info()
  emit(string.format("engine=%s luatex=%s lua=%s inner=%d warmup=%d samples=%d",
    status.banner or "?", tostring(status.luatex_version), _VERSION, M.INNER, M.WARMUP, M.SAMPLES))
end


-- Resource snapshot (font ids, node memory) for stability runs.
function M.resources(tag)
  emit(string.format("%-14s font.nextid=%d node_mem=%s", tag, font.nextid(), tostring(status.node_mem_usage)))
end

return M
