#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
# Strictly local and NONPRODUCTION; this tool never uploads or switches a live remote.
set -euo pipefail
if (( $# != 5 )); then
  echo "Usage: $0 NEW-OUTPUT-DIR PREVIOUS-OUTPUT-DIR-OR-- X86_64-BUNDLE AARCH64-BUNDLE DISPOSABLE-GNUPG-DIR" >&2
  exit 2
fi
output=$1 previous=$2 x86=$3 arm=$4 key_home=$5
for command in flatpak ostree gpg sha256sum base64 realpath du find; do
  command -v "$command" >/dev/null || { echo "Missing dependency: $command" >&2; exit 2; }
done
[[ ! -e "$output" && ! -L "$output" ]] || { echo "Output exists; refusing overwrite" >&2; exit 2; }
[[ -f "$x86" && -s "$x86" && -f "$arm" && -s "$arm" ]] ||
  { echo "Two nonempty bundles required" >&2; exit 2; }
[[ "$x86" == *x86_64.flatpak && "$arm" == *aarch64.flatpak ]] ||
  { echo "Expected architecture-specific bundle names" >&2; exit 2; }
[[ -f "$key_home/.u1-disposable-fingerprint" && ! -L "$key_home" ]] ||
  { echo "Only a newly created U1 disposable key may sign this proof" >&2; exit 2; }
fingerprint=$(cat "$key_home/.u1-disposable-fingerprint")
[[ "$fingerprint" =~ ^[A-F0-9]{40}$ ]] ||
  { echo "Invalid disposable key fingerprint" >&2; exit 2; }
gpg --homedir "$key_home" --batch --list-secret-keys "$fingerprint" >/dev/null
output=$(realpath -m -- "$output")
[[ "$output" != *[$'\n\r\t\ ']* ]] ||
  { echo "Use an output path without whitespace" >&2; exit 2; }
[[ -d "$(dirname -- "$output")" ]] ||
  { echo "Output parent directory must exist" >&2; exit 2; }
stage=$(mktemp -d "$output.partial.XXXXXXXX")
chmod 700 "$stage"
cleanup() {
  if [[ -d "$stage" ]]; then rm -rf -- "$stage"; fi
}
trap cleanup EXIT
repo="$stage/repo"
gpg --homedir "$key_home" --batch --export "$fingerprint" > "$stage/u1-public-key.gpg"
test -s "$stage/u1-public-key.gpg"
if [[ "$previous" == "-" ]]; then
  ostree --repo="$repo" init --mode=archive-z2
else
  [[ -d "$previous/repo" && -s "$previous/u1-public-key.gpg" && ! -L "$previous" ]] ||
    { echo "Prior output must contain an existing test repository and public key" >&2; exit 2; }
  cmp -s "$stage/u1-public-key.gpg" "$previous/u1-public-key.gpg" ||
    { echo "Key change requires an explicit test migration; refusing" >&2; exit 2; }
  cp -a -- "$previous/repo" "$repo"
fi
# Both imports must succeed before publishing *any* new signed summary.
flatpak build-import-bundle --no-update-summary --gpg-sign="$fingerprint" \
  --gpg-homedir="$key_home" "$repo" "$x86"
flatpak build-import-bundle --no-update-summary --gpg-sign="$fingerprint" \
  --gpg-homedir="$key_home" "$repo" "$arm"
refs=$(ostree --repo="$repo" refs)
for arch in x86_64 aarch64; do
  grep -Fx "app/io.github.Certain1989.Broadcaster.Devel/$arch/master" <<< "$refs" >/dev/null ||
    { echo "Wrong or missing Devel architecture: $arch" >&2; exit 1; }
done
flatpak build-update-repo --gpg-sign="$fingerprint" --gpg-homedir="$key_home" \
  --title='Broadcaster U1 isolated nonproduction proof' "$repo"
[[ -s "$repo/summary" && -s "$repo/summary.sig" ]] ||
  { echo "Signed OSTree summary absent" >&2; exit 1; }
# The public key is intentionally embedded. Private keys remain OUTSIDE output.
pub_key=$(base64 -w0 < "$stage/u1-public-key.gpg")
uri="file://$output/repo"
cat > "$stage/u1-test.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Broadcaster U1 NONPRODUCTION
Name=broadcaster-u1-test
Url=$uri
GPGKey=$pub_key
IsRuntime=false
NoEnumerate=true
EOF
cat > "$stage/u1-test.flatpakref" <<EOF
[Flatpak Ref]
Title=Broadcaster U1 NONPRODUCTION
Name=io.github.Certain1989.Broadcaster.Devel
Branch=master
Url=$uri
GPGKey=$pub_key
IsRuntime=false
SuggestRemoteName=broadcaster-u1-test
EOF
sha256sum "$x86" "$arm" > "$stage/BUNDLE-SHA256SUMS"
{
  printf 'NONPRODUCTION TEST OUTPUT; NOT FOR PUBLICATION\n'
  printf 'GitHub Pages, stable signing and production upload are NOT authorized.\n'
  printf 'Both architecture refs were checked before signing one coherent summary.\n'
  printf 'Repository size bytes: %s\n' "$(du -sb "$repo" | cut -f1)"
  printf 'Repository object/file count: %s\n' "$(find "$repo" -type f | wc -l)"
  printf 'x86_64 bundle bytes: %s\n' "$(wc -c < "$x86")"
  printf 'aarch64 bundle bytes: %s\n' "$(wc -c < "$arm")"
} > "$stage/METRICS.txt"
mv -- "$stage" "$output"
trap - EXIT
echo "NONPRODUCTION signed dual-arch proof staged: $output"