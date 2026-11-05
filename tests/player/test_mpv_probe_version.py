#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Static negative and positive checks for packaged mpv version validation."""
import unittest

from mpv_http_probe import expected_mpv_version


class MpvBannerTest(unittest.TestCase):
    def test_tagged_upstream_release(self):
        self.assertTrue(expected_mpv_version("mpv v0.42.0 Copyright 2000-2026\n", "0.42"))

    def test_untagged_version_banner(self):
        self.assertTrue(expected_mpv_version("mpv 0.42.0 Copyright 2000-2026\n", "0.42"))

    def test_previous_major_minor_is_rejected(self):
        self.assertFalse(expected_mpv_version("mpv v0.41.0\n", "0.42"))

    def test_wrong_executable_is_rejected(self):
        self.assertFalse(expected_mpv_version("flatpak: command not found\n", "0.42"))

    def test_wrong_minor_prefix_is_rejected(self):
        self.assertFalse(expected_mpv_version("mpv v0.420.0\n", "0.42"))


if __name__ == "__main__":
    unittest.main()
