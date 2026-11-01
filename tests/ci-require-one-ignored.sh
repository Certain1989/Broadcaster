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
# Full CI sets an explicit per-case deadline; fast/local callers retain
# the original unlimited default unless they opt in. Preserve the exact
# one-test success oracle below regardless of the execution method.
case_timeout=${BROADCASTER_NATIVE_CASE_TIMEOUT_SECONDS:-}
status=0
if [[ -n "$case_timeout" ]]; then
  if [[ ! "$case_timeout" =~ ^[1-9][0-9]*$ ]]; then
    echo "Invalid BROADCASTER_NATIVE_CASE_TIMEOUT_SECONDS" >&2
    exit 2
  fi
  echo "CI native case=$case_name deadline=${case_timeout}s"
  timeout --signal=TERM --kill-after=15s "${case_timeout}s" \
    cargo test --locked "$case_name" -- --ignored --test-threads=1 >"$log" 2>&1 || status=$?
else
  cargo test --locked "$case_name" -- --ignored --test-threads=1 >"$log" 2>&1 || status=$?
fi
if (( status != 0 )); then
  cat "$log"
  if (( status == 124 || status == 137 )); then
    echo "CI native case timeout: $case_name deadline=${case_timeout}s status=$status" >&2
  else
    echo "CI positive failed: $case_name status=$status" >&2
  fi
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
