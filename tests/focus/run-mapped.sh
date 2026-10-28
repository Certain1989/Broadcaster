#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
# Run each mapped GTK regression in an isolated process. Never stop at the
# first RED: CI must expose all failures while retaining a blocking exit code.
set -euo pipefail
cd "$(dirname "$0")/../.."

require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'FOCUS001 ENVIRONMENT missing=%s (all required tests unrun)\n' "$1" >&2
    exit 2
  fi
}
for tool in xvfb-run glib-compile-schemas timeout cargo; do require "$tool"; done

scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
schema_dir="$scratch/schema"
mkdir -p "$schema_dir" "$scratch/data" "$scratch/config" "$scratch/cache"
cp data/io.github.Certain1989.Broadcaster.gschema.xml "$schema_dir/"
if ! glib-compile-schemas "$schema_dir" >"$scratch/schema.log" 2>&1; then
  echo 'FOCUS001 ENVIRONMENT disposable GSettings schema compilation failed' >&2
  exit 2
fi


# Compile an ephemeral XTest emitter. Unlike calling the application's
# capture handler directly, this sends a genuine X11 keypress to the active
# mapped window and exercises native GTK event propagation.
xtest_ready=0
if command -v "${FOCUS001_CC:-cc}" >/dev/null 2>&1; then
  cat >"$scratch/xtest-key.c" <<'XTEST_C'
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <stdint.h>
#include <string.h>

int main(int argc, char **argv) {
    Display *display = XOpenDisplay(NULL);
    if (!display) return 3;
    if (argc == 6 && strcmp(argv[1], "--pixels") == 0) {
        // Pixel oracle only for disposable mapped GUI: no screenshots persisted.
        Window focus = None, root = DefaultRootWindow(display), child = None;
        int revert, dx = 0, dy = 0;
        XGetInputFocus(display, &focus, &revert);
        if (focus == None || focus == PointerRoot) return 4;
        if (!XTranslateCoordinates(display, focus, root, 0, 0, &dx, &dy, &child)) return 5;
        int x = atoi(argv[2]) + dx, y = atoi(argv[3]) + dy;
        int w = atoi(argv[4]), h = atoi(argv[5]);
        XWindowAttributes attr;
        if (!XGetWindowAttributes(display, root, &attr)) return 6;
        if (x < 0 || y < 0 || w <= 0 || h <= 0 ||
            x + w > attr.width || y + h > attr.height) return 6;
        XImage *img = XGetImage(display, root, x, y, w, h, AllPlanes, ZPixmap);
        if (!img) return 7;
        uint64_t hash = UINT64_C(14695981039346656037);
        for (int row = 0; row < h; row++) {
            for (int col = 0; col < w; col++) {
                hash ^= XGetPixel(img, col, row);
                hash *= UINT64_C(1099511628211);
            }
        }
        printf("%016llx\n", (unsigned long long)hash);
        XDestroyImage(img);
        XCloseDisplay(display);
        return 0;
    }
    if (argc != 2 && argc != 3) return 2;
    int repeat = argc == 3 ? atoi(argv[2]) : 1;
    if (repeat < 1 || repeat > 32) return 2;
    int event, error, major, minor;
    if (!XTestQueryExtension(display, &event, &error, &major, &minor)) return 4;
    KeySym symbol = XStringToKeysym(argv[1]);
    KeyCode code = XKeysymToKeycode(display, symbol);
    if (symbol == NoSymbol || code == 0) return 5;
    Window focused;
    int revert;
    XGetInputFocus(display, &focused, &revert);
    if (focused == None || focused == PointerRoot || focused == DefaultRootWindow(display)) {
        Window root = DefaultRootWindow(display), parent, *children = NULL;
        unsigned int count = 0;
        if (!XQueryTree(display, root, &root, &parent, &children, &count)) return 6;
        Window target = None;
        for (unsigned int i = 0; i < count; ++i) {
            XWindowAttributes attr;
            if (XGetWindowAttributes(display, children[i], &attr)
                && attr.map_state == IsViewable && !attr.override_redirect) {
                target = children[i];
            }
        }
        if (children) XFree(children);
        if (target == None) return 7;
        XSetInputFocus(display, target, RevertToParent, CurrentTime);
        XSync(display, False);
    }
    for (int i = 0; i < repeat; i++) {
        if (!XTestFakeKeyEvent(display, code, True, CurrentTime)) return 8;
        if (!XTestFakeKeyEvent(display, code, False, CurrentTime)) return 9;
    }
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
XTEST_C
  if "${FOCUS001_CC:-cc}" -o "$scratch/xtest-key" "$scratch/xtest-key.c" -lXtst -lX11 \
       >"$scratch/xtest-build.log" 2>&1; then
    xtest_ready=1
  fi
fi

# The filters refer to individually compiled #[ignore] tests; never combine
# them into a single --ignored GTK process (GTK init is thread-sensitive).
tests=(
  gtk_focus001_negative_control_detects_stolen_focus
  gtk_focus001_mediathek_down_up_roundtrip
  gtk_focus001_mediathek_horizontal_filter_roundtrips
  gtk_focus001_live_first_row_up_to_search
  gtk_focus001_streams_row_left_stays_in_page
  gtk_focus001_streams_expanded_actions_right_ten_retains_owner
  gtk_focus001_streams_collapsed_star_native_reachability
  gtk_focus001_global_mediathek_search_up_down_switcher
  gtk_focus001_streams_favourites_footer_async_focus_retention
  gtk_focus001_streams_back_escape_home_are_semantic
  gtk_focus001_streams_collapsed_star_two_key_reachability
  gtk_focus001_xtest_live_search_down_native_propagation
  gtk_focus001_xtest_mediathek_search_down_up
  gtk_focus001_live_search_switcher_roundtrip
  gtk_focus001_streams_search_switcher_roundtrip
  gtk_focus001_live_search_keyboard_horizontal_reverse
  gtk_focus001_mediathek_compact_toolbar_ownership_inventory
  gtk_focus001_streams_empty_toolbar_ownership_inventory
  gtk_focus001_app_menu_escape_restores_focus
  gtk_focus001_mediathek_osk_modal_escape_return
  gtk_focus001_xtest_live_card_ok_focuses_play
  gtk_focus001_xtest_live_card_right_focuses_play
  gtk_focus001_xtest_manage_sources_refresh_down_reaches_lower_row
  gtk_focus001_xtest_manage_sources_three_rows_native_modal_actions
  gtk_focus001_xtest_mediathek_complete_toolbar_graph
  gtk_focus001_xtest_global_three_page_switcher_activation
  gtk_focus001_xtest_negative_control_rejects_wrong_arrow
  gtk_focus001_xtest_osk_native_key_grid_and_layout
  gtk_focus001_xtest_programme_guides_empty_manager_and_add_dialog
  gtk_focus001_xtest_programme_guides_three_rows_native_actions
  gtk_focus001_xtest_streams_empty_toolbar_native_graph
  gtk_focus001_xtest_streams_virtualized_284_rows
  gtk_focus001_xtest_streams_virtualized_1484_rows
  gtk_focus001_xtest_streams_virtualized_10000_rows
  gtk_focus001_xtest_streams_virtualized_15000_rows
  gtk_focus001_xtest_live_osk_full_transaction_tv
  gtk_focus001_xtest_mediathek_osk_full_transaction_compact
  gtk_focus001_xtest_streams_osk_full_transaction_tv
  gtk_focus001_xtest_mapping_no_guide_keeps_card_focus
  gtk_focus001_xtest_mapping_catalogue_pager_cancel_return
  gtk_focus001_xtest_mapping_stale_refresh_blocks_apply
  gtk_focus001_xtest_favourites_footer_native_async_roundtrip
  gtk_focus001_xtest_streams_virtual_native_dpad_284
  gtk_focus001_xtest_streams_virtual_native_dpad_10000
  gtk_focus001_xtest_streams_virtual_native_dpad_15000
  gtk_focus001_xtest_streams_card_two_ok_fake_playback_return
  gtk_focus001_xtest_mapping_same_timestamp_catalogue_change_rejected
  gtk_focus001_xtest_mapping_current_guide_apply_and_return
  gtk_focus001_xtest_mapping_removed_guide_rejects_apply
  gtk_focus001_xtest_programme_guides_vertical_three_row_dpad
  gtk_focus001_xtest_programme_guides_complete_enabled_grid
  gtk_focus001_xtest_manage_sources_complete_three_row_grid
  gtk_focus001_local_rendered_indicator_negative_control_probe
  gtk_focus001_xtest_osk_qwertz_reopen_retains_layout
  gtk_focus001_xtest_manage_sources_empty_add_guide_connected
  gtk_focus001_xtest_manage_sources_confirm_disposable_delete_and_fallback
  gtk_focus001_xtest_guides_add_blank_rejected_and_cancel_connected
  gtk_focus001_xtest_streams_populated_toolbar_compact_native
  gtk_focus001_xtest_streams_populated_toolbar_tv_native
  gtk_focus001_xtest_streams_virtual_native_dpad_1484
  gtk_focus001_xtest_streams_manage_launcher_connected_compact
  gtk_focus001_xtest_streams_manage_launcher_connected_tv
  gtk_focus001_xtest_live_connected_toolbar_compact
  gtk_focus001_xtest_live_connected_toolbar_tv
  gtk_focus001_xtest_mediathek_connected_toolbar_compact
  gtk_focus001_xtest_mediathek_connected_toolbar_tv
  gtk_focus001_xtest_streams_connected_toolbar_compact
  gtk_focus001_xtest_streams_connected_toolbar_tv
  gtk_focus001_xtest_source_switch_one_native_persist
  gtk_focus001_xtest_source_switch_three_native_persist
  gtk_focus001_xtest_source_manager_upper_graph_compact
  gtk_focus001_xtest_source_manager_upper_graph_tv
  gtk_focus001_xtest_source_switch_one_reopen_reversal
  gtk_focus001_xtest_source_manager_lower_graph_compact
  gtk_focus001_xtest_source_manager_lower_graph_tv
  gtk_focus001_xtest_source_defaults_nested_compact
  gtk_focus001_xtest_source_defaults_nested_tv
  gtk_focus001_xtest_source_rename_nested_compact
  gtk_focus001_xtest_source_rename_nested_tv
  gtk_focus001_xtest_source_lower_replace_native_path
  gtk_focus001_xtest_source_lower_delete_native_path
  gtk_focus001_xtest_source_lower_reorder_down_native_path
  gtk_focus001_xtest_guide_shared_default_confirmation_persists
  gtk_focus001_xtest_guide_remove_keep_native_roundtrip
  gtk_focus001_xtest_source_manager_home_escape_semantics
  gtk_focus001_xtest_guide_manager_home_escape_semantics
  gtk_focus001_xtest_osk_alphanumeric_complete_rows_compact
  gtk_focus001_xtest_osk_alphanumeric_complete_rows_tv
  gtk_focus001_xtest_osk_symbols_complete_rows_compact
  gtk_focus001_xtest_osk_symbols_complete_rows_tv
  gtk_focus001_xtest_streams_card_full_action_graph_ok
  gtk_focus001_xtest_streams_card_full_action_graph_right
  gtk_focus001_xtest_streams_source_selector_populated_native_clear
  gtk_focus001_xtest_mediathek_loaded_results_compact
  gtk_focus001_xtest_mediathek_loaded_results_tv
  gtk_focus001_xtest_mediathek_middle_card_ok_primary_play
  gtk_focus001_xtest_mapping_card_native_portal_no_seeding
  gtk_focus001_recovery_source_graph_0_compact
  gtk_focus001_recovery_source_graph_1_compact
  gtk_focus001_recovery_source_graph_3_compact
  gtk_focus001_recovery_source_graph_0_tv
  gtk_focus001_recovery_source_graph_1_tv
  gtk_focus001_recovery_source_graph_3_tv
  gtk_focus001_recovery_guide_graph_0_compact
  gtk_focus001_recovery_guide_graph_3_compact
  gtk_focus001_recovery_guide_graph_0_tv
  gtk_focus001_recovery_guide_graph_3_tv
  gtk_focus001_recovery_osk_vertical_live_compact
  gtk_focus001_recovery_osk_vertical_live_tv
  gtk_focus001_recovery_osk_vertical_mediathek_compact
  gtk_focus001_recovery_osk_vertical_mediathek_tv
  gtk_focus001_recovery_osk_vertical_streams_compact
  gtk_focus001_recovery_osk_vertical_streams_tv
  gtk_focus001_recovery_virtual_284_page_boundary
  gtk_focus001_recovery_virtual_1484_page_boundary
  gtk_focus001_recovery_virtual_10000_page_boundary
  gtk_focus001_recovery_virtual_15000_page_boundary
  gtk_focus001_recovery_mediathek_actions_compact
  gtk_focus001_recovery_mediathek_actions_tv
  gtk_focus001_recovery_rich_group_compact
  gtk_focus001_recovery_rich_group_tv
  gtk_focus001_recovery_rich_language_compact
  gtk_focus001_recovery_rich_language_tv
  gtk_focus001_recovery_rich_country_compact
  gtk_focus001_recovery_rich_country_tv
  gtk_focus001_recovery_rich_region_compact
  gtk_focus001_recovery_rich_region_tv
  gtk_focus001_recovery_source_graph_2_compact
  gtk_focus001_recovery_source_graph_2_tv
  gtk_focus001_recovery_nested_guide_0_compact
  gtk_focus001_recovery_nested_guide_0_tv
  gtk_focus001_recovery_nested_guide_3_compact
  gtk_focus001_recovery_nested_guide_3_tv
  gtk_focus001_recovery_painted_live_compact
  gtk_focus001_recovery_painted_live_tv
  gtk_focus001_recovery_painted_mediathek_compact
  gtk_focus001_recovery_painted_mediathek_tv
  gtk_focus001_recovery_painted_streams_compact
  gtk_focus001_recovery_painted_streams_tv
  gtk_focus001_xtest_source_manager_actual_remote_back_and_home
  gtk_focus001_xtest_guide_manager_actual_remote_back_home_escape
  gtk_focus001_cont012_mediathek_populated_back_vs_up_compact
  gtk_focus001_cont012_mediathek_populated_back_vs_up_tv
  gtk_focus001_cont012_menu_live_preferences
  gtk_focus001_cont012_menu_live_add_playlist
  gtk_focus001_cont012_menu_live_manage_sources
  gtk_focus001_cont012_menu_live_fullscreen
  gtk_focus001_cont012_menu_live_shortcuts
  gtk_focus001_cont012_menu_live_about
  gtk_focus001_cont012_menu_mediathek_preferences
  gtk_focus001_cont012_menu_mediathek_add_playlist
  gtk_focus001_cont012_menu_mediathek_manage_sources
  gtk_focus001_cont012_menu_mediathek_fullscreen
  gtk_focus001_cont012_menu_mediathek_shortcuts
  gtk_focus001_cont012_menu_mediathek_about
  gtk_focus001_cont012_menu_streams_preferences
  gtk_focus001_cont012_menu_streams_add_playlist
  gtk_focus001_cont012_menu_streams_manage_sources
  gtk_focus001_cont012_menu_streams_fullscreen
  gtk_focus001_cont012_menu_streams_shortcuts
  gtk_focus001_cont012_menu_streams_about
  gtk_focus001_cont008_virtual284_full_native_roundtrip
  gtk_focus001_cont008_virtual1484_full_native_roundtrip
  gtk_focus001_cont008_osk_remote_live_compact
  gtk_focus001_cont008_osk_remote_live_tv
  gtk_focus001_cont008_osk_remote_mediathek_compact
  gtk_focus001_cont008_osk_remote_mediathek_tv
  gtk_focus001_cont008_connected_guide_escape_compact
  gtk_focus001_cont008_connected_guide_escape_tv
  gtk_focus001_cont008_connected_guide_back_compact
  gtk_focus001_cont008_connected_guide_back_tv
  gtk_focus001_cont008_connected_guide_home_compact
  gtk_focus001_cont008_connected_guide_home_tv
  gtk_focus001_cont008_streams_osk_remote_compact
  gtk_focus001_cont008_streams_osk_remote_tv
  gtk_focus001_cont008_native_far_1484_page_end_home
  gtk_focus001_cont008_native_far_10000_page_end_home
  gtk_focus001_cont008_native_far_15000_page_end_home
  gtk_focus001_cont008_painted_search_live_compact
  gtk_focus001_cont008_painted_search_live_tv
  gtk_focus001_cont008_painted_search_mediathek_compact
  gtk_focus001_cont008_painted_search_mediathek_tv
  gtk_focus001_cont008_painted_search_streams_compact
  gtk_focus001_cont008_painted_search_streams_tv
  gtk_focus001_cont008_painted_source_manager_compact
  gtk_focus001_cont008_painted_source_manager_tv
  gtk_focus001_cont008_painted_guide_manager_compact
  gtk_focus001_cont008_painted_guide_manager_tv
  gtk_focus001_cont008_source_card_ok_compact
  gtk_focus001_cont008_source_card_ok_tv
  gtk_focus001_cont008_source_card_right_compact
  gtk_focus001_cont008_source_card_right_tv
  gtk_focus001_cont008_live_five_rows_compact
  gtk_focus001_cont008_live_five_rows_tv
  gtk_focus001_cont008_live_middle_last_ok_compact
  gtk_focus001_cont008_live_middle_last_ok_tv
  gtk_focus001_cont008_live_middle_last_right_compact
  gtk_focus001_cont008_live_middle_last_right_tv
  gtk_focus001_cont008_app_menu_live_compact_back
  gtk_focus001_cont008_app_menu_live_compact_escape
  gtk_focus001_cont008_app_menu_live_compact_home
  gtk_focus001_cont008_app_menu_live_tv_back
  gtk_focus001_cont008_app_menu_live_tv_escape
  gtk_focus001_cont008_app_menu_live_tv_home
  gtk_focus001_cont008_app_menu_mediathek_compact_back
  gtk_focus001_cont008_app_menu_mediathek_compact_escape
  gtk_focus001_cont008_app_menu_mediathek_compact_home
  gtk_focus001_cont008_app_menu_mediathek_tv_back
  gtk_focus001_cont008_app_menu_mediathek_tv_escape
  gtk_focus001_cont008_app_menu_mediathek_tv_home
  gtk_focus001_cont008_app_menu_streams_compact_back
  gtk_focus001_cont008_app_menu_streams_compact_escape
  gtk_focus001_cont008_app_menu_streams_compact_home
  gtk_focus001_cont008_app_menu_streams_tv_back
  gtk_focus001_cont008_app_menu_streams_tv_escape
  gtk_focus001_cont008_app_menu_streams_tv_home
  gtk_focus001_cont008_menu_action_add_playlist_compact
  gtk_focus001_cont008_menu_action_add_playlist_tv
  gtk_focus001_cont008_menu_action_manage_sources_compact
  gtk_focus001_cont008_menu_action_manage_sources_tv
  gtk_focus001_cont009_menu_preferences_compact
  gtk_focus001_cont009_menu_preferences_tv
  gtk_focus001_cont009_menu_fullscreen_compact
  gtk_focus001_cont009_menu_fullscreen_tv
  gtk_focus001_cont009_menu_shortcuts_compact
  gtk_focus001_cont009_menu_shortcuts_tv
  gtk_focus001_cont009_menu_about_compact
  gtk_focus001_cont009_menu_about_tv
  gtk_focus001_cont008_dropdown_mediathek_compact_back
  gtk_focus001_cont008_dropdown_mediathek_compact_escape
  gtk_focus001_cont008_dropdown_mediathek_compact_home
  gtk_focus001_cont008_dropdown_mediathek_tv_back
  gtk_focus001_cont008_dropdown_mediathek_tv_escape
  gtk_focus001_cont008_dropdown_mediathek_tv_home
  gtk_focus001_cont008_dropdown_streams_compact_back
  gtk_focus001_cont008_dropdown_streams_compact_escape
  gtk_focus001_cont008_dropdown_streams_compact_home
  gtk_focus001_cont008_dropdown_streams_tv_back
  gtk_focus001_cont008_dropdown_streams_tv_escape
  gtk_focus001_cont008_dropdown_streams_tv_home
  gtk_focus001_cont010_source_move_up_compact
  gtk_focus001_cont010_source_move_down_tv
  gtk_focus001_cont010_guide_manager_nested_compact
  gtk_focus001_cont010_guide_manager_nested_tv
  gtk_focus001_cont010_global_switcher_boundaries_compact
  gtk_focus001_cont011_guide_edit_compact
  gtk_focus001_cont011_guide_remove_keep_tv
  gtk_focus001_cont011_guide_remove_confirm_compact
  gtk_focus001_cont012_guide_disable_apply_native_compact
  gtk_focus001_cont012_guide_enable_apply_native_tv
  gtk_focus001_cont012_guide_edit_osk_save_native_compact
  gtk_focus001_cont011_source_facet_portal_compact
  gtk_focus001_cont011_source_facet_portal_tv
  gtk_focus001_cont011_rich_source_facet_compact
  gtk_focus001_cont011_rich_source_facet_tv
  gtk_focus001_cont011_rich_group_facet_compact
  gtk_focus001_cont011_rich_group_facet_tv
  gtk_focus001_cont011_rich_language_facet_compact
  gtk_focus001_cont011_rich_language_facet_tv
  gtk_focus001_cont011_rich_country_facet_compact
  gtk_focus001_cont011_rich_country_facet_tv
  gtk_focus001_cont012_four_facet_intersection_clear_compact
  gtk_focus001_cont012_four_facet_intersection_clear_tv
  gtk_focus001_cont012_four_facet_sort_async_empty_restore_compact
  gtk_focus001_cont012_four_facet_sort_async_empty_restore_tv
  gtk_focus001_cont011_manager_all_action_reachability_compact
  gtk_focus001_cont011_manager_all_action_reachability_tv
)
# Fail closed when the public coverage ledger invents a test, omits a
# required mapped case, or uses an unrecognized evidence/outcome class.
# A repeated test ID is permitted only to document distinct measured states.
printf '%s\n' "${tests[@]}" >"$scratch/required-cases"
if ! awk -F '\t' '
  FNR == NR { required[$1] = 1; next }
  /^#/ || /^surface\t/ { next }
  {
    if (NF != 12) { printf "FOCUS001 LEDGER malformed columns line=%d\n", FNR > "/dev/stderr"; errors++; next }
    if ($8 !~ /^(PASS|RED|FLAKY|ENVIRONMENT|NOT_COVERED)$/) {
      printf "FOCUS001 LEDGER unknown outcome line=%d\n", FNR > "/dev/stderr"; errors++
    }
    if ($10 !~ /^(NATIVE_MAPPED|HELPER_ONLY|MODEL_ONLY|OWNER_PHYSICAL|NONE)$/) {
      printf "FOCUS001 LEDGER unknown evidence level line=%d\n", FNR > "/dev/stderr"; errors++
    }
    if ($2 ~ /^gtk_focus001_/) {
      if (!($2 in required)) {
        printf "FOCUS001 LEDGER phantom test=%s\n", $2 > "/dev/stderr"; errors++
      }
      seen[$2]++
    }
  }
  END {
    for (name in required) if (!(name in seen)) {
      printf "FOCUS001 LEDGER missing required test=%s\n", name > "/dev/stderr"; errors++
    }
    exit (errors ? 1 : 0)
  }
' "$scratch/required-cases" tests/focus/focus-coverage.tsv; then
  printf 'FOCUS001 ENVIRONMENT coverage ledger mismatch; required suite unrun\n' >&2
  exit 2
fi

# Transition guard: keep the exact original 141-case blocking gate while
# separately proving the expanded 267-case shard gate. This mode is permitted
# only with the immutable original-141 and full-267 registry fingerprints.
# It cannot silently drop or reorder baseline tests.
if [[ -n "${FOCUS001_BASELINE_141_SHA256:-}" ]]; then
  if [[ -n "${FOCUS001_ONLY_CASES:-}" || -n "${FOCUS001_SHARD_COUNT:-}" ||
        -n "${FOCUS001_SHARD_INDEX:-}" || ${#tests[@]} -ne 267 ]]; then
    echo 'FOCUS001 ENVIRONMENT incompatible or incomplete legacy transition gate' >&2
    exit 2
  fi
  all_sha=$(printf '%s\n' "${tests[@]}" | LC_ALL=C sort | sha256sum | cut -d' ' -f1)
  if [[ "$all_sha" != 56ff778fd9c3f97efc1b96da138c0186a8e370a7089eb1f8b02709576cd9f6c8 ]]; then
    echo 'FOCUS001 ENVIRONMENT full 267-case registration changed unexpectedly' >&2
    exit 2
  fi
  baseline_sha=$(printf '%s\n' "${tests[@]:0:141}" | LC_ALL=C sort | sha256sum | cut -d' ' -f1)
  if [[ "$baseline_sha" != "$FOCUS001_BASELINE_141_SHA256" ||
        "$baseline_sha" != de4499c97ea06c7f7ad03be9764195f05593bda925db43bfea27e03a5d84fba8 ]]; then
    echo 'FOCUS001 ENVIRONMENT original 141-case gate fingerprint mismatch' >&2
    exit 2
  fi
  tests=("${tests[@]:0:141}")
  printf 'FOCUS001 BASELINE_GATE registered=267 immutable_original_cases=141 sha=%s\n' "$baseline_sha"
fi

# Diagnostic cohort mode is optional. The required existing CI invocation leaves
# FOCUS001_ONLY_CASES unset, and continues to exercise EVERY registered case.
if [[ -n "${FOCUS001_ONLY_CASES:-}" ]]; then
  read -r -a selected <<< "${FOCUS001_ONLY_CASES//,/ }"
  (( ${#selected[@]} )) || { echo 'FOCUS001 invalid empty pilot cohort' >&2; exit 2; }
  declare -A selected_once=()
  for name in "${selected[@]}"; do
    [[ -z "${selected_once[$name]:-}" ]] || { echo "FOCUS001 duplicate cohort case=$name" >&2; exit 2; }
    selected_once[$name]=1
    found=0
    for registered in "${tests[@]}"; do
      [[ "$registered" != "$name" ]] || { found=1; break; }
    done
    (( found )) || { echo "FOCUS001 unknown cohort case=$name" >&2; exit 2; }
  done
  tests=("${selected[@]}")
fi

# Optional deterministic, bounded shard mode. The existing blocking CI
# invocation never sets these variables and still runs all 267 registered cases.
# Fail closed on partial configuration, overlap with explicit cohorts, an empty
# shard or non-decimal/out-of-range values. Registry order is locale-independent.
full_registry_count=${#tests[@]}
full_registry_sha256=$(printf '%s\n' "${tests[@]}" | LC_ALL=C sort | sha256sum | cut -d' ' -f1)
shard_count=${FOCUS001_SHARD_COUNT:-}
shard_index=${FOCUS001_SHARD_INDEX:-}
if [[ -n "$shard_count" || -n "$shard_index" ]]; then
  if [[ -n "${FOCUS001_ONLY_CASES:-}" || ! "$shard_count" =~ ^[0-9]+$ ||
        ! "$shard_index" =~ ^[0-9]+$ ]]; then
    echo 'FOCUS001 ENVIRONMENT invalid/incompatible shard selector' >&2; exit 2
  fi
  # Avoid bash octal interpretation of leading zeroes.
  shard_count=$((10#$shard_count))
  shard_index=$((10#$shard_index))
  if (( shard_count < 2 || shard_count > 32 || shard_index >= shard_count )); then
    echo 'FOCUS001 ENVIRONMENT shard index/count outside permitted range' >&2; exit 2
  fi
  mapfile -t sorted_registered < <(printf '%s\n' "${tests[@]}" | LC_ALL=C sort)
  selected=()
  for i in "${!sorted_registered[@]}"; do
    if (( i % shard_count == shard_index )); then
      selected+=("${sorted_registered[$i]}")
    fi
  done
  (( ${#selected[@]} > 0 )) || {
    echo 'FOCUS001 ENVIRONMENT selected shard is empty' >&2; exit 2;
  }
  tests=("${selected[@]}")
fi
# A single published-commit test executable may be reused across isolated
# Xvfb processes. No binary is inferred from a mutable Cargo target directory.
case_binary=${FOCUS001_TEST_BINARY:-}
binary_hash=UNSPECIFIED
# Shards are deliberately invalid unless linked to one immutable compiled
# binary and a durable per-case evidence directory; no Cargo-per-case fallback.
if [[ -n "$shard_count" && ( -z "$case_binary" || -z "${FOCUS001_PILOT_RESULTS_DIR:-}" ) ]]; then
  echo 'FOCUS001 ENVIRONMENT shard requires immutable test binary and result directory' >&2
  exit 2
fi
if [[ -n "$case_binary" ]]; then
  [[ -f "$case_binary" && -x "$case_binary" ]] || { echo 'FOCUS001 missing executable test binary' >&2; exit 2; }
  binary_hash=$(sha256sum "$case_binary" | cut -d' ' -f1)
fi
source_sha=$(git -c safe.directory="$PWD" rev-parse --verify HEAD)
source_tree=$(git -c safe.directory="$PWD" rev-parse --verify HEAD^{tree})
if [[ -n "$shard_count" ]]; then
  # Provenance is a required input, not a best-effort comment or mutable path.
  if [[ -z "${FOCUS001_EXPECTED_SOURCE_SHA:-}" ||
        -z "${FOCUS001_EXPECTED_SOURCE_TREE:-}" ||
        -z "${FOCUS001_EXPECTED_BINARY_SHA256:-}" ||
        -z "${FOCUS001_EXPECTED_REGISTRY_SHA256:-}" ||
        "$source_sha" != "$FOCUS001_EXPECTED_SOURCE_SHA" ||
        "$source_tree" != "$FOCUS001_EXPECTED_SOURCE_TREE" ||
        "$binary_hash" != "$FOCUS001_EXPECTED_BINARY_SHA256" ||
        "$full_registry_sha256" != "$FOCUS001_EXPECTED_REGISTRY_SHA256" ]]; then
    echo 'FOCUS001 ENVIRONMENT shard source/tree/binary/registry provenance missing or mismatched' >&2
    exit 2
  fi
fi
results_dir=${FOCUS001_PILOT_RESULTS_DIR:-}
if [[ -n "$results_dir" ]]; then mkdir -p -- "$results_dir"; fi
printf 'FOCUS001 SOURCE_SHA=%s SOURCE_TREE=%s BINARY_SHA256=%s\n' "$source_sha" "$source_tree" "$binary_hash"
printf 'FOCUS001 REGISTRY_SHA256=%s FULL_CASES=%d SELECTED_CASES=%d SHARD=%s/%s\n' \
  "$full_registry_sha256" "$full_registry_count" "${#tests[@]}" \
  "${shard_index:-ALL}" "${shard_count:-ALL}"
# The file is written only to an explicitly supplied diagnostic output dir.
# A sharded CI consumer must validate the registered set before interpreting
# per-case verdicts; a missing file/case is UNRUN, never silently PASS.
if [[ -n "$results_dir" ]]; then
  {
    printf 'format\tfocus001-shard-v1\n'
    printf 'source_sha\t%s\nsource_tree\t%s\n' "$source_sha" "$source_tree"
    printf 'binary_sha256\t%s\nregistry_sha256\t%s\n' "$binary_hash" "$full_registry_sha256"
    printf 'registry_count\t%d\nselected_count\t%d\n' "$full_registry_count" "${#tests[@]}"
    printf 'shard_index\t%s\nshard_count\t%s\n' "${shard_index:-ALL}" "${shard_count:-ALL}"
  } > "$results_dir/shard-manifest.tsv.tmp"
  mv -- "$results_dir/shard-manifest.tsv.tmp" "$results_dir/shard-manifest.tsv"
  printf '%s\n' "${tests[@]}" > "$results_dir/selected-cases.txt.tmp"
  mv -- "$results_dir/selected-cases.txt.tmp" "$results_dir/selected-cases.txt"
fi

record_pilot() {
  local name=$1 status=$2 code=$3 started=$4 log=$5
  [[ -n "$results_dir" ]] || return 0
  local filtered="$results_dir/$name.log"
  trace_excerpt "$log" > "$filtered"
  local log_sha
  log_sha=$(sha256sum "$filtered" | cut -d' ' -f1)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$status" "$code" "$source_sha" "$source_tree" \
    "$binary_hash" "$log_sha" "$(( $(date +%s) - started ))" > "$results_dir/$name.tsv.tmp"
  mv -- "$results_dir/$name.tsv.tmp" "$results_dir/$name.tsv"
}

pass=0
fail=0
timeout_count=0
environment=0
unrun=0

# Print only allowlisted focus diagnostics, bounded and scrubbed. Raw Cargo
# output stays in a disposable fixture directory, never in CI artifacts.
trace_excerpt() {
  local file=$1
  grep -E 'FOCUS001|^thread .+ panicked|^test .* FAILED|^test result:|^error: test failed|assertion failed' "$file" \
    | head -n 16 \
    | sed -E \
      -e 's#(https?|rtsp|ftp)://[^[:space:]]+#<redacted-url>#g' \
      -e 's/(token|password|secret|apikey|api_key)=[^[:space:]]+/\1=<redacted>/Ig' \
      -e 's/[^[:print:]\t]/?/g' || true
}

for test_name in "${tests[@]}"; do
  printf '\nFOCUS001 RUN %s\n' "$test_name"
  log="$scratch/$test_name.log"
  case_start=$(date +%s)
  case_status=UNRUN
  case_code=0
  case_root="$scratch/cases/$test_name"
  mkdir -p "$case_root/data" "$case_root/config" "$case_root/cache"
  # Every test relying on the native XTest fixture must fail closed as
  # ENVIRONMENT if compilation did not succeed. The recovery_* probes also
  # send XTest keys despite not containing `_xtest_` in their names.
  if [[ "$test_name" == *"_xtest_"* || "$test_name" == *"_recovery_"* ||
        "$test_name" == *"_cont008_"* || "$test_name" == *"_cont009_"* ||
        "$test_name" == *"_cont010_"* || "$test_name" == *"_cont011_"* || "$test_name" == *"_cont012_"* ||
        "$test_name" == *"_native_reachability" || "$test_name" == *"_two_key_reachability" ||
        "$test_name" == *"_app_menu_escape"* || "$test_name" == *"_osk_modal_escape"* ||
        "$test_name" == *"_rendered_indicator_negative_control_probe"* ]] && (( !xtest_ready )); then
    environment=$((environment + 1))
    case_status=ENVIRONMENT
    case_code=2
    echo "FOCUS001 ENVIRONMENT $test_name (XTest emitter unavailable)" | tee "$log"
    record_pilot "$test_name" "$case_status" "$case_code" "$case_start" "$log"
    continue
  fi
  test_command=(cargo test --locked "$test_name" -- --ignored --test-threads=1 --nocapture)
  if [[ -n "$case_binary" ]]; then
    test_command=("$case_binary" "$test_name" --ignored --test-threads=1 --nocapture)
  fi
  case_timeout=120s
  if [[ "$test_name" == gtk_focus001_cont008_virtual1484_full_native_roundtrip ]]; then
    case_timeout=300s
  fi
  if timeout --signal=TERM --kill-after=5s "$case_timeout" \
    xvfb-run -a env \
    GDK_BACKEND=x11 GSETTINGS_BACKEND=memory GSETTINGS_SCHEMA_DIR="$schema_dir" \
    XDG_DATA_HOME="$case_root/data" XDG_CONFIG_HOME="$case_root/config" XDG_CACHE_HOME="$case_root/cache" \
    FOCUS001_XTEST_BIN="$scratch/xtest-key" FOCUS001_PIXEL_BIN="$scratch/xtest-key" \
    "${test_command[@]}" >"$log" 2>&1; then
    if grep -Eq '^running 1 test$' "$log"; then
      pass=$((pass + 1))
      case_status=PASS
      printf 'FOCUS001 PASS %s\n' "$test_name"
    else
      unrun=$((unrun + 1))
      case_status=UNRUN
      printf 'FOCUS001 UNRUN %s (no unique test executed)\n' "$test_name"
      trace_excerpt "$log"
    fi
  else
    code=$?
    case_code=$code
    if ((code == 124 || code == 137)); then
      timeout_count=$((timeout_count + 1))
      case_status=TIMEOUT
      printf 'FOCUS001 TIMEOUT %s exit=%d\n' "$test_name" "$code"
    elif ((code == 125 || code == 126 || code == 127)) || \
         grep -Eqi '(FOCUS001 INPUT_UNVERIFIED|real X11 .* emission failed with Some\([3-9]\)|cannot open display|could not open display|gtk.*initializ.*fail|xvfb.*(failed|error)|failed to resolve dependencies offline)' "$log"; then
      environment=$((environment + 1))
      case_status=ENVIRONMENT
      printf 'FOCUS001 ENVIRONMENT %s exit=%d\n' "$test_name" "$code"
    else
      fail=$((fail + 1))
      case_status=RED
      printf 'FOCUS001 RED %s exit=%d\n' "$test_name" "$code"
    fi
    trace_excerpt "$log"
  fi
  record_pilot "$test_name" "$case_status" "$case_code" "$case_start" "$log"
done

printf '\nFOCUS001 SUMMARY required=%d PASS=%d RED=%d TIMEOUT=%d ENVIRONMENT=%d UNRUN=%d\n' \
  "${#tests[@]}" "$pass" "$fail" "$timeout_count" "$environment" "$unrun"
if ((pass != ${#tests[@]} || fail || timeout_count || environment || unrun)); then
  exit 1
fi
