#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""Observe LuaJIT upstream v2.1 drift without moving a pin or claiming ancestry."""
import argparse
import json
import subprocess
from pathlib import Path


def drift(manifest, head):
    modules = json.loads(Path(manifest).read_text(encoding="utf-8"))["modules"]
    source = next(module["sources"][0] for module in modules if module["name"] == "luajit")
    pinned = source["commit"]
    if len(pinned) != 40 or any(char not in "0123456789abcdef" for char in pinned):
        raise ValueError("LuaJIT requires a pinned 40-hex commit")
    return {"key": "luajit-drift", "title": "LuaJIT v2.1 pin needs review",
            "status": "pass" if pinned == head else "fail", "kind": "ci",
            "detail": "Pin matches upstream v2.1" if pinned == head else
                      "Pinned " + pinned[:12] + " differs from upstream v2.1 " + head[:12] +
                      "; inspect history and test before updating (not proof of ancestry)"}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    try:
        result = subprocess.run(
            ["git", "ls-remote", "--heads", "https://github.com/LuaJIT/LuaJIT.git",
             "refs/heads/v2.1"],
            capture_output=True, text=True, timeout=30, check=True)
        lines = result.stdout.splitlines()
        if len(lines) != 1 or not lines[0].endswith("\trefs/heads/v2.1"):
            raise ValueError("LuaJIT v2.1 head missing")
        head = lines[0].split("\t")[0]
        if len(head) != 40 or any(char not in "0123456789abcdef" for char in head):
            raise ValueError("Invalid upstream SHA")
        item = drift(args.manifest, head)
    except (subprocess.SubprocessError, ValueError, OSError) as exc:
        item = {"key": "luajit-drift", "title": "LuaJIT v2.1 check unavailable",
                "status": "fail", "kind": "transient",
                "detail": "Upstream Git reference check failed (" + type(exc).__name__ + ")"}
    Path(args.output).write_text(json.dumps([item], indent=2) + "\n", encoding="utf-8")
    print(item["key"] + ": " + item["status"] + " (" + item["detail"] + ")")


if __name__ == "__main__":
    main()
