-- SPDX-FileCopyrightText: 2026 Broadcaster contributors
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Execute the real private remote Lua, including ASS-event overlay lifecycle.
local keys, commands, screen, timer = {}, {}, "", nil
local now, pause, timepos, duration, speed = 100, false, 45, 180, 1
local title, sid, aid, fullscreen = "Japan: Die Todesstrafe", "no", "1", false
local updates, removals = 0, 0
local tracks = {
    {type="audio",id=1,lang="de",title="Original",selected=true},
    {type="audio",id=2,lang="en",title="English",selected=false},
    {type="sub",id=3,lang="en",title="English CC",selected=false},
    {type="sub",id=4,lang="de",title="German",selected=false},
}
mp = {
    get_time=function() return now end,
    get_osd_size=function() return 1920,1080 end,
    get_property=function(key, default)
        if key == "media-title" then return title end
        if key == "sid" then return sid end
        if key == "aid" then return aid end
        return default
    end,
    get_property_bool=function(key, default)
        if key == "pause" then return pause end
        return default
    end,
    get_property_number=function(key, default)
        if key == "time-pos" then return timepos end
        if key == "duration" then return duration end
        if key == "speed" then return speed end
        return default
    end,
    get_property_native=function(key, default)
        if key == "track-list" then return tracks end
        return default
    end,
    create_osd_overlay=function(format)
        assert(format == "ass-events")
        local overlay = {res_x=1280,res_y=720,data=""}
        function overlay:update()
            assert(self.res_x == 1280 and self.res_y == 720)
            screen=self.data; updates=updates+1
        end
        function overlay:remove() screen="";removals=removals+1 end
        return overlay
    end,
    commandv=function(...)
        local cmd={...};commands[#commands+1]=cmd
        if cmd[1] == "cycle" and cmd[2] == "pause" then pause=not pause end
        if cmd[1] == "cycle" and cmd[2] == "fullscreen" then fullscreen=not fullscreen end
        if cmd[1] == "set" and cmd[2] == "pause" then pause=cmd[3]=="yes" end
        if cmd[1] == "set" and cmd[2] == "sid" then sid=cmd[3] end
        if cmd[1] == "set" and cmd[2] == "aid" then aid=cmd[3] end
        if cmd[1] == "set" and cmd[2] == "speed" then speed=tonumber(cmd[3]) end
    end,
    add_forced_key_binding=function(key,name,callback)
        assert(not keys[key],"duplicate "..key);keys[key]=callback
    end,
    add_periodic_timer=function(interval,callback)
        assert(interval > 0);timer=callback
    end,
}
dofile("data/broadcaster-mpv-remote.lua")
local function press(key)
    assert(keys[key],"missing binding "..key); keys[key]()
end
local function last(command,second)
    local args=commands[#commands]
    assert(args and args[1]==command and (not second or args[2]==second),
        "expected "..command.." "..tostring(second))
end
local function displays(s) assert(screen:find(s,1,true), "missing "..s) end
local locale = (os.getenv("LC_ALL") or os.getenv("LC_MESSAGES") or os.getenv("LANGUAGE") or os.getenv("LANG") or "en"):lower()
local german = locale == "de" or locale:match("^de[%._@:%-]") ~= nil

assert(screen=="")
press("ENTER")
displays("Japan: Die Todesstrafe")
if german then
    displays("Ton");displays("Untertitel");displays("Vollbild")
else
    displays("Audio");displays("Subtitles");displays("Fullscreen")
end
displays("m 93") -- independent positioned vector backdrop rather than text-only UI
displays("\\p1");displays("\\pos(0,0)")
local before = updates
timer(); assert(updates==before,"unchanged frame should not be redrawn")
press("ENTER");last("cycle","pause");assert(pause)
press("KP_ENTER");assert(not pause)
press("RIGHT");press("ENTER");last("seek",10)
press("LEFT");press("LEFT");press("ENTER");last("seek",-10)
press("UP");press("RIGHT");last("seek",10) -- focus timeline
press("DOWN")
-- From Play/Pause, down selects Subtitles, left selects Audio.
press("DOWN");press("LEFT");press("ENTER")
displays(german and "Tonspuren" or "Audio tracks")
displays(german and "Tonspur 1" or "Audio track 1")
displays(german and "Englisch" or "English")
press("DOWN");press("ENTER");last("set","aid");assert(aid=="2")
press("RIGHT");press("ENTER")
displays(german and "Untertitel" or "Subtitles")
displays(german and "Untertitelspur 1" or "Subtitle track 1")
press("DOWN");press("ENTER");last("set","sid");assert(sid=="3")
press("RIGHT");press("ENTER")
displays(german and "Wiedergabegeschwindigkeit" or "Playback speed")
press("DOWN");press("ENTER");last("set","speed");assert(speed==1.25)
press("RIGHT");press("ENTER");assert(fullscreen)
-- Unwind submenus without terminating playback.
press("LEFT");press("ENTER");displays(german and "Wiedergabegeschwindigkeit" or "Playback speed")
press("GO_BACK");assert(screen~="")
press("GO_BACK");assert(screen=="")
press("GO_BACK");last("quit")
press("LEFT");last("seek",-10)
press("UP");last("seek",60)
press("MENU");assert(screen~="")
press("MENU");assert(screen=="")
press("ENTER");now=now+13;timer()
assert(screen=="" and removals>=2,"idle overlay must be removed")
press("ENTER");press("PLAYPAUSE");assert(pause)
now=now+13;timer();assert(screen~="","paused overlay should remain")
press("GO_HOME");last("quit")
press("STOP");last("quit")
duration=0;press("MENU");press("ENTER")
displays("LIVE")
press("UP");press("RIGHT") -- seek on live stream must be ignored

-- Real public-service streams can expose an audio track without language or
-- title metadata, and no selectable subtitle tracks at all. Never guess
-- "German" from the broadcaster; give a numbered, honest fallback instead.
tracks = {
    {type="audio",id=7,codec="aac",lang="",title="",selected=true},
    {type="audio",id=9,lang="es-ES",title="Alternate",selected=false},
}
press("DOWN");press("DOWN");press("LEFT");press("ENTER")
displays(german and "Tonspur 1" or "Audio track 1")
displays(german and "Sprache nicht angegeben" or "Language not provided")
displays("aac")
displays(german and "Spanisch" or "Spanish")
press("DOWN");press("ENTER");last("set","aid");assert(aid=="9")

press("RIGHT");press("ENTER")
displays(german and "Untertitelspur" or "Subtitles")
displays(german and "Keine Untertitelspuren verfügbar" or "No subtitle tracks available")
displays(german and "Aus" or "Off")
-- Even with no subtitle streams the Off item remains safe and selectable.
press("ENTER");last("set","sid");assert(sid=="no")

print("MPV graphical remote controls, metadata fallbacks and lifecycle PASS")
