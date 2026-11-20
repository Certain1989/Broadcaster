#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""Run outside GitHub; monitor whether scheduled checks are actually succeeding."""
import datetime as dt
import json
import os
import urllib.error
import urllib.parse
import urllib.request

WORKFLOWS = {"api-compatibility.yml": 54, "native-dependencies.yml": 240,
             "rustsec-weekly.yml": 240}
API = "https://api.github.com/repos/Certain1989/Broadcaster/actions/workflows/"


def recent_success(runs, now, threshold_hours):
    for run in runs.get("workflow_runs", []):
        if run.get("event") != "schedule" or run.get("conclusion") != "success":
            continue
        stamp = run.get("run_started_at") or run.get("created_at")
        if not stamp:
            continue
        timestamp = dt.datetime.fromisoformat(stamp.replace("Z", "+00:00"))
        if dt.timedelta(0) <= now - timestamp <= dt.timedelta(hours=threshold_hours):
            return True
    return False


def fetch_runs(workflow):
    request = urllib.request.Request(
        API + urllib.parse.quote(workflow, safe="") +
        "/runs?event=schedule&status=completed&per_page=30",
        headers={"Accept": "application/vnd.github+json",
                 "User-Agent": "Broadcaster-watchdog/1.0"})
    with urllib.request.urlopen(request, timeout=20) as response:
        payload = response.read(512 * 1024 + 1)
    if len(payload) > 512 * 1024:
        raise ValueError("GitHub run response too large")
    return json.loads(payload)


def kuma_push(url, healthy, summary):
    if not url:
        return
    # URL includes a private push token; never print or log it.
    parts = urllib.parse.urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.netloc or parts.username or parts.password:
        raise ValueError("Invalid KUMA_PUSH_URL")
    # Uptime Kuma's generated push URL may already contain status/msg parameters.
    # Replace them, do not append duplicates that could be interpreted as arrays.
    query = [(key, value) for key, value in
             urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
             if key not in ("status", "msg")]
    query.extend([("status", "up" if healthy else "down"), ("msg", summary[:140])])
    target = urllib.parse.urlunsplit(parts._replace(query=urllib.parse.urlencode(query)))
    try:
        with urllib.request.urlopen(urllib.request.Request(
                target, headers={"User-Agent": "Broadcaster-watchdog/1.0"}), timeout=15) as response:
            if response.status != 200:
                raise RuntimeError("Kuma push returned non-200")
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        # Never include a credential-bearing push URL in diagnostics.
        raise RuntimeError("Kuma push failed (" + type(exc).__name__ + ")") from None


def main():
    now = dt.datetime.now(dt.timezone.utc)
    failures = []
    for workflow, hours in WORKFLOWS.items():
        try:
            if not recent_success(fetch_runs(workflow), now, hours):
                failures.append(workflow + " has no recent successful scheduled run")
        except (ValueError, urllib.error.URLError, TimeoutError, OSError) as exc:
            failures.append(workflow + ": GitHub API unavailable (" + type(exc).__name__ + ")")
    summary = "; ".join(failures) if failures else "All maintenance schedules healthy"
    print(summary)
    kuma_push(os.environ.get("KUMA_PUSH_URL"), not failures, summary)
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
