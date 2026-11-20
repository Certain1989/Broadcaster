#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""No live network access: positive and negative regression cases."""
import datetime as dt
import copy
import io
import os
import subprocess
import urllib.error
from unittest.mock import patch
import json
import tempfile
import unittest
from pathlib import Path

import api_compat as ap
import issue_sync as sync
import luajit_drift as lua
import native_candidate as native


class APIContracts(unittest.TestCase):
    def test_mediathek_valid(self):
        def fetch(url, payload=None):
            if url.endswith("/channels"):
                return {"channels": ["ARD", "ZDF"], "error": None}
            self.assertEqual(payload["size"], 1)
            return {"result": {"results": [
                {"title": "News", "channel": "ARD", "topic": "News"}],
                "queryInfo": {"totalResults": 12}}, "err": None}
        self.assertIn("valid", ap.check_mediathek(fetch))

    def test_mediathek_empty_canary_is_transient(self):
        def fetch(url, payload=None):
            if payload is None:
                return {"channels": ["ARD"]}
            return {"result": {"results": [], "queryInfo": {"totalResults": 0}}}
        with self.assertRaises(ap.ProbeError) as error:
            ap.check_mediathek(fetch)
        self.assertEqual(error.exception.kind, "transient")

    def test_mediathek_rejects_upstream_error_despite_channels(self):
        with self.assertRaises(ap.ProbeError) as error:
            ap.check_mediathek(lambda url, payload=None:
                               {"error": "upstream failure", "channels": ["ARD"]})
        self.assertEqual(error.exception.kind, "contract")

    def test_mediathek_rejects_search_error(self):
        def fetch(url, payload=None):
            return {"channels": ["ARD"]} if payload is None else {
                "err": "unavailable", "result": {"results": [{"title": "ok"}],
                "queryInfo": {"totalResults": 1}}}
        with self.assertRaises(ap.ProbeError):
            ap.check_mediathek(fetch)

    def test_http_429_and_502_are_transient(self):
        for code in (429, 502):
            with self.subTest(code=code), patch.object(ap.urllib.request, "urlopen", side_effect=urllib.error.HTTPError(
                    "https://example.invalid", code, "Unavailable", {}, io.BytesIO())):
                with self.assertRaises(ap.ProbeError) as error:
                    ap.request_json("https://example.invalid")
                self.assertEqual(error.exception.kind, "transient")

    def test_mediathek_missing_fields_not_green(self):
        def fetch(url, payload=None):
            return {"channels": ["ARD"]} if payload is None else {"result": {"results": []}}
        with self.assertRaises(ap.ProbeError) as error:
            ap.check_mediathek(fetch)
        self.assertEqual(error.exception.kind, "contract")

    def test_zapp_valid(self):
        def fetch(url, payload=None):
            if url.endswith("channelInfoList"):
                return {"das_erste": {"name": "Das Erste", "streamUrl": "https://example.invalid"}}
            self.assertTrue(url.endswith("shows/das_erste"))
            return {"shows": [{"title": "News", "channel": "das_erste",
                               "startTime": "2026-11-20T10:00:00+01:00",
                               "endTime": "2026-11-20T11:00:00+01:00"}]}
        self.assertIn("valid", ap.check_zapp(fetch))

    def test_zapp_error_enum_not_green(self):
        def fetch(url, payload=None):
            if url.endswith("channelInfoList"):
                return {"zdf": {"name": "ZDF", "streamUrl": "x"}}
            return {"error": "unavailable"}
        with self.assertRaises(ap.ProbeError):
            ap.check_zapp(fetch)

    def test_transient_retries_fail_closed(self):
        calls = []
        def failure():
            calls.append(True)
            raise ap.ProbeError("transient", "timeout")
        result = ap.probe("zapp", failure, retries=2, pause=lambda _: None)
        self.assertEqual((result["status"], result["kind"], len(calls)),
                         ("fail", "transient", 3))

    def test_schema_errors_do_not_retry(self):
        calls = []
        def failure():
            calls.append(True)
            raise ap.ProbeError("contract", "invalid")
        self.assertEqual(ap.probe("zapp", failure, pause=lambda _: None)["kind"], "contract")
        self.assertEqual(len(calls), 1)


class IssueContracts(unittest.TestCase):
    def test_invalid_marker_rejected(self):
        with self.assertRaises(ValueError):
            sync.marker("x\nmalicious")

    def test_transient_first_failure_creates_no_issue(self):
        actions = []
        def api(method, path, payload=None):
            actions.append((method, path, payload))
            return [] if path.startswith("/issues?") else {"workflow_runs": []}
        sync.synchronize([{"key": "api-zapp", "title": "Zapp", "status": "fail",
                           "kind": "transient", "detail": "timeout"}], api=api,
                         previous={},
                         run_url="https://github.com/Certain1989/Broadcaster/actions/runs/1")
        self.assertFalse(any(method == "POST" for method, _, _ in actions))

    def test_existing_issue_updates_without_duplicate_and_closes(self):
        issue = {"number": 42, "body": sync.marker("api-zapp") + "\nolder",
                 "user": {"id": sync.BOT_ID}}
        writes = []
        def api(method, path, payload=None):
            if method == "GET":
                return [issue]
            writes.append((method, path, payload))
            return issue
        item = {"key": "api-zapp", "title": "Zapp", "status": "fail",
                "kind": "contract", "detail": "breaking field"}
        sync.synchronize([item], api=api, run_url="https://github.com/example")
        self.assertEqual(writes[0][:2], ("PATCH", "/issues/42"))
        self.assertNotIn("POST", [write[0] for write in writes])
        writes.clear()
        sync.synchronize([{**item, "status": "pass"}], api=api)
        self.assertEqual(writes[0][2], {"state": "closed"})

    def test_untrusted_issue_marker_never_modified(self):
        user_issue = {"number": 42, "body": sync.marker("api-zapp") + "\nspoof",
                      "user": {"id": 123}}
        writes = []
        def api(method, path, payload=None):
            if method == "GET": return [user_issue]
            writes.append((method, path, payload))
            return {"number": 43, "body": payload.get("body"), "user": {"id": sync.BOT_ID}}
        good = {"key": "api-zapp", "title": "Zapp", "status": "pass",
                "kind": "none", "detail": "recovered"}
        sync.synchronize([good], api=api)
        self.assertEqual(writes, [])
        sync.synchronize([{**good, "status": "fail", "kind": "contract"}], api=api)
        self.assertEqual(writes[0][0], "POST")
        self.assertEqual(writes[0][1], "/issues")

    def test_marker_requires_first_line_even_for_bot(self):
        bad = {"body": "user text\n" + sync.marker("api-zapp") + "\n",
               "user": {"id": sync.BOT_ID}}
        self.assertFalse(sync.owned_issue(bad, "api-zapp"))

    def test_previous_completed_run_is_scoped_by_provider(self):
        now = dt.datetime.now(dt.timezone.utc)
        recent = (now - dt.timedelta(hours=22)).isoformat()
        def api(method, path, payload=None):
            if "/actions/workflows/" in path:
                return {"workflow_runs": [
                    {"id": 12, "event": "schedule", "head_branch": "main",
                     "created_at": recent, "status": "completed", "conclusion": "failure"}]}
            if "/artifacts" in path:
                return {"artifacts": [{"name": sync.RESULT_ARTIFACT, "id": 18, "expired": False}]}
            raise AssertionError(path)
        previous = sync.previous_scheduled_results(api=api,
            read_artifact=lambda _: json.dumps([
                {"key": "api-mediathek", "status": "fail", "kind": "contract"},
                {"key": "api-zapp", "status": "pass", "kind": "none"}]).encode(), now=now)
        self.assertEqual(previous["api-mediathek"]["status"], "fail")
        writes = []
        def issues(method, path, payload=None):
            if method == "GET": return []
            writes.append((method, path))
            return {"number": 5, "body": payload.get("body"), "user": {"id": sync.BOT_ID}}
        zapp = {"key": "api-zapp", "title": "Zapp", "status": "fail",
                "kind": "transient", "detail": "timeout"}
        sync.synchronize([zapp], api=issues, previous=previous)
        self.assertEqual(writes, [])  # Mediathek failure must NOT count for Zapp
        sync.synchronize([{**zapp, "key": "api-mediathek"}], api=issues, previous=previous)
        self.assertEqual(writes[0][0], "POST")

    def test_previous_success_and_stale_result_do_not_count(self):
        now = dt.datetime.now(dt.timezone.utc)
        def runs(when):
            def api(method, path, payload=None):
                if "/actions/workflows/" in path:
                    return {"workflow_runs": [
                        {"id": 11, "event": "schedule", "head_branch": "main",
                         "created_at": when.isoformat(), "status": "completed"}]}
                if "/artifacts" in path:
                    return {"artifacts": [{"name": sync.RESULT_ARTIFACT, "id": 9}]}
                raise AssertionError(path)
            return api
        self.assertIsNone(sync.previous_scheduled_results(api=runs(now - dt.timedelta(days=4)),
                                                          read_artifact=lambda _: b"[]", now=now))
        good = json.dumps([
            {"key": "api-mediathek", "status": "pass", "kind": "none"},
            {"key": "api-zapp", "status": "pass", "kind": "none"}]).encode()
        previous = sync.previous_scheduled_results(api=runs(now - dt.timedelta(hours=23)),
            read_artifact=lambda _: good, now=now)
        self.assertEqual(previous["api-zapp"]["status"], "pass")
        self.assertIsNone(sync.previous_scheduled_results(api=runs(now - dt.timedelta(hours=23)),
            read_artifact=lambda _: b"not json", now=now))



class NativeContracts(unittest.TestCase):
    def fixture(self):
        modules = []
        for name in ("mpv", "libass", "libplacebo", "blueprint-compiler", "luajit"):
            source = {"type": "git", "url": "https://example.invalid/" + name,
                      "commit": "a" * 40}
            if name != "luajit":
                source["tag"] = "v1.0.0"
                source["x-checker-data"] = {"type": "git"}
            modules.append({"name": name, "sources": [source]})
        return {"id": "test", "modules": modules}

    def test_native_allows_only_approved_tag_commit_pairs(self):
        old = self.fixture()
        candidate = copy.deepcopy(old)
        source = candidate["modules"][0]["sources"][0]
        source["tag"] = "v1.0.1"
        source["commit"] = "b" * 40
        self.assertEqual(native.validate(old, candidate), ["mpv"])
        candidate["modules"][0]["config-opts"] = ["--unsafe"]
        with self.assertRaises(ValueError):
            native.validate(old, candidate)

    def test_native_rejects_unrelated_luajit_change(self):
        old = self.fixture()
        candidate = copy.deepcopy(old)
        candidate["modules"][-1]["sources"][0]["commit"] = "b" * 40
        with self.assertRaises(ValueError):
            native.validate(old, candidate)

    def test_native_rejects_prestaged_unrelated_file(self):
        oldcwd = os.getcwd()
        with tempfile.TemporaryDirectory() as directory:
            try:
                os.chdir(directory)
                subprocess.run(["git", "init", "-q"], check=True)
                manifest = Path(native.MANIFEST)
                manifest.parent.mkdir(parents=True)
                manifest.write_text(json.dumps(self.fixture()))
                subprocess.run(["git", "add", str(manifest)], check=True)
                subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=t@example.invalid",
                                "commit", "-qm", "baseline"], check=True)
                Path("unexpected.sh").write_text("echo unexpected\n")
                subprocess.run(["git", "add", "unexpected.sh"], check=True)
                with self.assertRaisesRegex(ValueError, "staged"):
                    native.assert_worktree(allow_manifest=True)
            finally:
                os.chdir(oldcwd)

    def test_checker_job_has_no_write_permissions(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/native-dependencies.yml").read_text()
        checker = workflow.split("\n  check:\n", 1)[1].split("\n  publish:\n", 1)[0]
        self.assertIn("      contents: read", checker)
        self.assertNotIn("contents: write", checker)
        self.assertNotIn("pull-requests: write", checker)
        self.assertNotIn("GH_TOKEN:", checker)
        self.assertIn("upload-artifact", checker)
        publisher = workflow.split("\n  publish:\n", 1)[1].split("\n  report:\n", 1)[0]
        self.assertIn("download-artifact", publisher)
        self.assertIn("native_candidate.py apply", publisher)
        for path in (".github/workflows/api-compatibility.yml",
                     ".github/workflows/native-dependencies.yml",
                     ".github/workflows/rustsec-weekly.yml"):
            text = (root / path).read_text()
            self.assertIn("github.ref == 'refs/heads/main'", text)

    def test_luajit_pin_drift_is_not_ancestry_claim(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            path.write_text(json.dumps({"modules": [{"name": "luajit", "sources": [
                {"commit": "a" * 40}]}]}))
            result = lua.drift(path, "b" * 40)
            self.assertEqual(result["status"], "fail")
            self.assertIn("not proof of ancestry", result["detail"])


if __name__ == "__main__":
    unittest.main()
