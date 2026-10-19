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
# One GTK initialization per process. The Rust test runner can dispatch
# sequential --test-threads=1 cases to DIFFERENT OS threads, which violates
# gtk-rs's single-initialization-thread requirement.
failed=0
for case in \
  osk_mapped_gtk_close_releases_dialog_and_draft \
  osk_mapped_gtk_input_transaction_and_cancel \
  osk_mapped_gtk_lifecycle_guards \
  osk_mapped_gtk_preferences_expander_and_popover
do
  if ! xvfb-run -a env GDK_BACKEND=x11 GSETTINGS_BACKEND=memory \
    GSETTINGS_SCHEMA_DIR="$schema_dir" \
    cargo test --locked "$case" -- --ignored --test-threads=1; then
    failed=1
  fi
done
exit "$failed"
