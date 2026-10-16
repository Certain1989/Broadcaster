#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Use a disposable GSettings keyfile backend so neither host nor TV-user
# preferences are modified. SCHEMA_DIR may point to a staged Flatpak export.
set -euo pipefail
app=io.github.Certain1989.Broadcaster
schema_dir="${SCHEMA_DIR:-data}"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

cp "$schema_dir/$app.gschema.xml" "$scratch/"
glib-compile-schemas --strict "$scratch"
mkdir -p "$scratch/config"

setting() {
  env GSETTINGS_SCHEMA_DIR="$scratch" \
      GSETTINGS_BACKEND=keyfile \
      XDG_CONFIG_HOME="$scratch/config" \
      gsettings "$@"
}

# A profile with no stored value receives the schema default.
test "$(setting get "$app" is-fullscreen)" = true
test "$(setting get "$app" is-maximized)" = false

# Explicitly leaving fullscreen persists across independent gsettings calls.
setting set "$app" is-fullscreen false
test "$(setting get "$app" is-fullscreen)" = false
setting set "$app" is-fullscreen true
test "$(setting get "$app" is-fullscreen)" = true

# Reset removes the override and restores the intended first-launch default.
setting reset "$app" is-fullscreen
test "$(setting get "$app" is-fullscreen)" = true
echo 'First-launch and persisted fullscreen settings: OK'
