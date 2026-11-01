#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
set -euo pipefail
if (( $# != 1 )); then
  echo "Usage: $0 NEW-PRIVATE-GNUPG-DIRECTORY" >&2
  exit 2
fi
home=$1
if [[ -e "$home" || -L "$home" ]]; then
  echo "Refusing to use an existing key directory" >&2
  exit 2
fi
umask 077
mkdir -m 700 -- "$home"
gpg --homedir "$home" --batch --pinentry-mode loopback --passphrase '' \
  --quick-generate-key 'Broadcaster U1 disposable test only <u1-test@example.invalid>' ed25519 sign 1d
fingerprint=$(gpg --homedir "$home" --batch --with-colons --list-secret-keys |
  awk -F: '$1 == "fpr" { print $10; exit }')
if [[ ! "$fingerprint" =~ ^[A-F0-9]{40}$ ]]; then
  echo "Failed to obtain a full signing fingerprint" >&2
  exit 1
fi
printf '%s\n' "$fingerprint" > "$home/.u1-disposable-fingerprint"
echo "Disposable U1 key created (private directory; never commit or upload)."