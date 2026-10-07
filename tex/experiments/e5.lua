-- E5: FIFO for responses + stdin for requests from inside lualatex.
local path = os.getenv("LODE_RESP")
local ok, f = pcall(io.open, path, "wb")
texio.write_nl("term and log", "E5 openout_any=" .. tostring(kpse.var_value("openout_any")) ..
  " fifo_open=" .. tostring(ok and f ~= nil) .. " path=" .. tostring(path))
if f then
  f:setvbuf("full", 65536)
  local t0 = os.gettimeofday()
  for i = 1, 1000 do f:write(string.pack("<I4", i)); f:flush() end
  local dt = (os.gettimeofday() - t0) * 1e6 / 1000
  f:write("DONE\n"); f:flush(); f:close()
  texio.write_nl("term and log", string.format("E5 1000 framed writes+flush: %.2f us each", dt))
end
local line = io.stdin:read("*l")
texio.write_nl("term and log", "E5 stdin line=" .. tostring(line))
