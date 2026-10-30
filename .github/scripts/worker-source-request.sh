#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Certain1989
# SPDX-License-Identifier: CC0-1.0
# Resolve an authorised source export request without evaluating comment text.
set -euo pipefail

event_name="${1:?event name required}"
event_file="${2:?event payload path required}"
requested_sha="${3:-}"
push_sha="${4:-}"

case "$event_name" in
  issue_comment)
    # Defense in depth: only the designated issue, owner, created event and repository.
    jq -e '
      .action == "created" and
      .repository.full_name == "Certain1989/Broadcaster" and
      .issue.number == 56 and
      (.issue.pull_request == null) and
      .comment.user.login == "Certain1989"
    ' "$event_file" >/dev/null || { echo "Unauthorised source request" >&2; exit 1; }
    # A terminal sentinel preserves trailing whitespace/newlines from the JSON field.
    comment="$(jq -rj '.comment.body // ""' "$event_file"; printf '\037')"
    comment="${comment%$'\037'}"
    if [[ "$comment" =~ ^/source-bundle\ ([0-9a-f]{40})$ ]]; then
      sha="${BASH_REMATCH[1]}"
    else
      echo "Invalid source request (expected /source-bundle followed by a lowercase 40-hex SHA)" >&2
      exit 1
    fi
    ;;
  workflow_dispatch)
    sha="${requested_sha:-$push_sha}"
    ;;
  push)
    sha="$push_sha"
    ;;
  *)
    echo "Unsupported source request event" >&2
    exit 1
    ;;
esac

[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "Invalid source commit SHA" >&2; exit 1; }
printf '%s\n' "$sha"
