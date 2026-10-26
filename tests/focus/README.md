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

## Continuation-005 — additional real-key spatial probes (2026-10-26)

The same isolated blocking runner enumerates **29** cases, including four new
`NATIVE_MAPPED` tests that send real XTest key events to actual GTK controls.
The new independent wrong-arrow negative control verifies that the geometry
oracle rejects a deliberately false Left classification of an observed Right
move; it is separate from the existing stolen-focus negative control. Neither
control establishes rendered ring contrast.

* **OSK grid:** `gtk_focus001_xtest_osk_native_key_grid_and_layout` exercises
  real q/ w traversal and reverse, a left-edge to right-edge wrap and reverse,
  vertical modal ownership, native OK into isolated draft, Shift and QWERTZ
  relabelling, and Escape. It does *not* establish all keys, Done/Cancel,
  persistence across app restart, compact viewport, or focus-ring pixels.
* **Programme Guides:** `gtk_focus001_xtest_programme_guides_empty_manager_and_add_dialog`
  opens the actual mapped empty manager under disposable XDG directories;
  verifies native Add URL ↔ Choose Local XMLTV arrows, native OK into the
  nested URL dialog, and two Escape/refocus steps. No real guide is downloaded
  or modified. Multi-guide source rows, disabled/reorder action grids, refresh,
  confirmation, errors, stale state and mapping picker remain `NOT_COVERED`.
* **My Channels empty toolbar:** `gtk_focus001_xtest_streams_empty_toolbar_native_graph`
  independently sends all four native arrows from each mapped/enabled toolbar
  control, checks destination owner, direction and local reverse, and performs
  graph reachability starting at Search. Its empty state is intentionally not
  evidence for populated source facets, recycled rows or async focus.

These are additive route probes, **not completion of app-wide navigation coverage**. The test inventory
must retain `NOT_COVERED` for Programme Guides multirow and mapping, OSK full
keys/layout persistence, multiple Live/Mediathek card states, 284/1,484/10,000
mapped virtualized IPTV lists, native playback return, asynchronous source
and favourites edge states, and pixel-rendered focus contrast. The correct
separate compositor gate remains `RENDERED_INDICATOR_NOT_AUTOMATED /
OWNER_PHYSICAL` on Fedora KDE Plasma Bigscreen N100 Wayland. `has_focus()` and
`has_visible_focus()` do not measure a painted indicator. Known product REDs
remain mandatory and nonzero in both local runner and public CI.

The first 29-case **CANDIDATE_DIRTY** Xvfb run (parent `21d80bfd...`)
completed **17 PASS / 12 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN**.
The new empty My Channels native toolbar graph reaches **all 11 enabled
controls** but reports **six nonreversible adjacent arrow edges**. An initial
assertion also counted the Search/OSK upward page-switcher portals as escapes;
this is an oracle error, not a product fault. The corrected graph allows only
those deliberate Up portals while retaining strict reversal/direction and
reachability assertions. The corrected full 29-case **CANDIDATE_DIRTY** rerun independently confirmed
**17 PASS / 12 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN** (blocking exit 1).
Both deliberate focus-owner and wrong-direction negative controls PASS.
The six new toolbar reversal failures remain RED; top-row Up portals are
classified separately and never waived as ordinary reverse edges. These
candidate results are **not** clean post-publication or CI evidence.

## Continuation-006 — published-baseline reconciliation and new executable probes

**Source and evidentiary separation:** The complete 219-file application Git
checkout at `51ec867e01d4a3a29dae365687792f6a537706da`
(tree `2a5f53de11b8219bd5759d30bf79b9f1ebc0baf9`) was recovered from an
independently SHA-256-verified *full Git bundle*. The preceding 29-case native
suite was already checked from that published clean commit both locally and
in CI [run 40342903096](https://github.com/Certain1989/Broadcaster/actions/runs/40342903096):
**17 PASS / 12 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN**. An ordinary
`cargo test` does not execute the `#[ignore]` GTK cases. Older README sections
above are *historical candidate milestones* and do not override this verified
baseline. The authoritative TSV now separates **evidence level** (actual XTest
route, helper-only, model-only, or owner hardware) from **outcome** (PASS, RED,
FLAKY, ENVIRONMENT, NOT_COVERED), viewport, and exact published versus dirty
validation; it does not treat helper seeding or focus state as a pixel oracle.

All new tests are **CANDIDATE_DIRTY** against that immutable published
source until independently published, clean-source restaged and rerun. This
extension adds isolated XTest routes and disposable synthetic fixture helpers
in the already authorized `#[cfg(test)]` modules. The existing native runner
remains fail-closed and preserves all 12 published baseline RED assertions.

| Native package/state | Added executable test | Evidence and limits |
| --- | --- | --- |
| Live OSK at 1280×800 | `gtk_focus001_xtest_live_osk_full_transaction_tv` | XTest OK into mapped modal, real q/w reversal, letters, symbols, space, backspace, caret, case/layout; Done/Cancel return and edit commit/cancel; local PASS |
| Mediathek OSK at 640×480 | `gtk_focus001_xtest_mediathek_osk_full_transaction_compact` | Equivalent compact-viewport OSK key/control categories; local PASS |
| My Channels OSK at 1280×800 | `gtk_focus001_xtest_streams_osk_full_transaction_tv` | Equivalent real OSK key graph and origin restoration; local PASS |
| Three Programme Guides (one disabled) | `gtk_focus001_xtest_programme_guides_three_rows_native_actions` | Real registry and mapped manager, four action controls in three rows; native Right/Left across first row and Edit/Remove modal return; local PASS. No actual refresh, confirmation commit or multirow D-pad reachability proved |
| Three playlist sources | `gtk_focus001_xtest_manage_sources_three_rows_native_modal_actions` | Isolated SQLite fixture; all seven buttons in each actual mapped row, reorder sensitivity boundaries; native middle Refresh Down fails to reach enabled lower action (**RED**). Four modal transitions via native OK/Escape without destructive confirmation |
| My Channels large lists: 284 / 1,484 / 10,000 / 15,000 | `gtk_focus001_xtest_streams_virtualized_*_rows` | Actual GtkListView allocations with synthetic page backing and native input *canary*; helper-seeded scroll_to middle leaves focus owner unmapped (**RED**). Because the reposition uses `scroll_to` and terminates before the middle arrow, this is **helper-only mapped GTK recycling evidence, NOT proof of native D-pad long-list coverage** |

The runner also refuses to execute a ledger with malformed columns, unknown status or evidence classes, phantom tests, or any missing required runner case; multiple rows per genuine test may document distinct states. `NONE` means **no evidence at all** for an explicitly `NOT_COVERED` class, not a fifth verified evidence method. This is a documentation-integrity gate, not an excuse to skip RED tests.

Each synthetic sample is ephemeral. No actual media/guide URL is fetched, no
playback backend is started, and no owner data or mutable runtime executable
section is changed. Failing routes are **gating product findings**, not
expected-green/waived tests. Known same-SHA Live cross-host disagreements still
require an actual second-host deterministic comparison; there is no fabricated
consensus from this source branch.

### App-wide package coverage — tests, not product acceptance

| Package | Measured native area | Genuinely NOT_COVERED / owner gate |
| --- | --- | --- |
| A Managers | zero and three guide rows, one and three source rows; sampled real arrow actions and modal returns (one source lower-row RED) | complete connected route from each row to every enabled action, safe mutations/async/error/reorder/confirmation, cross-row reversal |
| B Mapping and facets | pre-existing synthetic card mapping action reachability only | real populated exact-guide picker, suggestions, scoped Apply/Cancel, full search pager, stale-refresh/remove negative CAS, facet picker interactive routes |
| C OSK | 3 real page modals at two viewports with sampled key categories and Done/Cancel; PASS | every row/edge/wrap/layout, editor/guide modal and preference across application restart, painted contrast |
| D Switcher and toolbar | prior 29-case published XTest empty/native and helper scans; RED reverse and wrong-page portal | populated toolbar, source selectors, full compact+TV control graph across all three pages, popover modal return |
| E Cards and playback | Live OK-to-Play RED, Right cross-host mismatch, My Channels collapsed/expanded action RED, sampled Mediathek | first/middle/last actual Native D-pad routes for all three card types, safe Play second-OK/failing-backend/return, Mediathek search/list/pagination |
| F Async and virtual rows | existing favourites footer RED; mapped GtkListView synthetic scroll+focus RED at 284/1484/10000/15000 | native arrows over recycled page boundaries, favourites ON→OFF→ON persistence, last favourite disappears, source update/rebind, bound latency/RSS |
| G Visual oracle and reliability | stolen owner and wrong-direction negative oracles PASS; structural GTK indicator checked | deterministic rendered pixel focus-ring contrast and deliberately hidden-ring negative control, real owner Wayland/N100 device, cross-host Live timing discrepancy |

**Stop rule:** The assignment is **PARTIAL** while any feasible interactive class in
this grid lacks native route evidence. A RED measured with correct native
input **counts as coverage**, but a not-run route, helper-only scroll,
structural focus flag, untested fixture or conjectured hardware obstacle does
not. There is no authorization to change production navigation in this
branch. The final product correction register and owner acceptance are tracked
separately, not published with the public test suite.


## Continuation-006 after ca066 — mapped guide picker, persistence, native recycling and pixel oracle

The earlier 38-case baseline belongs to published commit
`ca066e2d1a7f0c4754a6f8df2f3c94590feaf6ff` (21 PASS / 17 RED under
CI 40365806670). The **new cases in this section** were developed on a
separate **CANDIDATE_DIRTY** tree based on that exact commit, and must not
be described as validation of a new published SHA until a new Git bundle
is verified and the complete checkout is retested.

The mapped full-app test suite now enumerates 49 separate bounded cases.
Eleven additions exercise:

- **Programme mapping:** actual persisted synthetic IPTV card with no guide
  (PASS, no phantom dialog); two synthetic guides and 40 catalogue IDs
  through native search page two and Cancel with unchanged SQLite (PASS);
  refreshed guide `last_success_at` invalidating an open picker (PASS).
  An equally real native test altering guide data **within the same**
  `last_success_at` second confirms the previously reported stale-guide
  correctness defect: the stale selection is erroneously persisted (RED).
  This test-only worker does not change source mapping/CAS behavior.
- **OSK layout preference:** native QWERTZ selection, Done and reopen on
  the same mapped My Channels page resets to QWERTY instead of retaining
  the explicit choice (**RED**). Persistence across process restarts is
  separately unverified.
- **Favourites footer:** native XTest Return toggles ON→OFF→ON and SQLite
  persistence agrees, but the visible GtkButton loses focus after each
  toggle (RED). This is not merely a helper call or model-only finding.
- **Virtualized list:** actual compact 640×480 GtkListView models of 284,
  10,000 and 15,000 fabricated rows execute **real** native Down then Up
  28/50 steps with mapped focus and source-qualified stable row identity
  (PASS). Earlier helper-only `scroll_to` jumps in these large lists still
  discover an unmapped focused widget (RED); neither proves full deep-page
  and last-row D-pad traversal or latency/RSS limits.
- **Playback:** a first native OK expands a card without playback; a
  second deliberate native OK on the mapped Play control invokes a strictly
  test-gated synthetic player once, and Escape restores the originating row
  (PASS). Fullscreen mpv failure/return remains NOT_COVERED.
- **Actual rendered focus contrast:** disposable X11 `XGetImage` reads
  pixels from the real Live toolbar button, comparing focused/unfocused
  keyboard states. At Xvfb the actual button pixel digests are identical
  (RED). Deliberately **adding a visible outline** changes the pixel digest
  (positive control); deliberately **suppressing the focus outline** makes
  the digest identical again (negative control). This gives a working
  painted-ring oracle rather than CSS-string/GtkStateFlags speculation, but
  its finding applies to the observed Xvfb renderer; the owner's Wayland
  N100 hardware/signoff is still mandatory. No committed production CSS or
  GTK widget markup was changed.

Each case uses an independent temporary XDG data/config/cache root and
`GSETTINGS_BACKEND=memory` in an isolated Xvfb process to prevent test
order from contaminating persisted settings or synthetic SQLite fixtures.
The pixel capture is integrated into the ephemeral, compiled XTest helper
inside the **same** `run-mapped.sh` runner and retains the fail-closed exit
contract. No screenshot containing real user content is persisted.

**Unclosed areas:** complete connected manager action/reorder/edit graph,
full guide picker candidate scopes/facets and mid-dialog removal, editor
keyboard persistence, compact and populated cross-view toolbar direction,
first/middle/last real Live and Mediathek result cards, full playback failure
and fullscreen return, deep/last virtualized row and performance gates,
reproducible local-vs-CI Live XTest discrepancies, full 10k runtime focus
correction, and physical Fedora Wayland rendered ring. The test/audit
assignment remains PARTIAL until these feasible native routes are measured.
