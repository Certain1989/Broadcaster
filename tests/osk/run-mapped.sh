#!/bin/sh
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
cd "$(dirname "$0")/../.."
command -v xvfb-run >/dev/null
command -v glib-compile-schemas >/dev/null
schema_dir=$(mktemp -d)
trap 'rm -rf "$schema_dir"' EXIT HUP INT TERM
cp data/io.github.Certain1989.Broadcaster.gschema.xml "$schema_dir/"
glib-compile-schemas "$schema_dir"
xvfb-run -a env GDK_BACKEND=x11 GSETTINGS_BACKEND=memory \
  GSETTINGS_SCHEMA_DIR="$schema_dir" \
  cargo test --locked osk_mapped_gtk_ -- --ignored --test-threads=1
