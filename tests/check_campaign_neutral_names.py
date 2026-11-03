#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Reject development campaign identifiers in executable source and automation."""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCOPES = ("src", "tests", "data", "build-aux", ".github", "po")
ROOT_FILES = ("build.rs", "Cargo.toml", "meson.build", "justfile")
PATTERN = re.compile(
    r"(?<![A-Za-z0-9])(?:"
    + "|".join((
        r"r0[0-9]", r"g0[0-9]", r"focus[-_]0[0-9]+",
        r"m1[-_]", r"d(?:29|35|38)[-_]", r"c[124][-_]",
        r"e[234][-_]", r"b0[0-9][-_]", r"p1[-_]",
        r"g1[-_]", r"d1[-_]", r"u1[-_]", r"r08",
        r"ops[-_]0[0-9]",
        r"as(?:10|13|15)(?=[_\\-]|\\b)",
        r"d(?:01|02|03|04|05|08|09|10)(?=[_\\-]|\\b)",
        r"e(?:01|03|06|07|10|11)(?=[_\\-]|\\b)",
        r"b(?:10|11|14)(?=[_\\-]|\\b)",
        r"f(?:02|10)(?=[_\\-]|\\b)",
    ))
    + r")",
    re.IGNORECASE,
)
# Actual Unicode C1 controls (U+0080–U+009F) are legitimate XMLTV terms,
# not source campaign names. Exceptions are exact symbol names and files only.
C1_DOMAIN_EXCEPTIONS = {
    "src/epg/xmltv.rs": (
        "allow_c1_omission", "omitted_c1_programmes", "parse_with_c1_omission"
    ),
    "src/epg/ingest.rs": ("omitted_c1_programmes",),
    "src/epg/desktop.rs": ("omitted_c1_programmes",),
    "src/epg/capacity_bench.rs": ("omitted_c1_programmes",),
}
EXCLUDED_FILES = {"tests/check_campaign_neutral_names.py"}

def allowed(rel: str, line: str, position: int) -> bool:
    for name in C1_DOMAIN_EXCEPTIONS.get(rel, ()):
        for hit in re.finditer(re.escape(name), line):
            if hit.start() <= position < hit.end():
                return True
    return False

def violations(rel: str, contents: str) -> list[str]:
    errors = []
    if PATTERN.search(rel):
        errors.append(f"{rel}: numbered executable filename")
    for no, line in enumerate(contents.splitlines(), 1):
        for match in PATTERN.finditer(line):
            if not allowed(rel, line, match.start()):
                errors.append(f"{rel}:{no}: {line.strip()[:200]}")
    return errors

def self_test() -> None:
    assert violations("src/window/r07_focus.rs", "fn r07_focus() {}")
    assert violations("src/test.rs", "NATIVE_FOCUS_AS10_EXPECTED_NEGATIVE_ORACLE")
    assert violations("src/test.rs", 'let status = "G09_EXPECTED_NEGATIVE";')
    assert violations(".github/workflows/ci.yaml", "cargo test c4_native_guide")
    assert violations("src/epg/xmltv.rs", "fn r07_unrelated() {}")
    assert not violations("src/epg/xmltv.rs", "let omitted_c1_programmes = 1;")
    assert not violations("src/epg/xmltv.rs", "let allow_c1_omission = true;")
    assert not violations("src/xmltv.rs", 'let title = "C1";')
    assert not violations("src/epg/xmltv.rs", "let stage = RefreshStage::Idle;")
    assert not violations("src/epg/cache.rs", 'let p = "cache.sqlite3-wal";')
    assert not violations("src/epg/cache.rs", 'let p = "cache.sqlite3-shm";')

def main() -> int:
    self_test()
    errors = []
    paths = [p for scope in SCOPES for p in (ROOT / scope).rglob("*") if p.is_file()]
    paths += [ROOT / p for p in ROOT_FILES if (ROOT / p).is_file()]
    for path in paths:
        rel = path.relative_to(ROOT).as_posix()
        if rel in EXCLUDED_FILES:
            continue
        try:
            contents = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        errors.extend(violations(rel, contents))
    if errors:
        print("Campaign-neutral naming preflight FAILED:", file=sys.stderr)
        print("\n".join(errors[:80]), file=sys.stderr)
        if len(errors) > 80:
            print(f"... {len(errors) - 80} more", file=sys.stderr)
        return 1
    print("Campaign-neutral naming preflight PASS (technical exceptions explicit)")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
