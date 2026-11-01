#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
set -euo pipefail
cd "$(dirname "$0")/../.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf 'not-a-bundle' > "$tmp/bad-x86_64.flatpak"
printf 'not-a-bundle' > "$tmp/bad-aarch64.flatpak"
if bash build-aux/u1/stage-signed-remote.sh "$tmp/out" - \
     "$tmp/bad-x86_64.flatpak" "$tmp/bad-aarch64.flatpak" "$tmp/no-key" 2>/dev/null; then
  echo "U1 failed to reject absent disposable key" >&2
  exit 1
fi
mkdir "$tmp/out"
if bash build-aux/u1/stage-signed-remote.sh "$tmp/out" - \
     "$tmp/bad-x86_64.flatpak" "$tmp/bad-aarch64.flatpak" "$tmp/no-key" 2>/dev/null; then
  echo "U1 failed to reject pre-existing output" >&2
  exit 1
fi
if bash build-aux/u1/make-test-key.sh "$tmp/out" 2>/dev/null; then
  echo "U1 key maker reused an existing path" >&2
  exit 1
fi
echo "U1 output/key negative-path guards: PASS"