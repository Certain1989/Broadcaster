#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
# Cargo succeeds even when a filtered test name matches zero tests.
# Every CI native positive must execute exactly one *named* ignored test.
set -euo pipefail
if (( $# != 1 )) || [[ ! "$1" =~ ^[A-Za-z_][A-Za-z_0-9]*$ ]]; then
  echo 'Expected one Rust test function name (no substring/wildcards)' >&2
  exit 2
fi
case_name=$1
log=$(mktemp)
trap 'rm -f "$log"' EXIT
if ! cargo test --locked "$case_name" -- --ignored --test-threads=1 >"$log" 2>&1; then
  cat "$log"
  echo "CI positive failed: $case_name" >&2
  exit 1
fi
cat "$log"
# Require one exact executed function tail, one passed test total and zero failed.
# GTK may print its own warnings onto the same stdout line between Cargo's
# "test name ..." prefix and the separate "ok" result. Do not require an
# uninterrupted "... ok" line: the one-pass Cargo summary proves success.
# A zero-match or ambiguous substring match still fails the count checks.
if [[ $(grep -Ec "^test ([A-Za-z0-9_]+::)*${case_name} \.\.\." "$log" || true) != 1 ]] || \
   [[ $(grep -Ec '^test result: ok\. 1 passed; 0 failed;' "$log" || true) != 1 ]]; then
  echo "CI false-green prevented: expected exactly one passed $case_name" >&2
  exit 1
fi
