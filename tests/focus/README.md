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

## Continuation-003 (test-only expansion; original base 452d12b)

Seven more independent, individually enumerated mapped GTK cases are included
for Live/My Channels global switcher return, Live Search↔Keyboard, compact
Mediathek's seven toolbar controls, the empty My Channels toolbar, the main
app-menu popover, and Mediathek OSK modal return. The first five use the real
mapped `TvWindow`/GTK ownership path but not hardware-equivalent input; the
last two require the native XTest emitter and verified editable input delivery.
The compact/empty-toolbar inventories intentionally claim **HELPER_ONLY**
focusability, not native directional coverage. Local CANDIDATE_DIRTY isolated-Xvfb execution on base `452d12b`
completed all 20 enumerated cases: **13 PASS / 7 known RED, 0 timeout / 0
environment / 0 unrun**. The new seven were all PASS locally, not a
post-publication published-commit rerun or CI result. Revalidate against
the exact Git commit after publication.

The TSV also enumerates explicit NOT_COVERED route and state classes including
all-tab native activation, Mediathek full adjacency, managers, guide mapping,
OSK grid, scroll virtualization and playback. No untested class is silently
called automated; rendered-focus contrast remains
**RENDERED_INDICATOR_NOT_AUTOMATED** until a reproducible pixel oracle with
negative control or an owner physical acceptance verifies it. Unknown toolbar
ordering or reverse-edge policy remains OWNER_DECISION_REQUIRED, not silently
asserted as a product design decision.

## Continuation-004 — native owner-card and source-manager canaries

The same blocking isolated runner now enumerates 23 mapped GTK cases. Three
new real XTest input cases exercise a disposable Live row and the **actual**
Manage Sources dialog created with a single synthetic, valid SQLite source.
The live row uses the production `TvLiveCard` and ListBox activation path;
merely entering it never invokes the Play action. The manager graph traverses
real enabled controls with directional key events; it **never clicks** Replace
or Delete. Test setup is `#[cfg(test)]` only. No real feeds, user data, network
requests, or mpv playback are used.

Locally against **CANDIDATE_DIRTY** based on published commit
`4c44bd7502cf658501c2a63b93202cc8d3e44068`, native tests show:
Live OK leaves focus on the card instead of Play (**RED**), while Live Right
focuses Play directly (**PASS**). The source manager Refresh/Defaults/Rename
upper controls and enable switch are reachable, but no observed native arrow
edge from that reachable subgraph enters the second row with Replace/Delete
(**RED**, one disposable source; both Move arrows disabled). The manager audit
records bounded actual-key traces; direct `grab_focus` is used only to revisit
an already discovered owner and must not be mistaken for a full physical replay.

All previously known RED tests remain active and mandatory. This extension
does **not** declare app-wide route completion: Programme Guides, all manager
mutations, exact per-key OSK grid, large GTK list virtualization, full
Mediathek adjacency, source reload, native card Play activation/return, and
rendered focus contrast still require automated or owner-physical evidence.
The prescribed spatial/reverse directions are owner-defined; absence of
route coverage is not an outstanding generic policy decision.

## Continuation-004 source recovery and extra route probes

The exact `afd260bf6428442282676ee2f76fb67aa386f71e` published source
was recovered from a verified full Git bundle. Two new **test-only** isolated
native-XTest probes inventory all seven Mediathek toolbar controls as a
bidirectional spatial graph (logging every initial arrow and reverse) and
exercise actual three-page switcher activation/focus return. These are not
production focus fixes; the baseline canaries remain blocking. Run status
must be distinguished from older published CI and from physical Wayland.

### New native graph candidate evidence (after verified source recovery)

The **dirty** four-path candidate on exact clean parent `afd260bf6428442282676ee2f76fb67aa386f71e` passed
locked/offline Cargo metadata, formatting, strict Clippy and ordinary unit tests
(325 PASS, 74 IGNORED). The expanded isolated Xvfb/XTest runner executed
25/25 cases: **14 PASS / 11 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN**;
it correctly returned nonzero. The two new native tests are RED and have
separately reproduced failures; they must remain failing until corrected.

The seven-control Mediathek mapped graph confirms every target is reachable
from Search, but five directional steps fail the opposite-key return oracle:
`0 Down -> 4 Up`, `1 Down -> 6 Up`, `2 Down -> 6 Up`, `4 Up -> 1 Down`,
`5 Up -> 1 Down`. No control-center move ran opposite the arrow, but full
reversibility is not satisfied. The global switcher check identifies a separate
entry-identity failure: native Up from **Live Search** enters an
`AdwViewSwitcherButton`, but pressing native OK *immediately activates
Mediathek* rather than remaining on Live. This is not a claim that Right
skips a page: it proves the initial portal chose a switcher control for the
wrong page. The test intentionally stops there until this is repaired.

Evidence is local CANDIDATE_DIRTY; published commit validation and CI at any
subsequent source SHA require independent transport. The first three-page
canary version assumed focus movement activated a tab; that invalid oracle
was corrected to require explicit OK and then augmented with a prerequisite
assertion that Up first focuses the *current* category. Only the corrected
version should be used for future product conclusions.
