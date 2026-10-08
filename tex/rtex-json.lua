-- rtex-json.lua: minimal JSON encoder/decoder (Lua 5.3, LuaTeX). No external deps.
local M = {}

local escapes = { ['"'] = '\\"', ['\\'] = '\\\\', ['\b'] = '\\b', ['\f'] = '\\f',
                  ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t' }
local function esc(s)
  return (s:gsub('[%c"\\]', function(c) return escapes[c] or string.format('\\u%04x', c:byte()) end))
end

local function is_array(t)
  local n = #t
  if n == 0 then return next(t) == nil end
  for k in pairs(t) do
    if math.type(k) ~= "integer" or k < 1 or k > n then return false end
  end
  return true
end

local encode
local function encode_value(v, out)
  local tv = type(v)
  if tv == "nil" then out[#out + 1] = "null"
  elseif tv == "boolean" then out[#out + 1] = v and "true" or "false"
  elseif tv == "number" then
    if math.type(v) == "integer" then out[#out + 1] = string.format("%d", v)
    elseif v ~= v or v == math.huge or v == -math.huge then out[#out + 1] = "null"
    else out[#out + 1] = string.format("%.17g", v) end
  elseif tv == "string" then out[#out + 1] = '"' .. esc(v) .. '"'
  elseif tv == "table" then
    if is_array(v) then
      out[#out + 1] = "["
      for i = 1, #v do
        if i > 1 then out[#out + 1] = "," end
        encode_value(v[i], out)
      end
      out[#out + 1] = "]"
    else
      out[#out + 1] = "{"
      local keys = {}
      for k in pairs(v) do keys[#keys + 1] = tostring(k) end
      table.sort(keys)
      for i, k in ipairs(keys) do
        if i > 1 then out[#out + 1] = "," end
        out[#out + 1] = '"' .. esc(k) .. '":'
        encode_value(v[k] == nil and v[tonumber(k)] or v[k], out)
      end
      out[#out + 1] = "}"
    end
  else
    out[#out + 1] = '"' .. esc(tostring(v)) .. '"'
  end
end
function M.encode(v)
  local out = {}
  encode_value(v, out)
  return table.concat(out)
end

-- Decoder -------------------------------------------------------------------
local function decode_error(s, i, msg) error(string.format("json: %s at %d: %s", msg, i, s:sub(i, i + 20))) end
local decode_value
local function skip(s, i) return s:find("%S", i) or #s + 1 end
local function decode_string(s, i)
  local j = i + 1
  local buf = {}
  while true do
    local c = s:sub(j, j)
    if c == "" then decode_error(s, i, "unterminated string") end
    if c == '"' then return table.concat(buf), j + 1 end
    if c == "\\" then
      local e = s:sub(j + 1, j + 1)
      local map = { ['"'] = '"', ['\\'] = '\\', ['/'] = '/', b = '\b', f = '\f', n = '\n', r = '\r', t = '\t' }
      if map[e] then buf[#buf + 1] = map[e]; j = j + 2
      elseif e == "u" then
        local hex = s:sub(j + 2, j + 5)
        local cp = tonumber(hex, 16)
        j = j + 6
        if cp >= 0xD800 and cp <= 0xDBFF and s:sub(j, j + 1) == "\\u" then
          local lo = tonumber(s:sub(j + 2, j + 5), 16)
          cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00)
          j = j + 6
        end
        buf[#buf + 1] = utf8.char(cp)
      else decode_error(s, j, "bad escape") end
    else
      local k = s:find('["\\]', j) or (#s + 1)
      buf[#buf + 1] = s:sub(j, k - 1)
      j = k
    end
  end
end
function decode_value(s, i)
  i = skip(s, i)
  local c = s:sub(i, i)
  if c == "{" then
    local obj = {}
    i = skip(s, i + 1)
    if s:sub(i, i) == "}" then return obj, i + 1 end
    while true do
      i = skip(s, i)
      if s:sub(i, i) ~= '"' then decode_error(s, i, "expected key") end
      local k; k, i = decode_string(s, i)
      i = skip(s, i)
      if s:sub(i, i) ~= ":" then decode_error(s, i, "expected :") end
      local v; v, i = decode_value(s, i + 1)
      obj[k] = v
      i = skip(s, i)
      local d = s:sub(i, i)
      if d == "," then i = i + 1 elseif d == "}" then return obj, i + 1 else decode_error(s, i, "expected , or }") end
    end
  elseif c == "[" then
    local arr = {}
    i = skip(s, i + 1)
    if s:sub(i, i) == "]" then return arr, i + 1 end
    while true do
      local v; v, i = decode_value(s, i)
      arr[#arr + 1] = v
      i = skip(s, i)
      local d = s:sub(i, i)
      if d == "," then i = i + 1 elseif d == "]" then return arr, i + 1 else decode_error(s, i, "expected , or ]") end
    end
  elseif c == '"' then return decode_string(s, i)
  elseif s:sub(i, i + 3) == "true" then return true, i + 4
  elseif s:sub(i, i + 4) == "false" then return false, i + 5
  elseif s:sub(i, i + 3) == "null" then return nil, i + 4
  else
    local num = s:match("^-?%d+%.?%d*[eE]?[-+]?%d*", i)
    if not num or num == "" then decode_error(s, i, "unexpected token") end
    local v = math.tointeger(tonumber(num)) or tonumber(num)
    return v, i + #num
  end
end
function M.decode(s)
  local v = decode_value(s, 1)
  return v
end

return M
