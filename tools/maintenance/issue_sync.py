#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""Manage bounded, deduplicated maintenance issues with the workflow token."""
import argparse
import datetime as dt
import json
import os
import re
import urllib.request
from pathlib import Path

REPO = "Certain1989/Broadcaster"
BASE = "https://api.github.com/repos/" + REPO


def marker(key):
    if not re.fullmatch(r"[a-z][a-z0-9-]{1,55}", key):
        raise ValueError("Invalid maintenance issue key")
    return "<!-- broadcaster-maintenance:" + key + " -->"


def github(method, path, payload=None):
    if os.environ.get("GITHUB_REPOSITORY") != REPO:
        raise RuntimeError("Refusing to modify unexpected repository")
    token = os.environ["GH_TOKEN"]
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(BASE + path, data=data, method=method,
        headers={"Authorization": "Bearer " + token,
                 "Accept": "application/vnd.github+json",
                 "X-GitHub-Api-Version": "2022-11-28",
                 "User-Agent": "Broadcaster-maintenance/1.0"})
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def existing_issues(api=github):
    issues = []
    for page in range(1, 6):
        batch = api("GET", f"/issues?state=open&per_page=100&page={page}")
        issues.extend(item for item in batch if "pull_request" not in item)
        if len(batch) < 100:
            break
    return issues


def repeated_transient(api=github, now=None, workflow="api-compatibility.yml"):
    """Defer opening incidents for initial provider outages (not schema changes)."""
    now = now or dt.datetime.now(dt.timezone.utc)
    runs = api("GET", "/actions/workflows/" + workflow +
               "/runs?event=schedule&status=completed&per_page=5")
    hours = 240 if workflow == "native-dependencies.yml" else 72
    for run in runs.get("workflow_runs", []):
        created = dt.datetime.fromisoformat(run["created_at"].replace("Z", "+00:00"))
        if now - created > dt.timedelta(hours=hours):
            return False
        return run.get("conclusion") == "failure"
    return False


def report_body(item, run_url):
    detail = item["detail"].replace("\r", " ").replace("\n", " ")[:300]
    return (marker(item["key"]) + "\n"
            "Automated maintenance check; no user data or upstream response body included.\n\n"
            "**Finding:** " + detail + "\n\n"
            "**Latest run:** " + run_url + "\n\n"
            "Repeated failures update this issue; a healthy check closes it. "
            "An upstream outage alone does not prove an application regression.\n")


def synchronize(results, api=github, run_url=None, now=None):
    run_url = run_url or ("https://github.com/" + REPO + "/actions/runs/" +
                          os.environ.get("GITHUB_RUN_ID", "0"))
    issues = existing_issues(api)
    for item in results:
        key = marker(item["key"])
        matching = [issue for issue in issues if key in (issue.get("body") or "")]
        if len(matching) > 1:
            raise RuntimeError("Multiple existing maintenance issues for " + item["key"])
        issue = matching[0] if matching else None
        if item["status"] == "pass":
            if issue:
                api("PATCH", "/issues/" + str(issue["number"]), {"state": "closed"})
                print("Closed:", item["key"])
            continue
        if item["status"] != "fail":
            raise ValueError("Invalid maintenance status")
        workflow = "native-dependencies.yml" if item["key"] == "luajit-drift" else "api-compatibility.yml"
        if item["kind"] == "transient" and not issue and not repeated_transient(api, now, workflow):
            print("First transient failure; deferring issue:", item["key"])
            continue
        body = report_body(item, run_url)
        if issue:
            if issue.get("body") != body:
                api("PATCH", "/issues/" + str(issue["number"]), {"body": body})
            print("Updated:", item["key"])
        else:
            created = api("POST", "/issues",
                          {"title": "[Maintenance] " + item["title"], "body": body})
            issues.append(created)
            print("Opened:", item["key"])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--results", type=Path)
    parser.add_argument("--key")
    parser.add_argument("--title")
    parser.add_argument("--status", choices=("pass", "fail"))
    parser.add_argument("--detail", default="See linked workflow run")
    args = parser.parse_args()
    if args.results:
        results = json.loads(args.results.read_text(encoding="utf-8"))
    else:
        if not all((args.key, args.title, args.status)):
            parser.error("Provide --results or --key, --title and --status")
        results = [{"key": args.key, "title": args.title, "status": args.status,
                    "kind": "ci", "detail": args.detail}]
    synchronize(results)


if __name__ == "__main__":
    main()
