#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Fail-closed local assertions for the packaged LuaJIT/remote probe."""
import unittest

from mpv_luajit_probe import adapted_remote_test, lua_entry, latest_stage, RESULT


class LuaJitProbeTests(unittest.TestCase):
    def test_remote_reference_exactly_once(self):
        original = 'dofile("data/broadcaster-mpv-remote.lua")'
        rewritten = adapted_remote_test(original)
        self.assertEqual(
            rewritten, 'dofile("/app/share/broadcaster/broadcaster-mpv-remote.lua")')

    def test_missing_or_duplicated_remote_reference_is_rejected(self):
        original = 'dofile("data/broadcaster-mpv-remote.lua")'
        for content in ("", original + original, 'dofile("other.lua")'):
            with self.subTest(content=content):
                with self.assertRaises(ValueError):
                    adapted_remote_test(content)

    def test_oracle_requires_jit_trace_and_complete_remote_suite(self):
        script = lua_entry(".probe-fixed", "result.txt")
        for clause in ('jit.status()', 'jit.util', 'traceinfo(1)',
                       'remote-tests.lua', 'file:write', RESULT.strip()):
            with self.subTest(clause=clause):
                self.assertIn(clause, script)

    def test_stage_parser_does_not_echo_untrusted_diagnostics(self):
        self.assertEqual(latest_stage("bad provider tokens, URL, credentials"), "NONE")
        self.assertEqual(latest_stage("BROADCASTER_LUAJIT_STAGE_JIT_ENABLED\n"
                                      "private details never shown"), "JIT_ENABLED")
        self.assertEqual(latest_stage("BROADCASTER_LUAJIT_STAGE_LOADED\n"
                                      "BROADCASTER_LUAJIT_STAGE_TRACE_COMPILED\n"),
                         "TRACE_COMPILED")


if __name__ == '__main__':
    unittest.main()
