-- Headless smoke of the DynamicEvents addon (280): loads the real addon file under a minimal
-- WoW-API stub and drives BOTH delivery routes (CHAT_MSG_ADDON + the whisper shim) through the
-- full command set, asserting the observable effects (chat lines, center toasts, shim
-- swallowing). Logic/parse coverage only — rendering stays the live eyeball.
-- Run: lua5.1 tools/wire-client/dynevents-addon-check.lua [path-to-DynamicEvents.lua]

local chats, toasts, frames = {}, {}, {}

local function mkframe()
  local f = { scripts = {} }
  setmetatable(f, {
    __index = function(t, k)
      local fn
      if k == "CreateFontString" or k == "CreateTexture" then
        fn = function() return mkframe() end
      elseif k == "GetWidth" or k == "GetHeight" then
        fn = function() return 668 end
      elseif k == "GetFrameLevel" then
        fn = function() return 1 end
      elseif k == "SetScript" then
        fn = function(self, name, h) t.scripts[name] = h end
      elseif k == "IsVisible" or k == "IsShown" then
        fn = function() return t.__shown end
      elseif k == "Show" then
        fn = function() t.__shown = true end
      elseif k == "Hide" then
        fn = function() t.__shown = false end
      else
        fn = function() end
      end
      rawset(t, k, fn)
      return fn
    end,
  })
  table.insert(frames, f)
  return f
end

CreateFrame = function() return mkframe() end
UIParent = mkframe()
WorldMapButton = mkframe()
WorldMapDetailFrame = mkframe()
UIErrorsFrame = { AddMessage = function(self, msg) table.insert(toasts, msg) end }
DEFAULT_CHAT_FRAME = { AddMessage = function(self, msg) table.insert(chats, msg) end }
GetTime = function() return 100 end
GetMapInfo = function() return "Elwynn" end
orig_chatframe_calls = 0
ChatFrame_OnEvent = function() orig_chatframe_calls = orig_chatframe_calls + 1 end
-- lua5.1 shims for the vanilla (5.0) API surface the addon uses
math.mod = math.mod or function(a, b) return a % b end
table.getn = table.getn or function(t) return #t end

dofile(arg[1] or "packages/dynamic_events/client/addons/DynamicEvents/DynamicEvents.lua")

local function fire(ev, a1, a2)
  event, arg1, arg2 = ev, a1, a2
  for _, f in ipairs(frames) do
    if f.scripts.OnEvent then f.scripts.OnEvent() end
  end
end

local failures = 0
local function expect(cond, what)
  if cond then
    print("OK: " .. what)
  else
    failures = failures + 1
    print("FAIL: " .. what)
  end
end
local function chat_has(s)
  for _, m in ipairs(chats) do
    if string.find(m, s, 1, true) then return true end
  end
  return false
end

-- 1. event.start via the CHAT_MSG_ADDON route
fire("CHAT_MSG_ADDON", "STC", "v1|event.start|0|1/1|1001|Clear Fargodeep Mine|0|0|10|240|1|-9758.0|191.7|75.0|1")
expect(chat_has("Clear Fargodeep Mine") and chat_has("has begun"), "event.start announces")

-- 2. event.state update parses (progress moves; name with spaces survives the pipe split)
fire("CHAT_MSG_ADDON", "STC", "v1|event.state|1|1/1|1001|Clear Fargodeep Mine|0|3|10|180|2|-9758.0|191.7|75.0|1")
expect(true, "event.state update handled without error")

-- 3. event.you medal preview
fire("CHAT_MSG_ADDON", "STC", "v1|event.you|2|1/1|1001|612|2")
expect(chat_has("Gold") and chat_has("contribution"), "event.you announces the medal tier")

-- 4. the WHISPER route: swallowed (original chat handler NOT called) and processed
local before = orig_chatframe_calls
fire_env = "STC\tv1|event.state|3|1/1|1001|Clear Fargodeep Mine|0|7|10|120|2|-9758.0|191.7|75.0|1"
event, arg1 = "CHAT_MSG_WHISPER", fire_env
ChatFrame_OnEvent("CHAT_MSG_WHISPER")
expect(orig_chatframe_calls == before, "whisper shim swallows STC traffic")
event, arg1 = "CHAT_MSG_WHISPER", "hi there"
ChatFrame_OnEvent("CHAT_MSG_WHISPER")
expect(orig_chatframe_calls == before + 1, "whisper shim passes normal whispers through")

-- 5. event.reward toast
fire("CHAT_MSG_ADDON", "STC", "v1|event.reward|4|1/1|1001|2|350|1000")
expect(chat_has("+350 XP"), "event.reward chat line carries the XP")
expect(table.getn(toasts) > 0 and string.find(toasts[1], "Event reward", 1, true), "event.reward center toast fires")

-- 6. event.end removes + announces
fire("CHAT_MSG_ADDON", "STC", "v1|event.end|5|1/1|1001|Clear Fargodeep Mine|1")
expect(chat_has("succeeded"), "event.end announces the outcome")

if failures == 0 then
  print("[addon-check] PASS")
  os.exit(0)
else
  print("[addon-check] FAIL (" .. failures .. ")")
  os.exit(1)
end
