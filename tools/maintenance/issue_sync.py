#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""Deduplicated maintenance issues; previous transient API result is per provider."""
import argparse
import datetime as dt
import io
import json
import os
import re
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

REPO = "Certain1989/Broadcaster"
BASE = "https://api.github.com/repos/" + REPO
BOT_ID = 41898282  # github-actions[bot], immutable GitHub user ID
RESULT_ARTIFACT = "api-probe-results"
MAX_ZIP = 65536
MAX_RESULT = 8192


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


def artifact_bytes(artifact_id):
    if os.environ.get("GITHUB_REPOSITORY") != REPO:
        raise RuntimeError("Unexpected repository")
    if not isinstance(artifact_id, int) or artifact_id <= 0:
        raise ValueError("Invalid artifact ID")
    request = urllib.request.Request(BASE + f"/actions/artifacts/{artifact_id}/zip",
        headers={"Authorization": "Bearer " + os.environ["GH_TOKEN"],
                 "Accept": "application/vnd.github+json",
                 "User-Agent": "Broadcaster-maintenance/1.0"})
    with urllib.request.urlopen(request, timeout=20) as response:
        archive = response.read(MAX_ZIP + 1)
    if len(archive) > MAX_ZIP:
        raise ValueError("Previous probe artifact ZIP exceeds safety limit")
    with zipfile.ZipFile(io.BytesIO(archive)) as zf:
        files = zf.infolist()
        if len(files) != 1 or files[0].filename != "api-results.json" or files[0].file_size > MAX_RESULT:
            raise ValueError("Unexpected result artifact structure")
        with zf.open(files[0]) as file:
            result = file.read(MAX_RESULT + 1)
    if len(result) > MAX_RESULT:
        raise ValueError("Previous probe result too large")
    return result


def previous_scheduled_results(api=github, read_artifact=artifact_bytes, now=None):
    """Only the immediately preceding completed scheduled run can count.

    A run without its expected result artifact or an excessively old run does
    not establish recurrence. Never infer an individual provider from the
    workflow's global failure conclusion.
    """
    now = now or dt.datetime.now(dt.timezone.utc)
    runs = api("GET", "/actions/workflows/api-compatibility.yml/"
               "runs?event=schedule&status=completed&per_page=10")
    current = str(os.environ.get("GITHUB_RUN_ID", ""))
    candidates = [run for run in runs.get("workflow_runs", [])
                  if run.get("event") == "schedule"
                  and run.get("head_branch") == "main"
                  and run.get("status") == "completed"
                  and str(run.get("id")) != current]
    if not candidates:
        return None
    newest = max(candidates, key=lambda run: run.get("created_at", ""))
    try:
        created = dt.datetime.fromisoformat(newest["created_at"].replace("Z", "+00:00"))
    except (ValueError, TypeError, KeyError):
        return None
    age = now - created
    if not dt.timedelta(0) <= age <= dt.timedelta(hours=54):
        return None
    artifacts = api("GET", f"/actions/runs/{newest['id']}/artifacts?per_page=100")
    matching = [a for a in artifacts.get("artifacts", [])
                if a.get("name") == RESULT_ARTIFACT and not a.get("expired", False)]
    if len(matching) != 1:
        return None
    try:
        results = json.loads(read_artifact(matching[0]["id"]))
    except (ValueError, KeyError, TypeError, zipfile.BadZipFile):
        return None
    if not isinstance(results, list) or len(results) != 2:
        return None
    found = {}
    for item in results:
        if not isinstance(item, dict) or item.get("key") not in ("api-mediathek", "api-zapp"):
            return None
        if item.get("status") not in ("pass", "fail") or item.get("kind") not in ("none", "contract", "transient"):
            return None
        if item["key"] in found:
            return None
        found[item["key"]] = item
    if set(found) != {"api-mediathek", "api-zapp"}:
        return None
    return found


def existing_issues(api=github):
    issues = []
    for page in range(1, 21):
        batch = api("GET", f"/issues?state=open&per_page=100&page={page}")
        issues.extend(item for item in batch if "pull_request" not in item)
        if len(batch) < 100:
            return issues
    raise RuntimeError("Issue pagination exhausted; cannot safely deduplicate")


def owned_issue(issue, key):
    user = issue.get("user") or {}
    return (user.get("id") == BOT_ID
            and (issue.get("body") or "").startswith(marker(key) + "\n"))


def report_body(item, run_url):
    detail = str(item["detail"]).replace("\r", " ").replace("\n", " ")[:300]
    return (marker(item["key"]) + "\n"
            "Automated maintenance check; no user data or upstream response body included.\n\n"
            "**Finding:** " + detail + "\n\n"
            "**Latest run:** " + run_url + "\n\n"
            "Repeated failures update this issue; a healthy check closes it. "
            "An upstream outage alone does not prove an application regression.\n")


def synchronize(results, api=github, run_url=None, now=None, previous=None):
    run_url = run_url or ("https://github.com/" + REPO + "/actions/runs/" +
                          os.environ.get("GITHUB_RUN_ID", "0"))
    if not isinstance(results, list) or not results:
        raise ValueError("Expected nonempty list of probe results")
    for item in results:
        marker(item["key"])
        if item.get("status") not in ("pass", "fail") or item.get("kind") not in ("none", "contract", "transient", "ci"):
            raise ValueError("Invalid maintenance result")
    issues = existing_issues(api)
    # Fetch previous result only when needed, once per run. 'previous' injection
    # is used by offline regression tests and does not bypass runtime validation.
    unresolved_transient = any(item["status"] == "fail" and item["kind"] == "transient"
                               and item["key"] in ("api-mediathek", "api-zapp")
                               for item in results)
    if unresolved_transient and previous is None:
        try:
            previous = previous_scheduled_results(api=api, now=now)
        except (ValueError, OSError, TimeoutError, urllib.error.URLError,
                zipfile.BadZipFile, TypeError, KeyError):
            # Missing GitHub Actions API/artifact evidence cannot justify
            # an incident for a first transient failure. Still process any
            # independent contract failures in the same result set.
            print("Previous provider evidence unavailable; deferring transient alerts")
            previous = {}
    for item in results:
        matching = [issue for issue in issues if owned_issue(issue, item["key"])]
        if len(matching) > 1:
            raise RuntimeError("Multiple bot-owned maintenance issues for " + item["key"])
        issue = matching[0] if matching else None
        if item["status"] == "pass":
            if issue:
                api("PATCH", "/issues/" + str(issue["number"]), {"state": "closed"})
                print("Closed:", item["key"])
            continue
        if (item["kind"] == "transient" and not issue
                and item["key"] in ("api-mediathek", "api-zapp")
                and not (isinstance(previous, dict)
                         and isinstance(previous.get(item["key"]), dict)
                         and previous[item["key"]].get("status") == "fail")):
            print("First or nonconsecutive provider failure:", item["key"])
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
        # A local runner/audit failure is not a provider outage.
    synchronize(results)


if __name__ == "__main__":
    main()
