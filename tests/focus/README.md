<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: CC0-1.0 -->
# FOCUS-001 mapped GTK regression suite (independent, test-only)

Run `bash tests/focus/run-mapped.sh` from a complete Broadcaster checkout with
Cargo, Xvfb, GLib schema tools, GTK4/libadwaita and build dependencies.
The script compiles **disposable GSettings**, and each specifically enumerated
`#[ignore]` GTK test runs in its **own Xvfb process**, with a 120-second
process limit. The runner executes **all** cases, emits bounded redacted
focus diagnostics, counts PASS/RED/TIMEOUT/ENVIRONMENT/UNRUN, and exits nonzero
unless *every required case passes*. It must remain a blocking CI gate:
expected baseline RED is not permission to skip, remove or `continue-on-error`
a case. A Rust GTK test executed with `cargo test --locked` alone is ignored;
the script is the real CI entry point.

## What each oracle establishes

* **Mapped window capture**: calls the actual `TvWindow` remote-key handler
  against live mapped controls, then checks the GTK window's focus owner,
  ancestry, mapped/visible/sensitive state, and expected direction and reversal.
  Direct helper calls do **not** test GTK fallback propagation.
* **Native X11 input**: the runner compiles a disposable, standalone XTest
  key emitter using system X11 headers/libs, uses the real Xvfb display, and
  sends actual X key press/release events. Native-route cases first type `a`
  into the mapped application's focused `GtkSearchEntry`, verify the text was
  received, then clear it. A missing input-delivery proof is classified as
  **ENVIRONMENT**, not a product RED. No dependencies or runtime handlers are
  modified. XTest is not evidence of Fedora Wayland compositor/device behavior.
* **Negative controls**: focus-theft tests detect a wrong owner. There is no
  validated screenshot/contrast oracle for a *painted* focus ring and therefore
  no reliable missing-ring negative control; these remain **NOT_AUTOMATED**.
  In particular, `has_visible_focus() == false` does not establish ring loss
  where application CSS may style `:focus`.
* **Synthetic/isolated state**: the My Channels card fixtures use fabricated
  rows; the favourites footer case uses an isolated private temporary SQLite
  database. No owner playlist/guide, media URL, user DB, network stream or
  destructive real-data operation is used. Management dialogs, OSK, large row
  recycling and live/Wayland screenshot cues are still independent gates.

## Coverage and interpretation

`focus-coverage.tsv` lists concrete expected/observed edges, actual injection
method, direction semantics and limitations. Comment lines beginning `#` are
SPDX metadata. The appended FOCUS-001 continuation represents
**CANDIDATE_DIRTY** testing against full published parent
`07ef08eeceb39376e589bf2fc42dacba81466837`, not yet a clean rerun of a
subsequent published commit. On that candidate the isolated suite ran 13 cases:
**6 PASS / 7 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN**. These counts
are evidence of *known product/UI or focus-route mismatches*, not an assertion
that the suite as a whole passes. Source and further CI revisions require new
validation; ordinary unit tests do not exercise ignored mapped cases.

Known REDs include Mediathek Search Down→Up not reversing, My Channels row
Left unexpectedly selecting a global category, collapsed star unreachable by
sampled real X11 D-pad paths, favourites-footer async toggle leaving no focus
owner, native Live Search Down not reaching the first row, and native Mediathek
Down→Up not reversing. The two separate collapsed-star canaries check direct
Right and the bounded one-/two-key search. Production navigation fixes belong
**only** to a separately authorized worker; these regressions must not be
weakened or recast as intended behavior to obtain green CI.

Up/Down/Left/Right are directional operations; Back/Escape/Home are semantic
operations and have separate test coverage. Existing `#[ignore]` GTK tests
elsewhere are not universally covered here. The owner retains the final Fedora
Bigscreen Wayland physical remote and visible-ring acceptance gates.
