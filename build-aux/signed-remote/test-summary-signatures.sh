#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
# Signed remote nonproduction-only HTTP signature negatives; invoked by native CI.
set -euo pipefail
unsigned_summary_server_pid=
invalid_summary_server_pid=
cleanup() {
  for pid in "$unsigned_summary_server_pid" "$invalid_summary_server_pid"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  done
}
trap cleanup EXIT

# XDG_DATA_HOME alone does not isolate XDG cache/config. Keep all three
# independent for each disposable client, preventing cross-fixture cache reuse.
client_flatpak() {
  local root=$1
  shift
  XDG_DATA_HOME="$root" XDG_CACHE_HOME="$root/cache" XDG_CONFIG_HOME="$root/config" \
    flatpak --user "$@"
}
print_summary_requests() {
  local label=$1 log=$2
  echo "Signed remote diagnostic: HTTP metadata requests for $label:"
  grep -E '"(GET|HEAD) /(summary|summaries/)' "$log" || true
}
print_remote_trust() {
  local root=$1 remote=$2
  local repo="$root/flatpak/repo" option
  if [ ! -f "$repo/config" ]; then
    echo "Signed remote diagnostic: no OSTree config for $remote"
    return
  fi
  for option in gpg-verify gpg-verify-summary collection-id url; do
    echo "Signed remote diagnostic: $remote $option=$(ostree --repo="$repo" config get "remote \"$remote\".$option" 2>/dev/null || echo unset)"
  done
}

# Negative: a complete repository with signed commits and summary,
# but neither legacy nor indexed summary signature, must not be accepted
# over HTTP by a fresh .flatpakrepo client. Use a distinct XDG_DATA_HOME to avoid stale summary and
# appstream metadata after a remote URL switch. A remote-ls on the
# already-enumerated positive client is not a valid negative oracle.
unsigned_summary_repo="$RUNNER_TEMP/signed-remote-unsigned-summary/repo"
mkdir -p "$(dirname "$unsigned_summary_repo")"
cp -a "$RUNNER_TEMP/signed-remote-v2/repo" "$unsigned_summary_repo"
test -s "$unsigned_summary_repo/summary" && test -s "$unsigned_summary_repo/summary.sig"
# Indexed OSTree summaries are signed under summaries/<digest>.idx.sig,
# not only at the repository root. Earlier negatives mutated root files
# while the client fetched and trusted the untouched nested signature.
test -d "$unsigned_summary_repo/summaries"
indexed_unsigned=$(find "$unsigned_summary_repo/summaries" -type f -name '*.idx.sig' -print -quit)
test -n "$indexed_unsigned"
echo 'Signed remote negative: removing legacy and nested indexed summary signatures'
rm -f "$unsigned_summary_repo/summary.sig" "$unsigned_summary_repo/summary.idx.sig"
find "$unsigned_summary_repo/summaries" -type f -name '*.idx.sig' -delete
test ! -e "$unsigned_summary_repo/summary.sig"
test ! -e "$unsigned_summary_repo/summary.idx.sig"
test -z "$(find "$unsigned_summary_repo/summaries" -type f -name '*.idx.sig' -print -quit)"
# Exercise the intended HTTP transport, not file://, to distinguish
# local-repository semantics from remote summary verification.
python3 -u build-aux/signed-remote/http-meter.py 18391 \
  "$unsigned_summary_repo" >"$RUNNER_TEMP/signed-remote-unsigned-summary-http.log" 2>&1 &
unsigned_summary_server_pid=$!
for try in 1 2 3 4 5 6 7 8 9 10; do
  if curl -fsS http://127.0.0.1:18391/summary >/dev/null 2>&1; then break; fi
  sleep 0.3
done
curl -fsS http://127.0.0.1:18391/summary >/dev/null
if curl -fsS http://127.0.0.1:18391/summary.sig >/dev/null 2>&1 || curl -fsS http://127.0.0.1:18391/summary.idx.sig >/dev/null 2>&1; then
  echo 'Signed remote negative HTTP fixture unexpectedly serves a root summary signature' >&2
  exit 1
fi
# Check the exact nested path originally advertised by the signed index.
unsigned_index_path=${indexed_unsigned#"$unsigned_summary_repo"/}
if curl -fsS "http://127.0.0.1:18391/$unsigned_index_path" >/dev/null 2>&1; then
  echo 'Signed remote negative HTTP fixture unexpectedly serves indexed signature' >&2
  exit 1
fi
sed -e 's#^Name=.*#Name=broadcaster-signed-remote-summary-positive#' \
    "$RUNNER_TEMP/signed-remote-v2/signed-remote-test.flatpakrepo" > "$RUNNER_TEMP/summary-positive.flatpakrepo"
summary_positive_client="$RUNNER_TEMP/signed-remote-summary-positive-client"
client_flatpak "$summary_positive_client" remote-add --from \
  broadcaster-signed-remote-summary-positive "$RUNNER_TEMP/summary-positive.flatpakrepo"
client_flatpak "$summary_positive_client" remote-ls \
  broadcaster-signed-remote-summary-positive | grep -F 'io.github.Certain1989.Broadcaster.Devel'
summary_positive_repo="$summary_positive_client/flatpak/repo"
test -f "$summary_positive_repo/config"
summary_positive_option='remote "broadcaster-signed-remote-summary-positive".gpg-verify-summary'
summary_default=$(ostree --repo="$summary_positive_repo" config get "$summary_positive_option" 2>/dev/null || true)
echo "Signed remote normal .flatpakrepo summary verification policy: ${summary_default:-unset}"
if [ "$summary_default" != true ]; then
  echo 'Signed remote normal .flatpakrepo onboarding did not enable summary verification' >&2
  exit 1
fi
# Negative: corrupt *all* advertised summary signature entry points.
# Flatpak fetches summaries/<digest>.idx.sig referenced by summary.idx.
# Corrupting only root summary.sig or summary.idx.sig is insufficient.
invalid_summary_repo="$RUNNER_TEMP/signed-remote-invalid-summary/repo"
mkdir -p "$(dirname "$invalid_summary_repo")"
cp -a "$RUNNER_TEMP/signed-remote-v2/repo" "$invalid_summary_repo"
test -s "$invalid_summary_repo/summary.sig"
printf 'invalid-Signed remote-summary-signature\n' > "$invalid_summary_repo/summary.sig"
if [ -f "$invalid_summary_repo/summary.idx.sig" ]; then
  printf 'invalid-Signed remote-index-signature\n' > "$invalid_summary_repo/summary.idx.sig"
fi
test -d "$invalid_summary_repo/summaries"
indexed_invalid=$(find "$invalid_summary_repo/summaries" -type f -name '*.idx.sig' -print -quit)
test -n "$indexed_invalid"
while IFS= read -r -d '' indexed_sig; do
  printf 'invalid-Signed remote-nested-index-signature\n' > "$indexed_sig"
done < <(find "$invalid_summary_repo/summaries" -type f -name '*.idx.sig' -print0)
echo 'Signed remote negative: corrupted legacy and nested indexed summary signatures'
python3 -u build-aux/signed-remote/http-meter.py 18392 "$invalid_summary_repo" >"$RUNNER_TEMP/signed-remote-invalid-summary-http.log" 2>&1 &
invalid_summary_server_pid=$!
for try in 1 2 3 4 5 6 7 8 9 10; do
  if curl -fsS http://127.0.0.1:18392/summary >/dev/null 2>&1; then break; fi
  sleep 0.3
done
curl -fsS http://127.0.0.1:18392/summary.sig >/dev/null
# Assert that the client-visible HTTP bytes really are the corrupted fixture,
# rather than a stale server or the original signed repository.
cmp -s <(curl -fsS http://127.0.0.1:18392/summary.sig) "$invalid_summary_repo/summary.sig"
if [ -f "$invalid_summary_repo/summary.idx.sig" ]; then
  cmp -s <(curl -fsS http://127.0.0.1:18392/summary.idx.sig) "$invalid_summary_repo/summary.idx.sig"
fi
# Verify HTTP really serves the corrupt signature at the nested URL used
# by Flatpak, not merely a corrupt unused root file.
while IFS= read -r -d '' indexed_sig; do
  indexed_path=${indexed_sig#"$invalid_summary_repo"/}
  cmp -s <(curl -fsS "http://127.0.0.1:18392/$indexed_path") "$indexed_sig"
done < <(find "$invalid_summary_repo/summaries" -type f -name '*.idx.sig' -print0)
sed -e 's#^Name=.*#Name=broadcaster-signed-remote-invalid-summary#' -e "s#^Url=.*#Url=http://127.0.0.1:18392#" "$RUNNER_TEMP/signed-remote-v2/signed-remote-test.flatpakrepo" > "$RUNNER_TEMP/invalid-summary.flatpakrepo"
invalid_summary_client="$RUNNER_TEMP/signed-remote-invalid-summary-client"
invalid_summary_log="$RUNNER_TEMP/signed-remote-invalid-summary.log"
if client_flatpak "$invalid_summary_client" remote-add --from broadcaster-signed-remote-invalid-summary "$RUNNER_TEMP/invalid-summary.flatpakrepo" >"$invalid_summary_log" 2>&1; then
  print_remote_trust "$invalid_summary_client" broadcaster-signed-remote-invalid-summary
  if client_flatpak "$invalid_summary_client" --verbose install -y --noninteractive --no-deps --no-related broadcaster-signed-remote-invalid-summary io.github.Certain1989.Broadcaster.Devel >>"$invalid_summary_log" 2>&1; then
    echo 'Signed remote SECURITY FAILURE: corrupt summary signatures allowed application installation' >&2
    cat "$invalid_summary_log" >&2
    print_summary_requests corrupt "$RUNNER_TEMP/signed-remote-invalid-summary-http.log"
    exit 1
  fi
fi
print_summary_requests corrupt "$RUNNER_TEMP/signed-remote-invalid-summary-http.log"
if ! grep -Eiq 'gpg|signature|verification|not signed|summary\.sig|summary\.idx\.sig' "$invalid_summary_log"; then
  echo 'Signed remote corrupt-summary rejection lacked a signature-specific error' >&2
  cat "$invalid_summary_log" >&2
  exit 1
fi
if client_flatpak "$invalid_summary_client" info io.github.Certain1989.Broadcaster.Devel >/dev/null 2>&1; then
  echo 'Signed remote corrupt-summary negative left an app installed' >&2
  exit 1
fi
echo 'Signed remote fresh HTTP client rejected corrupt summary signatures'
# Same pinned public key, distinct remote name, unsigned-summary URL,
# and a genuinely fresh user installation. A signature-specific
# rejection may happen at remote-add or at first remote-ls.
sed -e 's#^Name=.*#Name=broadcaster-signed-remote-unsigned-summary#' \
    -e "s#^Url=.*#Url=http://127.0.0.1:18391#" \
    "$RUNNER_TEMP/signed-remote-v2/signed-remote-test.flatpakrepo" > "$RUNNER_TEMP/unsigned-summary.flatpakrepo"
summary_negative_client="$RUNNER_TEMP/signed-remote-unsigned-summary-negative-client"
summary_log="$RUNNER_TEMP/signed-remote-unsigned-summary.log"
if client_flatpak "$summary_negative_client" remote-add --from \
     broadcaster-signed-remote-unsigned-summary "$RUNNER_TEMP/unsigned-summary.flatpakrepo" >"$summary_log" 2>&1; then
  print_remote_trust "$summary_negative_client" broadcaster-signed-remote-unsigned-summary
  summary_negative_repo="$summary_negative_client/flatpak/repo"
  summary_negative_option='remote "broadcaster-signed-remote-unsigned-summary".gpg-verify-summary'
  if [ "$(ostree --repo="$summary_negative_repo" config get "$summary_negative_option" 2>/dev/null || true)" != true ]; then
    echo 'Signed remote unsigned-summary remote onboarding lacked summary verification' >&2
    exit 1
  fi
  # Enumeration alone is not an installation trust oracle: some
  # Flatpak/OSTree versions can list refs without fetching a commit.
  # Require rejection at the actual installation boundary.
  client_flatpak "$summary_negative_client" remote-ls \
    broadcaster-signed-remote-unsigned-summary >>"$summary_log" 2>&1 || true
  if client_flatpak "$summary_negative_client" install \
       -y --noninteractive --no-deps --no-related \
       broadcaster-signed-remote-unsigned-summary io.github.Certain1989.Broadcaster.Devel \
       >>"$summary_log" 2>&1; then
    echo 'Signed remote SECURITY FAILURE: missing-summary-signature repository installed application' >&2
    cat "$summary_log" >&2
    print_summary_requests missing "$RUNNER_TEMP/signed-remote-unsigned-summary-http.log"
    exit 1
  fi
fi
if ! grep -Eiq 'gpg|signature|verification|not signed|summary\.sig' "$summary_log"; then
  echo 'Signed remote unsigned-summary rejection lacked a signature-specific error' >&2
  cat "$summary_log" >&2
  exit 1
fi
if client_flatpak "$summary_negative_client" info \
     io.github.Certain1989.Broadcaster.Devel >/dev/null 2>&1; then
  echo 'Signed remote unsigned-summary negative left an app installed' >&2
  exit 1
fi
print_summary_requests missing "$RUNNER_TEMP/signed-remote-unsigned-summary-http.log"
echo 'Signed remote fresh normal .flatpakrepo client rejected missing summary signature'
