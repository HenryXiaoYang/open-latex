-- E2: para/begin fires once per body paragraph and pairs with pre_linebreak_filter.
E2 = { begins = 0, prelbs = 0, stack = {}, pairs_ok = 0, pairs_bad = 0, groupcodes = {} }
function E2.parbegin()
  E2.begins = E2.begins + 1
  E2.stack[#E2.stack + 1] = { seq = E2.begins, line = tex.inputlineno, nest = tex.nest.ptr,
    font = font.current(), mode = tex.nest[tex.nest.ptr].mode }
end
luatexbase.add_to_callback("pre_linebreak_filter", function(head, groupcode)
  E2.prelbs = E2.prelbs + 1
  E2.groupcodes[groupcode] = (E2.groupcodes[groupcode] or 0) + 1
  local top = E2.stack[#E2.stack]
  if top and top.nest == tex.nest.ptr then
    E2.pairs_ok = E2.pairs_ok + 1
    texio.write_nl("log", string.format("E2 pair seq=%d start=%d end=%d groupcode=%q nest=%d",
      top.seq, top.line, tex.inputlineno, groupcode, tex.nest.ptr))
    E2.stack[#E2.stack] = nil
  else
    E2.pairs_bad = E2.pairs_bad + 1
    texio.write_nl("log", string.format("E2 UNPAIRED prelb groupcode=%q nest=%d topnest=%s line=%d",
      groupcode, tex.nest.ptr, top and tostring(top.nest) or "nil", tex.inputlineno))
  end
  return true
end, "e2")
function E2.report()
  local gc = {}
  for k, v in pairs(E2.groupcodes) do gc[#gc + 1] = string.format("%s=%d", k == "" and "<main>" or k, v) end
  table.sort(gc)
  texio.write_nl("term and log", string.format("E2 begins=%d prelbs=%d pairs_ok=%d pairs_bad=%d leftover=%d groupcodes: %s",
    E2.begins, E2.prelbs, E2.pairs_ok, E2.pairs_bad, #E2.stack, table.concat(gc, " ")))
end
