<!--
SPDX-FileCopyrightText: 2026 Broadcaster contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Experimental GTK keyboard — manual feasibility protocol

This branch is deliberately **non-shipping**: it wires a GTK-owned D-pad
prototype to **Live search** and the nested **Add Playlist → HTTP(S) URL**
field only. It is not application-wide integration, persistent Settings, or a
claim that a compositor touchscreen OSK works with a TV remote.

## Reproduce with mapped GTK widgets

1. Build this exact branch using the repository's native Meson/Cargo tooling.
   Start Broadcaster with `RUST_LOG=trace` **only with synthetic search and
   URL data**. Never publish actual playlist URLs or tokens in screenshots,
   logs, traces or CI artifacts.
2. Open **Live**, move focus to **Keyboard (prototype)** and press remote OK.
   Record actual focus and event order for Up/Down/Left/Right/OK/Back/Escape/
   Home, distinguishing window capture from dialog capture; the prototype
   emits navigation-event classes without text content.
3. Navigate the conventional letter rows (initial **QWERTY**) and compose
   `äöß`. Activate **Switch to QWERTZ** to select **QWERTZ** and confirm that
   Y and Z exchange their familiar positions; switch back using **Switch to QWERTY** and confirm the
   typed draft is unaffected. On every row, Right from the last key must wrap
   to that row's first key, and Left from its first to its last. Check the
   shorter action row and Up/Down from longer letter rows as well.
   Verify the caret separator sits directly against the text, with no
   character-sized blank before the newly inserted letter. Test caret
   Left/Right, Delete, Clear and Done. Confirm the original search changes
   only on Done and focus returns to its mapped entry. Repeat Cancel/Back;
   verify the original value and caret remain unchanged.
4. Open **My Channels → Add playlist → remote HTTP(S) URL**. Open the
   experimental keyboard inside that existing Adw.Dialog. Input
   `https://example.invalid/channel?x=1`. Done must edit the original URL
   *without* executing parent Save or initiating network traffic. Repeat
   Cancel/Back/Escape/Home: parent modal remains intact and focus returns.
5. Destroy or unmap the original editable *while keyboard is open* using a
   diagnostic GTK harness/inspector; verify no stale target update. Separately
   trace remote events in a Preferences subpage and a facet picker popover:
   these are focus/grab probes, **not wired keyboard integration**.
6. Run under native Wayland, X11 and Flatpak, as available, with an ordinary
   physical D-pad remote where possible. Record actual compositor, GTK/adw
   versions, GDK key symbols, source SHA and observed route. Xvfb only proves
   the simulated GTK path, not physical remote or real Wayland.
7. Retest ordinary physical text entry, paste, Back/Home, menu and modal
   Save/Replace behavior to catch unintended regressions.

Use `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked`, native Meson tests and existing CI jobs where available.
Distinguish PASS, FAIL and NOT RUN in the private handoff.

## Deliberate limits

This experiment must not declare success based solely on static inspection or green cargo
tests. It does not supply application-wide text fields, persistent opt-out,
translated keys/labels, production input validation, selection editing, or
production Preferences/popover integration. A failed GTK modal/grab proof
blocks production keyboard integration pending a new design decision, rather than reducing OSK scope.
