#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
# Fast smoke is deliberately a subset, not an alternative to full native-focus CI.
set -euo pipefail

schema_dir=$(mktemp -d)
negative_log=$(mktemp)
trap 'rm -rf "$schema_dir"; rm -f "$negative_log"' EXIT
cp data/io.github.Certain1989.Broadcaster.gschema.xml "$schema_dir/"
# Some nested GTK focus journeys can open the native file-chooser machinery.
cp /usr/share/glib-2.0/schemas/org.gtk.gtk4.Settings.FileChooser.gschema.xml "$schema_dir/"
glib-compile-schemas "$schema_dir"

run_positive() {
  local case_name=$1 geometry=$2 data_dir
  # GUIDE_MAPPING's real-SQLite safety guard requires its established fixture prefix.
  # Preserve that guard; never relax production or test storage protections.
  case "$case_name" in
    native_channel_*) data_dir=$(mktemp -d -t broadcaster-guide-mapping-fixture-XXXXXXXX) ;;
    *) data_dir=$(mktemp -d -t broadcaster-fast-ci-XXXXXXXX) ;;
  esac
  if ! xvfb-run -a -s "-screen 0 ${geometry}x24" \
    env GDK_BACKEND=x11 GSETTINGS_BACKEND=memory GSETTINGS_SCHEMA_DIR="$schema_dir" \
      XDG_DATA_HOME="$data_dir" \
      bash tests/ci-require-one-ignored.sh "$case_name"; then
    rm -rf "$data_dir"
    echo "Fast CI native case failed: $case_name" >&2
    return 1
  fi
  rm -rf "$data_dir"
}

# Positive: real source and guide management, persisted IPTV favourites,
# and *painted* focus, spanning the compact and TV viewport sizes.
# Every name must match exactly one ignored test, not Cargo's zero-match pass.
run_positive native_source_manager_1_640_480 640x480
# G1: new playlist-level Programme Guide action is D-pad reachable at both sizes.
run_positive source_manager_actual_remote_back_trace 640x480
run_positive source_manager_actual_remote_back_trace_tv 1280x800
run_positive native_epg_manager_3_1280_800 1280x800
run_positive native_channel_favourites_640_480 640x480
run_positive native_painted_focus_1280_800 1280x800

# Negative: prove the real GTK oracle still rejects a deliberately wrong
# source-manager focus assertion for its exact marker, not for an X11/setup
# failure. Full CI retains all other negative controls unchanged.
data_dir=$(mktemp -d -t broadcaster-fast-ci-negative-XXXXXXXX)
if xvfb-run -a -s '-screen 0 640x480x24' \
  env GDK_BACKEND=x11 GSETTINGS_BACKEND=memory GSETTINGS_SCHEMA_DIR="$schema_dir" \
    XDG_DATA_HOME="$data_dir" \
    cargo test --locked source_manager_native_negative_oracle -- --ignored --test-threads=1 \
    >"$negative_log" 2>&1; then
  cat "$negative_log"
  rm -rf "$data_dir"
  echo 'Fast CI negative focus oracle unexpectedly passed' >&2
  exit 1
fi
if ! grep -Fq 'SOURCE_MANAGER_EXPECTED_NEGATIVE_ORACLE' "$negative_log"; then
  cat "$negative_log"
  rm -rf "$data_dir"
  echo 'Fast CI negative oracle failed for an unrelated reason' >&2
  exit 1
fi
rm -rf "$data_dir"
echo 'Fast CI: real GTK/SQLite/XTest positives and marker-controlled negative passed'
