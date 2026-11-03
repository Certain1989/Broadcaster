#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
# Exercise the real native CI selection helper with a controlled Cargo test inventory.
# A function-name prefix must not run a second GTK test in the same process.
set -euo pipefail
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cat >"$scratch/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
base=streams::management::native_navigation_test_support
if [[ " $* " == *' --list '* ]]; then
  case ${MOCK_INVENTORY:-prefix} in
    prefix)
      printf '%s\n' \
        "$base::source_manager_actual_remote_back_trace: test" \
        "$base::source_manager_actual_remote_back_trace_tv: test"
      ;;
    duplicate)
      printf '%s\n' \
        "$base::source_manager_actual_remote_back_trace: test" \
        "another::source_manager_actual_remote_back_trace: test"
      ;;
    zero)
      printf '%s\n' "$base::unrelated_test: test" ;;
    *) exit 99 ;;
  esac
  exit 0
fi
# For the positive mock, the helper must pass the complete path and --exact.
[[ " $* " == *' --exact '* ]] || exit 90
[[ " $* " == *' --ignored '* ]] || exit 91
wanted="$base::source_manager_actual_remote_back_trace"
for arg in "$@"; do
  if [[ "$arg" == "$wanted" ]]; then
    printf 'test %s ... ok\n' "$wanted"
    printf '\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out\n'
    exit 0
  fi
done
exit 92
MOCK
chmod +x "$scratch/cargo"
export PATH="$scratch:$PATH"
export BROADCASTER_NATIVE_CASE_TIMEOUT_SECONDS=10

MOCK_INVENTORY=prefix bash tests/ci-require-one-ignored.sh source_manager_actual_remote_back_trace >"$scratch/log" 2>&1 || {
  cat "$scratch/log"; echo 'Exact prefix isolation positive failed' >&2; exit 1;
}
grep -Fq '1 passed; 0 failed' "$scratch/log"

if MOCK_INVENTORY=duplicate bash tests/ci-require-one-ignored.sh source_manager_actual_remote_back_trace >"$scratch/log" 2>&1; then
  echo 'Ambiguous same-leaf test inventory was wrongly accepted' >&2; exit 1
fi
grep -Fq 'expected one named source_manager_actual_remote_back_trace test, found 2' "$scratch/log"

if MOCK_INVENTORY=zero bash tests/ci-require-one-ignored.sh source_manager_actual_remote_back_trace >"$scratch/log" 2>&1; then
  echo 'Zero-match test inventory was wrongly accepted' >&2; exit 1
fi
grep -Fq 'expected one named source_manager_actual_remote_back_trace test, found 0' "$scratch/log"
echo 'Native CI exact selection: prefix isolation and ambiguous/zero-match negatives PASS'
