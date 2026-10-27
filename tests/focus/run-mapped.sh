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
    if (argc != 2) return 2;
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
    if (!XTestFakeKeyEvent(display, code, True, CurrentTime)) return 8;
    if (!XTestFakeKeyEvent(display, code, False, CurrentTime)) return 9;
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
  case_root="$scratch/cases/$test_name"
  mkdir -p "$case_root/data" "$case_root/config" "$case_root/cache"
  if [[ "$test_name" == *"_xtest_"* || "$test_name" == *"_native_reachability" || "$test_name" == *"_two_key_reachability" || "$test_name" == *"_app_menu_escape"* || "$test_name" == *"_osk_modal_escape"* ]] && (( !xtest_ready )); then
    environment=$((environment + 1))
    echo "FOCUS001 ENVIRONMENT $test_name (XTest emitter unavailable)"
    continue
  fi
  if timeout --signal=TERM --kill-after=5s 120s \
    xvfb-run -a env \
    GDK_BACKEND=x11 GSETTINGS_BACKEND=memory GSETTINGS_SCHEMA_DIR="$schema_dir" \
    XDG_DATA_HOME="$case_root/data" XDG_CONFIG_HOME="$case_root/config" XDG_CACHE_HOME="$case_root/cache" \
    FOCUS001_XTEST_BIN="$scratch/xtest-key" FOCUS001_PIXEL_BIN="$scratch/xtest-key" \
    cargo test --locked "$test_name" -- --ignored --test-threads=1 --nocapture >"$log" 2>&1; then
    if grep -Eq '^running 1 test$' "$log"; then
      pass=$((pass + 1))
      printf 'FOCUS001 PASS %s\n' "$test_name"
    else
      unrun=$((unrun + 1))
      printf 'FOCUS001 UNRUN %s (no unique test executed)\n' "$test_name"
      trace_excerpt "$log"
    fi
  else
    code=$?
    if ((code == 124 || code == 137)); then
      timeout_count=$((timeout_count + 1))
      printf 'FOCUS001 TIMEOUT %s exit=%d\n' "$test_name" "$code"
    elif ((code == 125 || code == 126 || code == 127)) || \
         grep -Eqi '(FOCUS001 INPUT_UNVERIFIED|cannot open display|could not open display|gtk.*initializ.*fail|xvfb.*(failed|error)|failed to resolve dependencies offline)' "$log"; then
      environment=$((environment + 1))
      printf 'FOCUS001 ENVIRONMENT %s exit=%d\n' "$test_name" "$code"
    else
      fail=$((fail + 1))
      printf 'FOCUS001 RED %s exit=%d\n' "$test_name" "$code"
    fi
    trace_excerpt "$log"
  fi
done

printf '\nFOCUS001 SUMMARY required=%d PASS=%d RED=%d TIMEOUT=%d ENVIRONMENT=%d UNRUN=%d\n' \
  "${#tests[@]}" "$pass" "$fail" "$timeout_count" "$environment" "$unrun"
if ((pass != ${#tests[@]} || fail || timeout_count || environment || unrun)); then
  exit 1
fi
