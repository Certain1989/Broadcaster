#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Prove packaged mpv executes LuaJIT traces and the real remote Lua tests.

Synthetic, offline local WAV only. The mpv script and result marker live in
its disposable Flatpak build tree, never in the deliverable bundle; all files
are removed after the check. Neither process diagnostics nor URLs are logged.
"""
import argparse
import subprocess
import tempfile
import wave
from pathlib import Path

from mpv_http_probe import expected_mpv_version


RESULT = "LuaJIT 2.1 JIT trace and Broadcaster remote PASS\n"
REMOTE_REFERENCE = 'dofile("data/broadcaster-mpv-remote.lua")'
REMOTE_IN_APP = '/app/share/broadcaster/broadcaster-mpv-remote.lua'


def adapted_remote_test(source):
    """Run the unchanged test logic against the installed Flatpak remote."""
    if source.count(REMOTE_REFERENCE) != 1:
        raise ValueError("unexpected Broadcaster remote test format")
    return source.replace(REMOTE_REFERENCE, f'dofile("{REMOTE_IN_APP}")')


def lua_entry(app_folder, marker):
    return f'''-- Ephemeral packaged-LuaJIT validation script; not part of the app.
-- Export fixed stage tokens only. Python never logs media, local paths or
-- arbitrary script diagnostics and still requires the success marker.
local function checkpoint(stage)
    io.stderr:write("BROADCASTER_LUAJIT_STAGE_" .. stage .. "\\n")
    io.stderr:flush()
end
checkpoint("LOADED")
local jit = assert(require("jit"))
checkpoint("JIT_IMPORTED")
assert(type(jit.version) == "string" and
       jit.version:find("LuaJIT 2.1", 1, true) == 1,
       "mpv did not embed LuaJIT 2.1")
assert(jit.status(), "LuaJIT JIT is disabled in packaged mpv")
checkpoint("JIT_ENABLED")
jit.flush()
require("jit.opt").start("hotloop=1")
local function arithmetic()
    local result = 0
    for i = 1, 1000 do result = result + i end
    return result
end
for i = 1, 20 do assert(arithmetic() == 500500) end
assert(require("jit.util").traceinfo(1), "LuaJIT did not compile a JIT trace")
checkpoint("TRACE_COMPILED")
-- The accelerated hotloop=1 setting is probe-only. Reset it to LuaJIT's
-- shipped defaults before executing the real remote regression fixture.
-- Otherwise this artificially changes the JIT policy of the application.
require("jit.opt").start()
jit.flush()
checkpoint("JIT_POLICY_RESTORED")
-- The real mpv Lua VM has a live global `mp` object. The standalone remote
-- fixture intentionally replaces global `mp` with a mock. Running it in the
-- live mpv global environment corrupts the surrounding script API. Execute
-- the unmodified test logic in a Lua 5.1 environment scoped to this fixture,
-- including its nested dofile of the actual *installed* remote Lua source.
local fixture = "/app/share/broadcaster/{app_folder}/remote-tests.lua"
local production = "/app/share/broadcaster/broadcaster-mpv-remote.lua"
local runner = assert(loadfile(fixture), "remote fixture could not be loaded")
checkpoint("FIXTURE_LOADED")
assert(loadfile(production), "installed production remote could not be loaded")
checkpoint("PRODUCTION_REMOTE_LOADED")
local env = setmetatable({{}}, {{__index = _G}})
env._G = env
local fixture_result = false
env.print = function(message)
    if message == "MPV graphical remote controls, metadata fallbacks and lifecycle PASS" then
        fixture_result = true
    else
        error("unexpected remote fixture print output")
    end
end
env.dofile = function(path)
    assert(path == production, "fixture must load exact installed remote")
    local nested = assert(loadfile(path), "could not load installed remote")
    setfenv(nested, env)
    return nested()
end
setfenv(runner, env)
local ok, failure = pcall(runner)
if not ok then
    -- Whitelist only exact publicly tracked test/script filenames and numeric
    -- source lines. Never forward script exception details, URLs or metadata.
    local source, line = tostring(failure):match("([^/:]+%.lua):(%d+):")
    local safe_source = source == "remote-tests.lua" and "FIXTURE"
                     or source == "broadcaster-mpv-remote.lua" and "REMOTE"
                     or "OTHER"
    local safe_line = tonumber(line) or 0
    if safe_line > 9999 then safe_line = 0 end
    io.stderr:write(string.format("BROADCASTER_LUAJIT_TEST_FAILURE_%s_%04d\\n",
                                  safe_source, safe_line))
    error("packaged remote suite failed")
end
assert(fixture_result, "remote fixture did not reach its PASS marker")
checkpoint("REMOTE_SUITE_PASS")
local file = assert(io.open("/app/share/broadcaster/{app_folder}/{marker}", "w"))
file:write("{RESULT.strip()}\\n")
assert(file:close())
checkpoint("MARKER_WRITTEN")
'''


STAGES = ("LOADED", "JIT_IMPORTED", "JIT_ENABLED", "TRACE_COMPILED",
          "JIT_POLICY_RESTORED", "FIXTURE_LOADED", "PRODUCTION_REMOTE_LOADED",
          "REMOTE_SUITE_PASS", "MARKER_WRITTEN")


def latest_stage(output):
    # Deliberately never surface arbitrary stdout/stderr from mpv or any
    # third-party script. Return only one fixed stage name from known tokens.
    completed = [name for name in STAGES
                 if f"BROADCASTER_LUAJIT_STAGE_{name}" in output]
    return completed[-1] if completed else "NONE"


def safe_remote_failure(output):
    # Report only a public-source file category and four-digit line number.
    # All arbitrary Lua/mpv exception messages remain captured and suppressed.
    import re
    found = re.findall(r"BROADCASTER_LUAJIT_TEST_FAILURE_(FIXTURE|REMOTE|OTHER)_(\d{4})(?!\d)", output)
    return f"{found[-1][0]}:{found[-1][1]}" if found else "NONE"


def synthetic_wav(path):
    with wave.open(str(path), "wb") as audio:
        audio.setnchannels(1)
        audio.setsampwidth(2)
        audio.setframerate(8000)
        audio.writeframes(b"\x00\x00" * 8000)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-root", default="flatpak_app")
    parser.add_argument("--remote-test", default="tests/mpv-remote.lua")
    parser.add_argument("--expect-version", default="0.42", choices=("0.42",))
    args = parser.parse_args()
    build_root = Path(args.build_root).resolve()
    packaged_mpv = build_root / "files/bin/mpv"
    app_share = build_root / "files/share/broadcaster"
    installed_remote = app_share / "broadcaster-mpv-remote.lua"
    if not packaged_mpv.is_file() or not installed_remote.is_file():
        raise AssertionError("packaged mpv or installed remote Lua script missing")
    reference = Path(args.remote_test).read_text(encoding="utf-8")
    adapted = adapted_remote_test(reference)

    command = ["flatpak", "build", str(build_root), "mpv"]
    version = subprocess.run([*command, "--version"], capture_output=True,
                             text=True, timeout=15, check=True).stdout
    if not expected_mpv_version(version, args.expect_version):
        raise AssertionError("packaged mpv version mismatch for LuaJIT probe")

    # Temporary files are mapped into the build sandbox at /app/share.
    with tempfile.TemporaryDirectory(prefix=".luajit-probe-", dir=app_share) as tmp:
        folder = Path(tmp)
        marker = folder / "result.txt"
        (folder / "remote-tests.lua").write_text(adapted, encoding="utf-8")
        (folder / "probe.lua").write_text(
            lua_entry(folder.name, marker.name), encoding="utf-8")
        synthetic_wav(folder / "clip.wav")
        base = f"/app/share/broadcaster/{folder.name}"
        argv = [*command, "--no-config", "--no-video", "--ao=null",
                "--cache=no", "--really-quiet", f"--script={base}/probe.lua",
                "--", f"{base}/clip.wav"]
        result = subprocess.run(argv, capture_output=True, text=True,
                                errors="replace", timeout=45, check=False)
        passed = marker.is_file() and marker.read_text(encoding="utf-8") == RESULT
        print("Packaged LuaJIT oracle:",
              "mpv_exit_ok=", result.returncode == 0,
              "jit_and_remote_marker=", passed,
              "last_completed_stage=", latest_stage(result.stdout + result.stderr),
              "safe_remote_failure=", safe_remote_failure(result.stdout + result.stderr))
        if result.returncode != 0 or not passed:
            raise AssertionError("packaged LuaJIT JIT/remote probe failed")
        print("PASS packaged LuaJIT 2.1 JIT trace and real Broadcaster remote tests")


if __name__ == "__main__":
    main()
