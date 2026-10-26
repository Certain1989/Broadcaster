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
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    Display *display = XOpenDisplay(NULL);
    if (!display) return 3;
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
)
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
  if [[ "$test_name" == *"_xtest_"* || "$test_name" == *"_native_reachability" || "$test_name" == *"_two_key_reachability" || "$test_name" == *"_app_menu_escape"* || "$test_name" == *"_osk_modal_escape"* ]] && (( !xtest_ready )); then
    environment=$((environment + 1))
    echo "FOCUS001 ENVIRONMENT $test_name (XTest emitter unavailable)"
    continue
  fi
  if timeout --signal=TERM --kill-after=5s 120s \
    xvfb-run -a env \
    GDK_BACKEND=x11 GSETTINGS_BACKEND=memory GSETTINGS_SCHEMA_DIR="$schema_dir" \
    XDG_DATA_HOME="$scratch/data" XDG_CONFIG_HOME="$scratch/config" XDG_CACHE_HOME="$scratch/cache" \
    FOCUS001_XTEST_BIN="$scratch/xtest-key" \
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
