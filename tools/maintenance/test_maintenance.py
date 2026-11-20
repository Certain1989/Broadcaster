#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""No live network access: positive and negative regression cases."""
import datetime as dt
import json
import tempfile
import unittest
from pathlib import Path

import api_compat as ap
import issue_sync as sync
import luajit_drift as lua
import watchdog


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
                         run_url="https://github.com/Certain1989/Broadcaster/actions/runs/1")
        self.assertFalse(any(method == "POST" for method, _, _ in actions))

    def test_existing_issue_updates_without_duplicate_and_closes(self):
        issue = {"number": 42, "body": sync.marker("api-zapp") + "\nolder"}
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

    def test_transient_requires_recent_failure(self):
        recent = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=20)).isoformat()
        def api(method, path, payload=None):
            return {"workflow_runs": [{"created_at": recent, "conclusion": "failure"}]}
        self.assertTrue(sync.repeated_transient(api))


class NativeAndWatchdog(unittest.TestCase):
    def test_luajit_pin_drift_is_not_ancestry_claim(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            path.write_text(json.dumps({"modules": [{"name": "luajit", "sources": [
                {"commit": "a" * 40}]}]}))
            result = lua.drift(path, "b" * 40)
            self.assertEqual(result["status"], "fail")
            self.assertIn("not proof of ancestry", result["detail"])

    def test_kuma_push_replaces_existing_status_and_msg(self):
        from unittest.mock import patch
        from urllib.parse import parse_qs, urlsplit
        calls = []
        class Response:
            status = 200
            def __enter__(self): return self
            def __exit__(self, *args): return False
        def fake_urlopen(request, timeout):
            calls.append(request.full_url)
            return Response()
        with patch.object(watchdog.urllib.request, "urlopen", fake_urlopen):
            watchdog.kuma_push("https://monitor.example/api/push/private-token?status=up&msg=OLD&ping=",
                               False, "Probe missed")
        query = parse_qs(urlsplit(calls[0]).query, keep_blank_values=True)
        self.assertEqual(query["status"], ["down"])
        self.assertEqual(query["msg"], ["Probe missed"])
        self.assertEqual(query["ping"], [""])

    def test_watchdog_detects_expired_scheduled_success(self):
        now = dt.datetime.now(dt.timezone.utc)
        runs = {"workflow_runs": [
            {"event": "schedule", "conclusion": "failure", "run_started_at": now.isoformat()},
            {"event": "schedule", "conclusion": "success",
             "run_started_at": (now - dt.timedelta(hours=2)).isoformat()}]}
        self.assertTrue(watchdog.recent_success(runs, now, 54))
        self.assertFalse(watchdog.recent_success(runs, now + dt.timedelta(days=20), 54))


if __name__ == "__main__":
    unittest.main()
