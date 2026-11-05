#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Negative controls prevent the packaged-player CI probes from going falsely green."""
import subprocess
import unittest

from mpv_tls_probe import assert_tls_evidence, has_certificate_verification_diagnostic
from mpv_http_probe import hls_fixture_present


class TLSEvidenceTest(unittest.TestCase):
    def completed(self, code, stderr="", stdout=""):
        return subprocess.CompletedProcess(["synthetic-player"], code,
                                           stdout=stdout, stderr=stderr)

    def check(self, negative=None, negative_paths=(), positive=None,
              positive_paths=("/clip.wav",)):
        if negative is None:
            negative = self.completed(2, "TLS certificate verification failed.\n")
        if positive is None:
            positive = self.completed(0)
        assert_tls_evidence(negative, negative_paths, positive, positive_paths)

    def test_real_diagnostic_and_positive_get(self):
        self.check()

    def test_diagnostic_in_mpv_default_stdout(self):
        negative = self.completed(2, stdout="[curl] TLS certificate verification failed.\n")
        self.check(negative=negative)
        self.assertTrue(has_certificate_verification_diagnostic(negative))

    def test_diagnostic_in_explicit_stderr(self):
        negative = self.completed(2, stderr="[curl] TLS certificate verification failed.\n")
        self.check(negative=negative)

    def test_unrelated_stdout_error_is_not_certificate_proof(self):
        with self.assertRaisesRegex(AssertionError, "certificate-verification diagnostic"):
            self.check(negative=self.completed(2, stdout="[curl] Could not resolve host"))

    def test_wrong_player_error_is_not_certificate_proof(self):
        with self.assertRaisesRegex(AssertionError, "certificate-verification diagnostic"):
            self.check(negative=self.completed(1, "could not connect to host"))

    def test_empty_stderr_is_not_certificate_proof(self):
        with self.assertRaisesRegex(AssertionError, "certificate-verification diagnostic"):
            self.check(negative=self.completed(1))

    def test_successful_unverified_negative_rejected(self):
        with self.assertRaisesRegex(AssertionError, "accepted untrusted"):
            self.check(negative=self.completed(0, "TLS certificate verification failed."))

    def test_failed_positive_control_rejected(self):
        with self.assertRaisesRegex(AssertionError, "same HTTPS media"):
            self.check(positive=self.completed(1))

    def test_missing_positive_media_request_rejected(self):
        with self.assertRaisesRegex(AssertionError, "same HTTPS media"):
            self.check(positive_paths=())

    def test_negative_media_request_rejected(self):
        with self.assertRaisesRegex(AssertionError, "default TLS request reached"):
            self.check(negative_paths=("/clip.wav",))


class HLSFixtureTest(unittest.TestCase):
    def test_required_hls_missing_fails_closed(self):
        with self.assertRaisesRegex(AssertionError, "required HLS fixture unavailable"):
            hls_fixture_present(None, True)

    def test_optional_local_hls_can_be_skipped(self):
        self.assertFalse(hls_fixture_present(None, False))

    def test_required_hls_data_runs(self):
        self.assertTrue(hls_fixture_present(b"synthetic-ts", True))


if __name__ == "__main__":
    unittest.main()
