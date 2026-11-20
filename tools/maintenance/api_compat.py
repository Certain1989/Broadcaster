#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""Bounded read-only upstream API compatibility checks; never retrieve media."""
import argparse
import datetime as dt
import json
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

MAX_BYTES = 2 * 1024 * 1024
USER_AGENT = "Broadcaster-maintenance/1.0 (+https://github.com/Certain1989/Broadcaster)"


class ProbeError(Exception):
    def __init__(self, kind, message):
        self.kind = kind
        super().__init__(message)


def request_json(url, payload=None):
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        url, data=data,
        headers={"User-Agent": USER_AGENT, "Accept": "application/json",
                 "Content-Type": "application/json"},
        method="POST" if data is not None else "GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            raw = response.read(MAX_BYTES + 1)
    except urllib.error.HTTPError as exc:
        raise ProbeError("transient" if exc.code >= 500 or exc.code == 429 else "contract",
                         "HTTP status " + str(exc.code)) from None
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise ProbeError("transient", "Network request failed (" + type(exc).__name__ + ")") from None
    if len(raw) > MAX_BYTES:
        raise ProbeError("contract", "Response exceeds 2 MiB")
    try:
        return json.loads(raw)
    except (ValueError, UnicodeError):
        raise ProbeError("contract", "Response is not valid JSON") from None


def require(condition, message):
    if not condition:
        raise ProbeError("contract", message)


def check_mediathek(fetch=request_json):
    base = "https://mediathekviewweb.de/api"
    channels = fetch(base + "/channels")
    require(isinstance(channels, dict) and isinstance(channels.get("channels"), list),
            "Mediathek channels response changed shape")
    if not channels["channels"]:
        raise ProbeError("transient", "Mediathek catalogue empty")
    require(all(isinstance(x, str) and x.strip() for x in channels["channels"]),
            "Mediathek channel names invalid")
    query = {"queries": [{"fields": ["title", "topic"], "query": "Tagesschau"}],
             "sortBy": "timestamp", "sortOrder": "desc", "future": False,
             "offset": 0, "size": 1}
    response = fetch(base + "/query", query)
    require(isinstance(response, dict) and isinstance(response.get("result"), dict),
            "Mediathek search response lacks result")
    result = response["result"]
    require(isinstance(result.get("results"), list) and isinstance(result.get("queryInfo"), dict),
            "Mediathek results/queryInfo changed shape")
    count = result["queryInfo"].get("totalResults")
    require(type(count) is int and count >= 0, "Mediathek totalResults invalid")
    for item in result["results"]:
        require(isinstance(item, dict) and all(isinstance(item.get(k), str)
                for k in ("title", "channel", "topic")), "Mediathek result fields invalid")
    return "Channel catalogue and bounded search response structurally valid"


def check_zapp(fetch=request_json):
    base = "https://api.zapp.mediathekview.de/v1"
    channels = fetch(base + "/channelInfoList")
    require(isinstance(channels, dict) and bool(channels),
            "Zapp channelInfoList not a nonempty object")
    for key, channel in channels.items():
        require(isinstance(key, str) and bool(key) and isinstance(channel, dict)
                and isinstance(channel.get("name"), str)
                and isinstance(channel.get("streamUrl"), str),
                "Zapp channelInfoList fields invalid")
    channel_id = next((key for key in ("das_erste", "zdf") if key in channels),
                      next(iter(channels)))
    shows = fetch(base + "/shows/" + urllib.parse.quote(channel_id, safe=""))
    require(isinstance(shows, dict) and isinstance(shows.get("shows"), list),
            "Zapp shows response changed shape or returned error")
    for show in shows["shows"]:
        require(isinstance(show, dict) and isinstance(show.get("title"), str)
                and isinstance(show.get("channel"), str),
                "Zapp show title/channel invalid")
        for field in ("startTime", "endTime"):
            value = show.get(field)
            require(isinstance(value, str), "Zapp show lacks " + field)
            try:
                timestamp = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
                require(timestamp.tzinfo is not None, "Zapp show timestamp lacks timezone")
            except ValueError:
                raise ProbeError("contract", "Zapp show has invalid " + field) from None
    return "Channel list and programme response structurally valid"


def probe(name, callback, retries=2, pause=time.sleep):
    for attempt in range(retries + 1):
        try:
            return {"key": "api-" + name, "title": name + " API compatibility",
                    "status": "pass", "kind": "none", "detail": callback()}
        except ProbeError as exc:
            if exc.kind != "transient" or attempt == retries:
                return {"key": "api-" + name, "title": name + " API compatibility",
                        "status": "fail", "kind": exc.kind, "detail": str(exc)}
            pause(5)
    raise AssertionError("unreachable")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    results = [probe("mediathek", check_mediathek), probe("zapp", check_zapp)]
    args.output.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    for result in results:
        print(f"{result['key']}: {result['status']} ({result['kind']}: {result['detail']})")
    return int(any(result["status"] != "pass" for result in results))


if __name__ == "__main__":
    raise SystemExit(main())
