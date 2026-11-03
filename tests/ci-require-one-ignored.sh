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
list_log=$(mktemp)
trap 'rm -f "$log" "$list_log"' EXIT
# Cargo's ordinary substring filter can match a second renamed test (for
# example a TV-size suffix), initializing GTK from two different test threads.
# Resolve exactly one fully qualified test first, then use libtest's --exact.
if ! cargo test --locked --bin broadcaster -- --list >"$list_log" 2>&1; then
  cat "$list_log"
  echo "CI test inventory failed: $case_name" >&2
  exit 1
fi
mapfile -t matching_tests < <(awk -v sought="$case_name" '
  /: test$/ {
    name = $0
    sub(/: test$/, "", name)
    count = split(name, components, /::/)
    if (components[count] == sought) print name
  }
' "$list_log")
if (( ${#matching_tests[@]} != 1 )); then
  printf 'CI false-green prevented: exact selector expected one named %s test, found %s\n' \
    "$case_name" "${#matching_tests[@]}" >&2
  printf '%s\n' "${matching_tests[@]}" >&2
  exit 1
fi
exact_name=${matching_tests[0]}
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
    cargo test --locked --bin broadcaster "$exact_name" -- --exact --ignored --test-threads=1 >"$log" 2>&1 || status=$?
else
  cargo test --locked --bin broadcaster "$exact_name" -- --exact --ignored --test-threads=1 >"$log" 2>&1 || status=$?
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
# A zero-match or ambiguous name still fails before execution, and the
# fully qualified name must also appear exactly once in the result log.
if [[ $(grep -Fc "test ${exact_name} ..." "$log" || true) != 1 ]] || \
   [[ $(grep -Ec '^test result: ok\. 1 passed; 0 failed;' "$log" || true) != 1 ]]; then
  echo "CI false-green prevented: expected exactly one passed $case_name" >&2
  exit 1
fi
