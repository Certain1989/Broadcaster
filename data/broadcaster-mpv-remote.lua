-- SPDX-FileCopyrightText: 2026 Broadcaster contributors
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Private TV controller. mpv's stock OSC stays available for mouse input.
-- ASS events give every shape and label its own position, unlike set_osd_ass.
local visible, selected, panel = false, 2, nil
local previous_selection, options, option_index = 2, {}, 1
local last_input, idle_seconds = 0, 12
local speeds = {0.5, 0.75, 1, 1.25, 1.5, 2}
-- Locale follows the parent desktop. Unsupported languages retain English.
local function current_locale()
    for _, key in ipairs({"LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"}) do
        local value = os.getenv(key)
        if value and value ~= "" then return value:lower() end
    end
    return "en"
end
local de = current_locale():match("^de[%._@:%-]") ~= nil or current_locale() == "de"
local translations = {
    ["−10 seconds"] = "−10 Sekunden", ["+10 seconds"] = "+10 Sekunden",
    ["▶  Play"] = "▶  Abspielen", ["Ⅱ  Pause"] = "Ⅱ  Pause",
    ["Audio"] = "Ton", ["Subtitles"] = "Untertitel",
    ["Speed"] = "Tempo", ["Fullscreen"] = "Vollbild", ["Exit"] = "Beenden",
    ["Audio tracks"] = "Tonspuren", ["Playback speed"] = "Wiedergabegeschwindigkeit",
    ["Back to controls"] = "Zurück zur Steuerung", ["Off"] = "Aus",
    ["Unknown"] = "Unbekannt", ["No tracks available"] = "Keine Spuren verfügbar",
    ["Audio track"] = "Tonspur", ["Subtitle track"] = "Untertitelspur",
    ["Language not provided"] = "Sprache nicht angegeben",
    ["No subtitle tracks available"] = "Keine Untertitelspuren verfügbar",
    ["German"] = "Deutsch", ["English"] = "Englisch",
    ["French"] = "Französisch", ["Spanish"] = "Spanisch",
    ["Italian"] = "Italienisch", ["Dutch"] = "Niederländisch",
    ["Portuguese"] = "Portugiesisch", ["Japanese"] = "Japanisch",
    ["LIVE"] = "LIVE",
}
local function tr(value)
    return (de and translations[value]) or value
end
local overlay = mp.create_osd_overlay("ass-events")
local last_frame = nil

local function clean(value)
    return tostring(value or ""):gsub("[{}]", " "):gsub("\\", " "):gsub("[\r\n]", " ")
end

local function short(value, count)
    value = clean(value)
    if #value <= count then return value end
    local cut = count
    -- Do not split a UTF-8 codepoint when truncating a long programme title.
    while cut > 1 and value:byte(cut + 1) and
        value:byte(cut + 1) >= 128 and value:byte(cut + 1) < 192 do
        cut = cut - 1
    end
    return value:sub(1, cut) .. "…"
end

-- mpv's track.lang and track.title are optional. Only label languages
-- supplied by the demuxer; a single untagged audio track is NOT known German.
local language_names = {
    de="German", deu="German", ger="German",
    en="English", eng="English",
    fr="French", fra="French", fre="French",
    es="Spanish", spa="Spanish",
    it="Italian", ita="Italian",
    nl="Dutch", nld="Dutch", dut="Dutch",
    pt="Portuguese", por="Portuguese",
    ja="Japanese", jpn="Japanese",
}
local function track_label(track, number, kind)
    local prefix = tr(kind == "audio" and "Audio track" or "Subtitle track")
    local code = type(track.lang) == "string" and track.lang:match("^%s*(.-)%s*$") or ""
    local basic = code:lower():match("^([a-z]+)") or ""
    local known = language_names[basic]
    local language = known and tr(known)
        or (code ~= "" and basic ~= "und" and code or tr("Language not provided"))
    local title = type(track.title) == "string" and track.title:match("^%s*(.-)%s*$") or ""
    local suffix = ""
    if title ~= "" and title:lower() ~= language:lower() and title:lower() ~= code:lower() then
        suffix = " · " .. title
    elseif code == "" and type(track.codec) == "string" and track.codec ~= "" then
        -- Technical format is evidence of track identity, never of its language.
        suffix = " · " .. track.codec
    end
    return prefix .. " " .. number .. " — " .. language .. suffix
end

local function clock(seconds)
    seconds = math.max(0, math.floor(seconds or 0))
    local h, m, s = math.floor(seconds / 3600), math.floor(seconds / 60) % 60, seconds % 60
    if h > 0 then return string.format("%d:%02d:%02d", h, m, s) end
    return string.format("%02d:%02d", m, s)
end

local WHITE, ACCENT, MUTED = "F5F5F5", "00CAFF", "B6B6B6" -- ASS BGR
local function rect(parts, x, y, w, h, r, color, alpha)
    if w <= 0 or h <= 0 then return end
    r = math.floor(math.min(r, w / 2, h / 2))
    local x2, y2 = x + w, y + h
    local path = string.format(
        "m %d %d l %d %d b %d %d %d %d %d %d l %d %d " ..
        "b %d %d %d %d %d %d l %d %d b %d %d %d %d %d %d " ..
        "l %d %d b %d %d %d %d %d %d",
        x+r,y, x2-r,y, x2,y,x2,y,x2,y+r, x2,y2-r,
        x2,y2,x2,y2,x2-r,y2, x+r,y2, x,y2,x,y2,x,y2-r,
        x,y+r, x,y,x,y,x+r,y)
    parts[#parts+1] = string.format(
        "{\\an7\\pos(0,0)\\p1\\bord0\\shad0\\1c&H%s&\\1a&H%02X&}%s{\\p0}",
        color, alpha or 0, path)
end

local function label(parts, x, y, value, size, color, align, bold)
    x, y = math.floor(x + 0.5), math.floor(y + 0.5)
    parts[#parts+1] = string.format(
        "{\\an%d\\pos(%d,%d)\\fnDejaVu Sans\\fs%d\\b%d\\bord0\\shad0\\1c&H%s&}%s",
        align or 7, x, y, size, bold and 1 or 0, color or WHITE, clean(value))
end

local function pill(parts, x, y, w, h, value, active, primary)
    rect(parts, x, y, w, h, 12, active and "383838" or "333337", active and 5 or 65)
    if active then
        -- Inset accent line wraps the same rounded geometry as the button.
        rect(parts, x, y, w, h, 12, ACCENT, 25)
        rect(parts, x+3, y+3, w-6, h-6, 9, "353337", 0)
    end
    label(parts, x+w/2, y+h/2, value, primary and 23 or 20,
          active and ACCENT or WHITE, 5, active)
end

local function seek(seconds)
    if mp.get_property_number("duration", 0) > 0 then
        mp.commandv("seek", seconds, "relative")
    end
end

-- Virtual 1280-wide coordinate space, anchored to the real OSD bottom.
-- Separate ASS events provide independent positions and crisp vector shapes.
local function render()
    if not visible then return end
    local w, h = 1280, 720
    if mp.get_osd_size then
        local actual_w, actual_h = mp.get_osd_size()
        if actual_w and actual_h and actual_w > 0 and actual_h > 0 then
            h = math.max(540, math.floor(1280 * actual_h / actual_w + 0.5))
        end
    end
    overlay.res_x, overlay.res_y = w, h

    local paused = mp.get_property_bool("pause", false)
    local duration = mp.get_property_number("duration", 0)
    local pos = mp.get_property_number("time-pos", 0)
    local title = short(mp.get_property("media-title", "Broadcaster"), 88)
    local parts = {}
    local bottom = h - 22
    local top = bottom - 218
    rect(parts, 72, top, 1136, 218, 21, "18181B", 35)
    label(parts, 113, top+23, title, 26, WHITE, 7, true)
    label(parts, 1164, top+29,
          clock(pos) .. "  /  " .. (duration > 0 and clock(duration) or tr("LIVE")),
          21, MUTED, 9)

    if selected == 0 and not panel then
        rect(parts, 102, top+73, 1076, 31, 13, ACCENT, 48)
    end
    rect(parts, 124, top+85, 1032, 8, 4, "77777B", 60)
    if duration > 0 then
        local amount = math.max(0, math.min(1, pos / duration))
        local fill = math.floor(1032 * amount + 0.5)
        rect(parts, 124, top+85, fill, 8, 4, ACCENT, 0)
        rect(parts, 124+fill-8, top+79, 16, 20, 8, ACCENT, 0)
    else
        label(parts, 640, top+86, tr("LIVE"), 19, WHITE, 5, true)
    end

    local buttons = {
        {130, top+114, 240, 43, tr("−10 seconds")},
        {388, top+114, 504, 43, tr(paused and "▶  Play" or "Ⅱ  Pause")},
        {910, top+114, 240, 43, tr("+10 seconds")},
        {115, top+168, 181, 36, tr("Audio")},
        {305, top+168, 181, 36, tr("Subtitles")},
        {495, top+168, 181, 36, tr("Speed")},
        {685, top+168, 263, 36, tr("Fullscreen")},
        {957, top+168, 208, 36, tr("Exit")},
    }
    for i, button in ipairs(buttons) do
        pill(parts, button[1], button[2], button[3], button[4], button[5],
             selected == i and not panel, i <= 3)
    end

    if panel then
        local names = {audio=tr("Audio tracks"), sub=tr("Subtitles"), speed=tr("Playback speed")}
        local ptop = top - 265
        rect(parts, 690, ptop, 494, 255, 17, "161619", 15)
        label(parts, 719, ptop+18, names[panel], 24, WHITE, 7, true)
        label(parts, 1156, ptop+24, tr("Back to controls"), 16, MUTED, 9)
        local top_index = math.max(1, math.min(option_index - 2, math.max(1, #options - 4)))
        for i = top_index, math.min(#options, top_index + 4) do
            local row_y = ptop + 57 + (i-top_index)*36
            pill(parts, 708, row_y, 458, 32, short(options[i].label, 51), i == option_index, false)
        end
        if panel == "sub" and #options == 1 then
            -- Keep Off selectable, but explain that mpv exposed no subtitle tracks.
            label(parts, 715, ptop+112, tr("No subtitle tracks available"), 19, MUTED, 7)
        end
        if #options > 5 then
            label(parts, 935, ptop+238, string.format("%d / %d", option_index, #options),
                  15, MUTED, 5)
        end
    end

    local next_frame = table.concat(parts, "\n")
    if next_frame ~= last_frame then
        overlay.data = next_frame
        overlay:update()
        last_frame = next_frame
    end
end

local function touch()
    last_input = mp.get_time()
    render()
end

local function show()
    visible, selected, panel = true, 2, nil
    touch()
end

local function hide()
    visible, panel = false, nil
    overlay:remove()
    last_frame = nil
end

local function open_panel(kind)
    options, panel, option_index = {}, kind, 1
    previous_selection = selected
    if kind == "speed" then
        local current = mp.get_property_number("speed", 1)
        for _, value in ipairs(speeds) do
            options[#options+1] = {label=string.format("%g×",value), value=value}
            if math.abs(current-value) < 0.001 then option_index = #options end
        end
    else
        if kind == "sub" then options[1] = {label=tr("Off"), value="no"} end
        local number = 0
        for _, track in ipairs(mp.get_property_native("track-list", {})) do
            if track.type == kind and track.id ~= nil then
                number = number + 1
                options[#options+1] = {
                    label=track_label(track, number, kind), value=track.id
                }
                if track.selected then option_index = #options end
            end
        end
        if kind == "sub" and mp.get_property("sid", "no") == "no" then option_index = 1 end
        if #options == 0 then options[1] = {label=tr("No tracks available"), value=nil} end
    end
    touch()
end

local function activate()
    if panel then
        local choice = options[option_index]
        if choice and choice.value ~= nil then
            if panel == "speed" then
                mp.commandv("set", "speed", tostring(choice.value))
            else
                mp.commandv("set", panel == "audio" and "aid" or "sid",
                            tostring(choice.value))
            end
        end
        panel, selected = nil, previous_selection
    elseif selected == 0 then
        -- Timeline: seek with Left and Right instead of activating it.
    elseif selected == 1 then seek(-10)
    elseif selected == 2 then mp.commandv("cycle", "pause")
    elseif selected == 3 then seek(10)
    elseif selected == 4 then open_panel("audio")
    elseif selected == 5 then open_panel("sub")
    elseif selected == 6 then open_panel("speed")
    elseif selected == 7 then mp.commandv("cycle", "fullscreen")
    elseif selected == 8 then mp.commandv("quit"); return
    end
    touch()
end

local function ok()
    if not visible then show() else activate() end
end

local function move(horizontal, vertical, hidden_seek)
    if not visible then seek(hidden_seek); return end
    if panel then
        if vertical ~= 0 then
            option_index = (option_index-1+vertical+#options) % #options + 1
        elseif horizontal < 0 then
            panel, selected = nil, previous_selection
        end
    elseif selected == 0 then
        if horizontal ~= 0 then seek(horizontal * 10)
        elseif vertical > 0 then selected = 2
        elseif vertical < 0 then seek(60) end
    elseif horizontal ~= 0 then
        local first, count = selected <= 3 and 1 or 4, selected <= 3 and 3 or 5
        selected = first + (selected-first+horizontal+count) % count
    elseif vertical < 0 then
        if selected <= 3 then selected = 0
        else selected = math.min(3, math.max(1, selected-3)) end
    elseif vertical > 0 then
        if selected <= 3 then selected = math.min(8, selected+3)
        else selected = 0 end
    end
    touch()
end

local function back()
    if panel then
        panel, selected = nil, previous_selection
        touch()
    elseif visible then hide()
    else mp.commandv("quit") end
end
local function menu() if visible then hide() else show() end end
local function media(...)
    mp.commandv(...)
    if visible then touch() end
end

mp.add_forced_key_binding("ENTER", "broadcaster-ok", ok)
mp.add_forced_key_binding("KP_ENTER", "broadcaster-keypad-ok", ok)
mp.add_forced_key_binding("LEFT", "broadcaster-left", function() move(-1, 0, -10) end)
mp.add_forced_key_binding("RIGHT", "broadcaster-right", function() move(1, 0, 10) end)
mp.add_forced_key_binding("UP", "broadcaster-up", function() move(0, -1, 60) end)
mp.add_forced_key_binding("DOWN", "broadcaster-down", function() move(0, 1, -60) end)
for _, key in ipairs({"GO_BACK", "ESC", "BS"}) do
    mp.add_forced_key_binding(key, "broadcaster-back-" .. key, back)
end
mp.add_forced_key_binding("MENU", "broadcaster-menu", menu)
mp.add_forced_key_binding("STOP", "broadcaster-stop", function() mp.commandv("quit") end)
for _, key in ipairs({"GO_HOME", "HOME"}) do
    mp.add_forced_key_binding(key, "broadcaster-home-" .. key,
                               function() mp.commandv("quit") end)
end
mp.add_forced_key_binding("PLAYPAUSE", "broadcaster-playpause",
                           function() media("cycle", "pause") end)
mp.add_forced_key_binding("PLAY", "broadcaster-play",
                           function() media("set", "pause", "no") end)
mp.add_forced_key_binding("PAUSE", "broadcaster-pause",
                           function() media("set", "pause", "yes") end)

mp.add_periodic_timer(0.5, function()
    if visible then
        if not panel and not mp.get_property_bool("pause", false)
            and mp.get_time() - last_input >= idle_seconds then
            hide()
        else
            render() -- render only calls update when text/time actually changed
        end
    end
end)
