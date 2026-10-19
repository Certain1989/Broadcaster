<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# On-screen keyboard: focused runtime verification

The keyboard edits a private GTK text draft and updates the source editable only on **Done**. Availability is on by default and can be turned off in Preferences → Input. On means *on demand*; focusing a text field does not open it automatically.

The current representative launchers are **Live search** and **Add Playlist → HTTP(S) URL**. A later integration step must provide independent launchers for all other editable field instances; none of those fields should be reported as covered yet.

## Automated mapped GTK fixture

The native Meson suite registers an Xvfb-backed, isolated GSettings fixture when `xvfb-run` is installed. To execute the same runner manually from the source root:

```sh
sh tests/osk/run-mapped.sh
```

The script compiles the app's test target with Cargo, starts a mapped GTK/X11 session, and runs `osk_mapped_gtk_` tests with a disposable memory GSettings backend. These tests exercise editor transaction boundaries, weak cleanup and mapped origin/parent/focus conditions. A passing build or synthetic display does **not** establish behavior for real Wayland remotes.

## Development Flatpak and Wayland checklist

Use the development Flatpak bundle from the exact pull-request commit being reviewed. Record its source commit, installed Flatpak commit, GDK backend and actual remote key mapping, but never include personal playlists, stream URLs, custom mpv arguments or authentication data in reports.

1. Open Live. With the D-pad only, navigate Search → Keyboard → results and back. Open Keyboard with OK. Verify that focus highlighting is visible and every key can be reached by Up/Down/Left/Right, including uneven action-key widths and row wrap. Type digits, symbols, a space, German `ä ö ü ß`, use Shift for capitals, and switch QWERTY/QWERTZ. Verify cursor moves, selection/backspace and long text visibility.
2. Check Done updates the search field once and returns focus. Repeat with Cancel, Back, Escape and Home: no change to original text/caret/selection or unintended page navigation. Test mouse and physical keyboard/paste/IME editing in the draft, as available.
3. Open My Channels → Add Playlist → Enter Playlist URL. Type only synthetic example URLs (for example `https://example.invalid/list.m3u?x=1`). Done edits the URL entry without saving/importing. Cancel and Back discard edits without closing the source editor. Check the original parent Save validation and open/close behavior independently. Keep actual playlists out of evidence.
4. Open Preferences → Input; switch On-screen keyboard OFF. Confirm the two launchers disappear immediately, ordinary editable typing still works, and remote navigation can return to Settings to re-enable it. Switch ON and confirm launchers return without restart; restart and check the saved state. On a fresh, isolated settings profile, confirm it defaults to ON.
5. In separate diagnostic GTK fixtures, exercise removed, destroyed, hidden/unmapped, insensitive, reparented, externally edited and parent-closed text origins; no ghost write or stale focus is acceptable. Confirm the keyboard/draft can be finalized after close. Exercise a real Preferences subpage, advanced expander and dynamically created popover; check modal Back/Escape/Home precedence and return focus.
6. Recheck existing workflows including mpv Arguments dynamic row Add/Remove/Restore/Undo and its immediate GSettings behavior. An OSK launcher is not yet attached to these additional fields; do not mistake native editing success for on-screen-keyboard integration.

Each check must be recorded separately as **PASS**, **FAIL** or **NOT RUN**, with source/build identification. Compilation and CI success do not substitute for a physical D-pad/Wayland observation.
