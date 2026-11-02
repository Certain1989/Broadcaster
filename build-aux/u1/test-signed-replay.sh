#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
# U1 NONPRODUCTION diagnostic: does a normal Flatpak client follow a replayed,
# still-valid older signed repository at the *same* HTTP origin?
# This is observational, not an anti-rollback PASS assertion.
set -euo pipefail
: "$RUNNER_TEMP"
root="$RUNNER_TEMP/u1-signed-replay"
app=io.github.Certain1989.Broadcaster.Devel
remote=broadcaster-u1-replay
arch=$(flatpak --default-arch)
ref="app/$app/$arch/master"
old=$(ostree --repo="$RUNNER_TEMP/u1-v1/repo" rev-parse "$ref")
new=$(ostree --repo="$RUNNER_TEMP/u1-v2/repo" rev-parse "$ref")
test -n "$old" && test -n "$new" && test "$old" != "$new"
test -s "$RUNNER_TEMP/u1-v1/repo/summary.sig"
test -s "$RUNNER_TEMP/u1-v2/repo/summary.sig"
test -n "$(find "$RUNNER_TEMP/u1-v1/repo/summaries" -type f -name '*.idx.sig' -print -quit)"
mkdir -p "$root"
ln -s "$RUNNER_TEMP/u1-v2/repo" "$root/current"
python3 -u build-aux/u1/http-meter.py 18393 "$root/current" >"$root/http.log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  if curl -fsS http://127.0.0.1:18393/summary >/dev/null 2>&1; then break; fi
  sleep 0.3
done
cmp -s <(curl -fsS http://127.0.0.1:18393/summary) "$RUNNER_TEMP/u1-v2/repo/summary"
sed -e "s#^Name=.*#Name=$remote#" \
  -e 's#^Url=.*#Url=http://127.0.0.1:18393#' \
  "$RUNNER_TEMP/u1-v2/u1-test.flatpakrepo" >"$root/replay.flatpakrepo"
export XDG_DATA_HOME="$root/data" XDG_CACHE_HOME="$root/cache" XDG_CONFIG_HOME="$root/config"
flatpak --user remote-add --from "$remote" "$root/replay.flatpakrepo"
flatpak --user install -y --noninteractive --no-deps --no-related "$remote" "$app"
test "$(flatpak --user info --show-commit "$app")" = "$new"
installed=$(flatpak --user info --show-location "$app")
grep -Fx 'u1-nonproduction-content-revision-2' \
  "$installed/files/share/broadcaster/u1-transport-test-marker.txt"
# Atomic same-origin switch: replay a COMPLETE v1 repository, including its
# authentic signed summary/index and old objects. No signatures are forged.
ln -s "$RUNNER_TEMP/u1-v1/repo" "$root/older"
mv -Tf "$root/older" "$root/current"
cmp -s <(curl -fsS http://127.0.0.1:18393/summary) "$RUNNER_TEMP/u1-v1/repo/summary"
before_requests=$(grep -c '"GET /' "$root/http.log" || true)
if flatpak --user update -y --noninteractive --no-deps "$app" >"$root/older-update.log" 2>&1; then
  outcome=success
else
  outcome=error
fi
after_requests=$(grep -c '"GET /' "$root/http.log" || true)
observed=$(flatpak --user info --show-commit "$app")
if [[ "$observed" != "$old" && "$observed" != "$new" ]]; then
  echo "U1 replay diagnostic: unexpected installed commit $observed" >&2
  cat "$root/older-update.log" >&2
  exit 1
fi
if [[ "$observed" == "$old" ]]; then
  echo 'U1 replay diagnostic: OLDER VALID SIGNED REVISION INSTALLED (rollback observed)'
else
  echo 'U1 replay diagnostic: installed revision stayed at v2; NOT proof of anti-replay protection'
fi
echo "U1 replay diagnostic: Flatpak update exit=$outcome, HTTP GET count $before_requests -> $after_requests"
# A nonzero update is not an anti-rollback verdict: expose the bounded native
# Flatpak diagnostic instead of silently discarding it in a runner-local file.
# This is a synthetic Devel-only remote with no owner credentials or data.
if [[ "$outcome" == error ]]; then
  echo 'U1 replay diagnostic: older signed update output (first 60 lines):'
  sed -n '1,60p' "$root/older-update.log"
fi
if (( after_requests > before_requests )); then
  echo 'U1 replay diagnostic: replay-window HTTP requests (up to 40):'
  grep '"GET /' "$root/http.log" | tail -n "$((after_requests-before_requests))" | sed -n '1,40p'
fi
if (( after_requests <= before_requests )); then
  echo 'U1 replay diagnostic: INCONCLUSIVE metadata freshness (no observed replay HTTP GET)'
fi
# Recover on the same origin; never leave the disposable client downgraded.
ln -s "$RUNNER_TEMP/u1-v2/repo" "$root/newer"
mv -Tf "$root/newer" "$root/current"
cmp -s <(curl -fsS http://127.0.0.1:18393/summary) "$RUNNER_TEMP/u1-v2/repo/summary"
flatpak --user update -y --noninteractive --no-deps "$app"
test "$(flatpak --user info --show-commit "$app")" = "$new"
installed=$(flatpak --user info --show-location "$app")
grep -Fx 'u1-nonproduction-content-revision-2' \
  "$installed/files/share/broadcaster/u1-transport-test-marker.txt"
echo "U1 replay diagnostic: disposable client restored to signed v2 on $arch"
