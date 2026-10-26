#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
# Run GTK focus tests in distinct processes: gtk::init cannot be concurrently
# invoked on arbitrary Rust test threads. Fail on the first real RED regression.
set -euo pipefail
cd "$(dirname "$0")/../.."
command -v xvfb-run >/dev/null || { echo 'xvfb-run is required' >&2; exit 2; }
command -v glib-compile-schemas >/dev/null || { echo 'glib-compile-schemas is required' >&2; exit 2; }
schema_dir=$(mktemp -d)
trap 'rm -rf "$schema_dir"' EXIT
cp data/io.github.Certain1989.Broadcaster.gschema.xml "$schema_dir/"
glib-compile-schemas "$schema_dir"
mkdir -p "$schema_dir/data" "$schema_dir/config" "$schema_dir/cache"

# No combined --ignored run: even unrelated GTK tests in one process interfere.
tests=(
  gtk_focus001_negative_control_detects_stolen_focus
  gtk_focus001_mediathek_down_up_roundtrip
  gtk_focus001_mediathek_horizontal_filter_roundtrips
  gtk_focus001_live_first_row_up_to_search
  gtk_focus001_streams_row_left_stays_in_page
)
for test_name in "${tests[@]}"; do
  printf '\nFOCUS001 RUN %s\n' "$test_name"
  timeout 120s xvfb-run -a env \
    GDK_BACKEND=x11 GSETTINGS_BACKEND=memory GSETTINGS_SCHEMA_DIR="$schema_dir" \
    XDG_DATA_HOME="$schema_dir/data" XDG_CONFIG_HOME="$schema_dir/config" XDG_CACHE_HOME="$schema_dir/cache" \
    cargo test --locked "$test_name" -- --ignored --test-threads=1 --nocapture
  printf 'FOCUS001 PASS %s\n' "$test_name"
done
