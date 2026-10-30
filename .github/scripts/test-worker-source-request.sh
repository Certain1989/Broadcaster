#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Certain1989
# SPDX-License-Identifier: CC0-1.0
set -euo pipefail

script="$(dirname "${BASH_SOURCE[0]}")/worker-source-request.sh"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
sha='0123456789abcdef0123456789abcdef01234567'
alt='abcdef0123456789abcdef0123456789abcdef01'

make_event() {
  local body="$1" actor="${2:-Certain1989}" issue="${3:-56}" repo="${4:-Certain1989/Broadcaster}" action="${5:-created}" is_pr="${6:-false}"
  jq -nc --arg body "$body" --arg actor "$actor" --argjson issue "$issue" --arg repo "$repo" --arg action "$action" --argjson is_pr "$is_pr" \
    '{action:$action,repository:{full_name:$repo},issue:{number:$issue,pull_request:(if $is_pr then {url:"https://example.invalid"} else null end)},comment:{user:{login:$actor},body:$body}}' > "$root/event.json"
}
expect_ok() {
  local expected="$1"; shift
  local got
  got="$(bash "$script" "$@")" || { echo "Expected success: $*" >&2; exit 1; }
  [[ "$got" == "$expected" ]] || { echo "Incorrect resolved SHA: $got" >&2; exit 1; }
}
expect_fail() {
  if bash "$script" "$@" >"$root/out" 2>"$root/err"; then
    echo "Unexpected success: $*" >&2; exit 1
  fi
}

make_event "/source-bundle $sha"
expect_ok "$sha" issue_comment "$root/event.json" "" "$alt"
make_event "/source-bundle $sha" OtherUser
expect_fail issue_comment "$root/event.json" "" "$alt"
make_event "/source-bundle $sha" Certain1989 57
expect_fail issue_comment "$root/event.json" "" "$alt"
make_event "/source-bundle $sha" Certain1989 56 OtherOrg/Broadcaster
expect_fail issue_comment "$root/event.json" "" "$alt"
make_event "/source-bundle $sha" Certain1989 56 Certain1989/Broadcaster edited
expect_fail issue_comment "$root/event.json" "" "$alt"
make_event "/source-bundle $sha" Certain1989 56 Certain1989/Broadcaster created true
expect_fail issue_comment "$root/event.json" "" "$alt"
for bad in \
  "/source-bundle ${sha}xyz" \
  "/source-bundle $(printf '%s' "$sha" | tr 'abcdef' 'ABCDEF')" \
  "/source-bundle $sha; echo UNSAFE" \
  "/source-bundle $sha " \
  "/source-bundle $sha"$'\n' \
  "prefix /source-bundle $sha" \
  '/source-bundle deadbeef'; do
  make_event "$bad"
  expect_fail issue_comment "$root/event.json" "" "$alt"
done
expect_ok "$sha" workflow_dispatch "$root/event.json" "$sha" "$alt"
expect_ok "$alt" workflow_dispatch "$root/event.json" "" "$alt"
expect_ok "$alt" push "$root/event.json" "$sha" "$alt"
expect_fail workflow_dispatch "$root/event.json" 'bad input' "$alt"
expect_fail push "$root/event.json" "" 'not-a-sha'
expect_fail unknown "$root/event.json" "$sha" "$alt"
printf 'Worker source request parser: positive and negative cases PASS\n'
