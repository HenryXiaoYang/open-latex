-- E3: which error/warning callbacks fire, and what status reports afterwards.
E3 = {}
for _, cb in ipairs{"show_error_hook", "show_error_message", "show_warning_message", "show_lua_error_hook"} do
  local ok, err = pcall(function()
    luatexbase.add_to_callback(cb, function(...)
      E3[cb] = (E3[cb] or 0) + 1
      texio.write_nl("log", string.format("E3 callback %s fired: lasterror=%s ctx=%s line=%s",
        cb, tostring(status.lasterrorstring), tostring(status.lasterrorcontext), tostring(tex.inputlineno)))
    end, "e3")
  end)
  texio.write_nl("term and log", string.format("E3 register %s -> %s %s", cb, tostring(ok), tostring(err)))
end
function E3.state(tag)
  texio.write_nl("term and log", string.format("E3 %s: lasterrorstring=%q grouplevel=%d nest=%d interaction=%d",
    tag, tostring(status.lasterrorstring), tex.currentgrouplevel, tex.nest.ptr, tex.interactionmode))
end
function E3.report()
  local parts = {}
  for k, v in pairs(E3) do if type(v) == "number" then parts[#parts + 1] = k .. "=" .. v end end
  table.sort(parts)
  texio.write_nl("term and log", "E3 fired: " .. table.concat(parts, " "))
end
