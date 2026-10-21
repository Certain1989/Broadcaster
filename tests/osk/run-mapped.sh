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
failed_cases=""
for case in \
  osk_mapped_gtk_close_releases_dialog_and_draft \
  osk_mapped_gtk_done_and_back_release_dialog_and_draft \
  osk_mapped_gtk_parent_close_during_draft \
  osk_mapped_gtk_cancel_invalidated_origin \
  osk_mapped_gtk_dropped_origin_cancel_nonretention \
  osk_mapped_gtk_input_transaction_and_cancel \
  osk_mapped_gtk_native_unicode_backspace \
  osk_mapped_gtk_pending_selection_native_backspace \
  osk_mapped_gtk_native_draft_focus_clears_collapsed_selection \
  osk_mapped_gtk_draft_selection_survives_key_focus_transfer \
  osk_mapped_gtk_caret_visible_with_remote_key_focus \
  osk_mapped_gtk_caret_buttons_collapse_selection \
  osk_mapped_gtk_compact_add_playlist_url_launcher \
  osk_mapped_gtk_mpv_row_native_remote_route_and_lifecycle \
  osk_mapped_gtk_channel_move_menu_native_remote \
  osk_mapped_gtk_first_row_move_popup_native_focus_containment \
  osk_mapped_gtk_channel_menus_all_rows_and_sort_focus_containment \
  osk_mapped_gtk_channel_sort_menu_native_remote \
  gtk_mediathek_toolbar_narrow_allocations \
  osk_mapped_gtk_entry_reentrant_signal_trace \
  osk_mapped_gtk_search_reentrant_signal_trace \
  osk_mapped_gtk_entry_row_reentrant_signal_trace \
  osk_mapped_gtk_multiple_changed_signal_observers \
  osk_mapped_gtk_native_url_correction_clears_error \
  osk_mapped_gtk_lifecycle_guards \
  osk_mapped_gtk_preferences_expander_and_popover \
  osk_mapped_gtk_entry_row_immediate_save_once \
  osk_mapped_gtk_settings_toggle_and_search \
  osk_mapped_gtk_removed_entry_row_no_ghost_save \
  osk_mapped_gtk_all_keys_fit_without_scrolling
do
  log_file="$schema_dir/$case.log"
  if xvfb-run -a env G_DEBUG=fatal-criticals GDK_BACKEND=x11 GSETTINGS_BACKEND=memory \
    GSETTINGS_SCHEMA_DIR="$schema_dir" \
    cargo test --locked "$case" -- --ignored --test-threads=1 >"$log_file" 2>&1; then
    printf 'K1 mapped GTK PASS: %s\n' "$case"
    if [ "$case" = osk_mapped_gtk_channel_menus_all_rows_and_sort_focus_containment ]; then
      grep 'F02 .*GtkModelButton' "$log_file" || :
    fi
  else
    failed=1
    failed_cases="$failed_cases $case"
    printf 'K1 mapped GTK FAIL: %s\n' "$case"
  fi
done
if [ "$failed" -ne 0 ]; then
  # Reproduce the assertions at the very end of Meson's truncated log.
  for case in $failed_cases; do
    printf 'K1 mapped GTK details for %s:\n' "$case" >&2
    tail -n 45 "$schema_dir/$case.log" >&2
  done
  # Meson displays only the last 100 log lines; keep the precise names of
  # every failing isolated fixture visible even when early output is cut off.
  printf 'K1 mapped GTK failed cases:%s\n' "$failed_cases" >&2
else
  echo "K1 mapped GTK: every named fixture passed"
fi
exit "$failed"
