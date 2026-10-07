-- E14: which tex.runtoks form leaves an input level behind?
local function ptr() return status.input_ptr end
local src = "Some words here with \\emph{x} and $y^2$."
local function report(name, f, n)
  local p0 = ptr()
  for _ = 1, n do f() end
  texio.write_nl("term and log", string.format("E14 %-28s input_ptr %d -> %d (%d calls)", name, p0, ptr(), n))
end
report("runtoks(fn tex.print)", function() tex.runtoks(function() tex.print("\\setbox0\\vbox{" .. src .. "\\par}") end) end, 200)
report("runtoks(fn tex.sprint)", function() tex.runtoks(function() tex.sprint("\\setbox0\\vbox{" .. src .. "\\par}") end) end, 200)
report("runtoks(fn, grouped=true)", function() tex.runtoks(function() tex.print("\\setbox0\\vbox{" .. src .. "\\par}") end, true) end, 200)
tex.toks[255] = "\\setbox0\\vbox{" .. src .. "\\par}"
report("runtoks(toks register)", function() tex.runtoks(255) end, 200)
report("settoks+runtoks(register)", function() tex.toks[255] = "\\setbox0\\vbox{" .. src .. "\\par}"; tex.runtoks(255) end, 200)
report("runtoks(macro cs)", function() tex.runtoks("lodeRunMacro") end, 200)
report("quittoks after print", function() tex.runtoks(function() tex.print("\\setbox0\\vbox{" .. src .. "\\par}") tex.quittoks() end) end, 200)
