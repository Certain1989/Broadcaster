#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
# U1 nonproduction-only HTTP signature negatives; invoked by native CI.
set -euo pipefail
unsigned_summary_server_pid=
invalid_summary_server_pid=
cleanup() {
  for pid in "$unsigned_summary_server_pid" "$invalid_summary_server_pid"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  done
}
trap cleanup EXIT

# Negative: a complete repository with signed commits and summary
# but no summary.sig must not be accepted over HTTP by a NEW .flatpakrepo
# client. Use a distinct XDG_DATA_HOME to avoid stale summary and
# appstream metadata after a remote URL switch. A remote-ls on the
# already-enumerated positive client is not a valid negative oracle.
unsigned_summary_repo="$RUNNER_TEMP/u1-unsigned-summary/repo"
mkdir -p "$(dirname "$unsigned_summary_repo")"
cp -a "$RUNNER_TEMP/u1-v2/repo" "$unsigned_summary_repo"
test -s "$unsigned_summary_repo/summary" && test -s "$unsigned_summary_repo/summary.sig"
# Remove BOTH the legacy and indexed summary signatures. A modern
# client can verify the index and install despite absent summary.sig.
if [ -s "$unsigned_summary_repo/summary.idx.sig" ]; then
  echo 'U1 negative: signed summary.idx.sig present; removing it too'
else
  echo 'U1 negative: summary.idx.sig absent; legacy-only signature path'
fi
rm -f "$unsigned_summary_repo/summary.sig" "$unsigned_summary_repo/summary.idx.sig"
test ! -e "$unsigned_summary_repo/summary.sig"
test ! -e "$unsigned_summary_repo/summary.idx.sig"
# Exercise the intended HTTP transport, not file://, to distinguish
# local-repository semantics from remote summary verification.
python3 -u build-aux/u1/http-meter.py 18391 \
  "$unsigned_summary_repo" >"$RUNNER_TEMP/u1-unsigned-summary-http.log" 2>&1 &
unsigned_summary_server_pid=$!
for try in 1 2 3 4 5 6 7 8 9 10; do
  if curl -fsS http://127.0.0.1:18391/summary >/dev/null 2>&1; then break; fi
  sleep 0.3
done
curl -fsS http://127.0.0.1:18391/summary >/dev/null
if curl -fsS http://127.0.0.1:18391/summary.sig >/dev/null 2>&1 || curl -fsS http://127.0.0.1:18391/summary.idx.sig >/dev/null 2>&1; then
  echo 'U1 negative HTTP fixture unexpectedly serves a summary signature' >&2
  exit 1
fi
sed -e 's#^Name=.*#Name=broadcaster-u1-summary-positive#' \
    "$RUNNER_TEMP/u1-v2/u1-test.flatpakrepo" > "$RUNNER_TEMP/summary-positive.flatpakrepo"
summary_positive_client="$RUNNER_TEMP/u1-summary-positive-client"
XDG_DATA_HOME="$summary_positive_client" flatpak --user remote-add --from \
  broadcaster-u1-summary-positive "$RUNNER_TEMP/summary-positive.flatpakrepo"
XDG_DATA_HOME="$summary_positive_client" flatpak --user remote-ls \
  broadcaster-u1-summary-positive | grep -F 'io.github.Certain1989.Broadcaster.Devel'
summary_positive_repo="$summary_positive_client/flatpak/repo"
test -f "$summary_positive_repo/config"
summary_positive_option='remote "broadcaster-u1-summary-positive".gpg-verify-summary'
summary_default=$(ostree --repo="$summary_positive_repo" config get "$summary_positive_option" 2>/dev/null || true)
echo "U1 normal .flatpakrepo summary verification policy: ${summary_default:-unset}"
if [ "$summary_default" != true ]; then
  echo 'U1 normal .flatpakrepo onboarding did not enable summary verification' >&2
  exit 1
fi
# Negative: corrupt *all* advertised summary signature entry points.
# Modern Flatpak may verify summary.idx.sig and never consult the
# legacy summary.sig, so changing only the latter is not a negative.
invalid_summary_repo="$RUNNER_TEMP/u1-invalid-summary/repo"
mkdir -p "$(dirname "$invalid_summary_repo")"
cp -a "$RUNNER_TEMP/u1-v2/repo" "$invalid_summary_repo"
test -s "$invalid_summary_repo/summary.sig"
printf 'invalid-U1-summary-signature\n' > "$invalid_summary_repo/summary.sig"
if [ -f "$invalid_summary_repo/summary.idx.sig" ]; then
  printf 'invalid-U1-index-signature\n' > "$invalid_summary_repo/summary.idx.sig"
  echo 'U1 negative: corrupted both legacy and index summary signatures'
else
  echo 'U1 negative: no modern summary.idx.sig; corrupted legacy signature'
fi
python3 -u build-aux/u1/http-meter.py 18392 "$invalid_summary_repo" >"$RUNNER_TEMP/u1-invalid-summary-http.log" 2>&1 &
invalid_summary_server_pid=$!
for try in 1 2 3 4 5 6 7 8 9 10; do
  if curl -fsS http://127.0.0.1:18392/summary >/dev/null 2>&1; then break; fi
  sleep 0.3
done
curl -fsS http://127.0.0.1:18392/summary.sig >/dev/null
sed -e 's#^Name=.*#Name=broadcaster-u1-invalid-summary#' -e "s#^Url=.*#Url=http://127.0.0.1:18392#" "$RUNNER_TEMP/u1-v2/u1-test.flatpakrepo" > "$RUNNER_TEMP/invalid-summary.flatpakrepo"
invalid_summary_client="$RUNNER_TEMP/u1-invalid-summary-client"
invalid_summary_log="$RUNNER_TEMP/u1-invalid-summary.log"
if XDG_DATA_HOME="$invalid_summary_client" flatpak --user remote-add --from broadcaster-u1-invalid-summary "$RUNNER_TEMP/invalid-summary.flatpakrepo" >"$invalid_summary_log" 2>&1; then
  if XDG_DATA_HOME="$invalid_summary_client" flatpak --user install -y --noninteractive --no-deps --no-related broadcaster-u1-invalid-summary io.github.Certain1989.Broadcaster.Devel >>"$invalid_summary_log" 2>&1; then
    echo 'U1 SECURITY FAILURE: corrupt summary signatures allowed application installation' >&2
    cat "$invalid_summary_log" >&2
    exit 1
  fi
fi
if ! grep -Eiq 'gpg|signature|verification|not signed|summary\.sig|summary\.idx\.sig' "$invalid_summary_log"; then
  echo 'U1 corrupt-summary rejection lacked a signature-specific error' >&2
  cat "$invalid_summary_log" >&2
  exit 1
fi
if XDG_DATA_HOME="$invalid_summary_client" flatpak --user info io.github.Certain1989.Broadcaster.Devel >/dev/null 2>&1; then
  echo 'U1 corrupt-summary negative left an app installed' >&2
  exit 1
fi
echo 'U1 fresh HTTP client rejected corrupt summary signatures'
# Same pinned public key, distinct remote name, unsigned-summary URL,
# and a genuinely fresh user installation. A signature-specific
# rejection may happen at remote-add or at first remote-ls.
sed -e 's#^Name=.*#Name=broadcaster-u1-unsigned-summary#' \
    -e "s#^Url=.*#Url=http://127.0.0.1:18391#" \
    "$RUNNER_TEMP/u1-v2/u1-test.flatpakrepo" > "$RUNNER_TEMP/unsigned-summary.flatpakrepo"
summary_negative_client="$RUNNER_TEMP/u1-unsigned-summary-negative-client"
summary_log="$RUNNER_TEMP/u1-unsigned-summary.log"
if XDG_DATA_HOME="$summary_negative_client" flatpak --user remote-add --from \
     broadcaster-u1-unsigned-summary "$RUNNER_TEMP/unsigned-summary.flatpakrepo" >"$summary_log" 2>&1; then
  summary_negative_repo="$summary_negative_client/flatpak/repo"
  summary_negative_option='remote "broadcaster-u1-unsigned-summary".gpg-verify-summary'
  if [ "$(ostree --repo="$summary_negative_repo" config get "$summary_negative_option" 2>/dev/null || true)" != true ]; then
    echo 'U1 unsigned-summary remote onboarding lacked summary verification' >&2
    exit 1
  fi
  # Enumeration alone is not an installation trust oracle: some
  # Flatpak/OSTree versions can list refs without fetching a commit.
  # Require rejection at the actual installation boundary.
  XDG_DATA_HOME="$summary_negative_client" flatpak --user remote-ls \
    broadcaster-u1-unsigned-summary >>"$summary_log" 2>&1 || true
  if XDG_DATA_HOME="$summary_negative_client" flatpak --user install \
       -y --noninteractive --no-deps --no-related \
       broadcaster-u1-unsigned-summary io.github.Certain1989.Broadcaster.Devel \
       >>"$summary_log" 2>&1; then
    echo 'U1 SECURITY FAILURE: missing-summary-signature repository installed application' >&2
    cat "$summary_log" >&2
    exit 1
  fi
fi
if ! grep -Eiq 'gpg|signature|verification|not signed|summary\.sig' "$summary_log"; then
  echo 'U1 unsigned-summary rejection lacked a signature-specific error' >&2
  cat "$summary_log" >&2
  exit 1
fi
if XDG_DATA_HOME="$summary_negative_client" flatpak --user info \
     io.github.Certain1989.Broadcaster.Devel >/dev/null 2>&1; then
  echo 'U1 unsigned-summary negative left an app installed' >&2
  exit 1
fi
echo 'U1 fresh normal .flatpakrepo client rejected missing summary signature'
