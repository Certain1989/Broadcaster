#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""Fail-closed handoff between a read-only upstream checker and a PR publisher."""
import argparse
import copy
import json
import os
import re
import shutil
import subprocess
from pathlib import Path

MANIFEST = "build-aux/io.github.Certain1989.Broadcaster.json"
COMPONENTS = frozenset({"mpv", "libass", "libplacebo", "blueprint-compiler"})
LIMIT = 1024 * 1024


def run_git(*args):
    return subprocess.run(["git", *args], capture_output=True, check=True).stdout


def names(*args):
    return set(run_git(*args).decode().strip("\0").split("\0")) - {""}


def assert_worktree(allow_manifest=False):
    # Staging a candidate from a contaminated index would commit unrelated files.
    if names("diff", "--cached", "--name-only", "-z"):
        raise ValueError("Unexpected staged changes (index must be empty)")
    tracked = names("diff", "--name-only", "-z")
    if tracked - ({MANIFEST} if allow_manifest else set()):
        raise ValueError("Unexpected modified tracked file")
    if names("ls-files", "--others", "--exclude-standard", "-z"):
        raise ValueError("Unexpected untracked file")
    if subprocess.run(["git", "diff", "--check"], capture_output=True).returncode:
        raise ValueError("Unexpected whitespace/diff errors")
    path = Path(MANIFEST)
    if path.is_symlink() or not path.is_file():
        raise ValueError("Manifest must be a regular file")


def read_manifest(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > LIMIT:
        raise ValueError("Missing, symlinked or oversized manifest")
    return json.loads(path.read_text(encoding="utf-8"))


def validate(baseline, proposed):
    if not isinstance(baseline, dict) or not isinstance(proposed, dict):
        raise ValueError("Manifest top level must be an object")
    old_modules = baseline.get("modules")
    new_modules = proposed.get("modules")
    if not isinstance(old_modules, list) or not isinstance(new_modules, list):
        raise ValueError("Manifest must contain modules")
    names_old = [m.get("name") for m in old_modules]
    names_new = [m.get("name") for m in new_modules]
    if any(not isinstance(n, str) for n in names_old) or len(set(names_old)) != len(names_old):
        raise ValueError("Duplicate or invalid source module")
    if names_old != names_new:
        raise ValueError("Module identities or ordering changed")
    expected = copy.deepcopy(baseline)
    changes = []
    for index, (before, after) in enumerate(zip(old_modules, new_modules)):
        name = before["name"]
        if name not in COMPONENTS:
            continue
        old_sources, new_sources = before.get("sources"), after.get("sources")
        if not isinstance(old_sources, list) or len(old_sources) != 1 or not isinstance(new_sources, list) or len(new_sources) != 1:
            raise ValueError("Expected one source in " + name)
        original, candidate = old_sources[0], new_sources[0]
        if original.get("type") != "git" or candidate.get("type") != "git":
            raise ValueError("Source type changed")
        tag, commit = candidate.get("tag"), candidate.get("commit")
        if not isinstance(tag, str) or not re.fullmatch(r"v?[0-9]+\.[0-9]+\.[0-9]+", tag):
            raise ValueError("Unexpected upstream version tag")
        if not isinstance(commit, str) or not re.fullmatch("[0-9a-f]{40}", commit):
            raise ValueError("Expected full pinned Git commit")
        for field in ("tag", "commit"):
            expected["modules"][index]["sources"][0][field] = candidate[field]
        if original["tag"] != tag or original["commit"] != commit:
            if original["tag"] == tag or original["commit"] == commit:
                raise ValueError("Tag and pin must advance together")
            changes.append(name)
    if proposed != expected:
        raise ValueError("Manifest changes extend beyond four approved tag/commit pairs")
    return changes


def baseline():
    return json.loads(run_git("show", "HEAD:" + MANIFEST))


def check(destination, output):
    assert_worktree(allow_manifest=True)
    changes = validate(baseline(), read_manifest(MANIFEST))
    if changes:
        if not destination:
            raise ValueError("Missing output candidate path")
        destination = Path(destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(MANIFEST, destination)
    if output:
        with open(output, "a", encoding="utf-8") as file:
            file.write("changed=" + ("true" if changes else "false") + "\n")
    print("Approved native source changes:", ", ".join(changes) if changes else "none")
    return changes


def apply(candidate):
    assert_worktree(allow_manifest=False)
    changes = validate(read_manifest(MANIFEST), read_manifest(candidate))
    if not changes:
        raise ValueError("Candidate artifact contains no approved update")
    shutil.copyfile(candidate, MANIFEST)
    run_git("add", "--", MANIFEST)
    if names("diff", "--cached", "--name-only", "-z") != {MANIFEST}:
        raise ValueError("Unexpected staged content after applying candidate")
    if subprocess.run(["git", "diff", "--cached", "--check"], capture_output=True).returncode:
        raise ValueError("Candidate staged diff is not clean")
    print("Staged approved changes:", ", ".join(changes))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("check", "apply"))
    parser.add_argument("--candidate")
    parser.add_argument("--output")
    args = parser.parse_args()
    if args.mode == "check":
        check(args.candidate, args.output)
    else:
        if not args.candidate:
            parser.error("apply requires --candidate")
        apply(args.candidate)


if __name__ == "__main__":
    main()
