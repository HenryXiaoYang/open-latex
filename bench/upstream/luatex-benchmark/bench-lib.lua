-- Shared statistics + reporting helpers for the LuaLaTeX benchmarks.
--
-- Kept in a real .lua file (loaded via \directlua{dofile(...)}) so that the
-- '%' in format strings and the '#' length operator are NOT interpreted by
-- TeX (where '%' starts a comment and '#' is a macro parameter). This is the
-- robust way to embed non-trivial Lua in a LaTeX document.

results = {}   -- category name -> { median, p5, p95, n }
_BT = {}       -- current accumulator of per-compile times (ms)

local function measured(t, w)
  local m = {}
  for i = w + 1, #t do m[#m + 1] = t[i] end
  table.sort(m)
  return m
end

-- Percentile by the same index rule as the internal harness:
--   value at 0-indexed floor(n*frac)  ->  1-indexed floor(n*frac)+1
local function at(s, frac)
  local n = #s
  if n == 0 then return 0 end
  local idx = math.floor(n * frac) + 1
  if idx < 1 then idx = 1 elseif idx > n then idx = n end
  return s[idx]
end

function report(name, w)
  local s = measured(_BT, w)
  results[name] = { median = at(s, 0.5), p5 = at(s, 0.05), p95 = at(s, 0.95), n = #s }
  local r = results[name]
  texio.write_nl(string.format(
    "%-14s  median=%7.3f ms   P5=%7.3f   P95=%7.3f   (n=%d)",
    name, r.median, r.p5, r.p95, r.n))
end

local function window_median(t, lo, hi)
  local s = {}
  for i = lo, hi do s[#s + 1] = t[i] end
  table.sort(s)
  local n = #s
  if n == 0 then return 0 end
  local mid = math.floor(n / 2)
  if n % 2 == 1 then return s[mid + 1] else return (s[mid] + s[mid + 1]) / 2 end
end

function window_report(name, t)
  texio.write_nl(string.format(
    "%-8s  1-50: %7.3f ms   225-275: %7.3f ms   451-500: %7.3f ms",
    name, window_median(t, 1, 50), window_median(t, 225, 275), window_median(t, 451, 500)))
end

-- Amortized stability report: SW/MW each hold three window means (ms).
function stability_report(SW, MW)
  texio.write_nl("==== stability over 500 in-session compiles (amortized 50-compile windows) ====")
  texio.write_nl(string.format(
    "Short      1-50: %6.3f ms   226-275: %6.3f ms   451-500: %6.3f ms", SW[1], SW[2], SW[3]))
  texio.write_nl(string.format(
    "Multi-line 1-50: %6.3f ms   226-275: %6.3f ms   451-500: %6.3f ms", MW[1], MW[2], MW[3]))
end
