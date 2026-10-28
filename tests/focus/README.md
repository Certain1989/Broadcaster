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


## Continuation-006 after 975b03 — current/picker correctness and populated manager graphs

The prior 49-case published baseline is commit
`975b03b23b93099955f352c34f9ffa99422eb316`, tree
`f88d2eb2202204fe36abcd2c961a512c0514f0a1`.
It was recovered from the **complete independently verified** Git bundle,
then staged and validated as a clean 219-entry checkout using the canonical
`broadcaster-source-sync.sh stage`, `verify` and `check --mode all`. Cargo
metadata, formatting, Clippy `-D warnings` and 325 ordinary tests passed;
98 display-dependent tests were ignored by ordinary Cargo. This confirms
source provenance, **not** remote focus acceptance.

The test-only continuation adds five isolated Xvfb/XTest cases against a
separate candidate tree. Each activation sends actual XTest keys to visible,
sensitive, mapped GTK widgets; the setup `grab_focus()` calls are disclosed
and **do not** prove native D-pad reachability from the preceding card or
manager header. Every SQLite source and guide is disposable, no real URL is
fetched, and no production player is started.

| New coverage | Isolated local XTest evidence | Scope limit |
| --- | --- | --- |
| Current-guide mapping commit | Native exact-ID OK creates the correct source/ID override, closes modal and returns to same card action: **PASS** | No candidate scoring or entry from unexpanded card by all directions |
| Guide removed while mapping modal is open | External deletion of disposable guide/metadata followed by native candidate OK produces no orphaned override, keeps picker open: **PASS** | Not the full concurrent GUI guide manager refresh/delete transaction |
| Three-row Programme Guides vertical | Native Down/Down/Up/Up on real mapped default buttons, geometry-sorted: **PASS** | Does not activate Refresh or default-confirm commits |
| Three-row Programme Guides enabled graph | Every geometrically available Left/Right/Up/Down edge between the **11 enabled** real guide-row actions (one disabled Refresh) delivered by XTest: **PASS** | Does not prove navigating from toolbar into manager header, nesting all dialogs, persisted edits or pixel focus |
| Three-row Manage Sources enabled graph | 19 enabled real GTK controls, **58** genuine arrow events and **32** mismatched target edges: **RED**. Every mismatch is recorded without short-circuiting earlier cases | Safe source rename/reorder persistence, network/cancellation, focus-rendering and closing manager separately require coverage |

All five cases are **CANDIDATE_DIRTY** evidence until published and
independently checked at their new exact commit. Previous REDs remain required
and are **never** converted to expected-green skips. Even if every newly
measured route passes, the whole-app audit is **PARTIAL** until the remaining
explicit untested states in the TSV and D38 inventory receive native evidence.

## Continuation-007 — connected modal journeys, native populated toolbars and finite gaps

This test-only candidate starts from the **complete, exact published source
`2f7d1dd80a5fd9cc3a6b97be850cae020d8de10d`**, not the older bundled E4
source. All proposed changes here are inside the existing `#[cfg(test)]`
window test module, the blocking mapped-test runner, this README and the
public tab-separated route ledger; runtime widgets, focus handlers and
styles are unchanged. Until the exact new commit is published, source edits
and every locally executed check remain **CANDIDATE_DIRTY**, not a claimed
clean post-publication validation.

The 54-case published baseline was confirmed separately from CI run
[40384187602](https://github.com/Certain1989/Broadcaster/actions/runs/40384187602):
**54 required / 33 PASS / 21 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN**.
The first locally recorded candidate at the preceding revision was 32 PASS /
22 RED. This one-case host/result difference must not be papered over. The
same published SHA passed formatting, Clippy, ordinary unit tests, both
Flatpak architectures and REUSE; Rust CI correctly failed the mandatory
focus gate. The following six new, isolated GTK tests extend the suite to
**60 required** and are always blocking, including any new REDs:

* `gtk_focus001_xtest_manage_sources_empty_add_guide_connected` follows real
  native OK from a disposable, empty manager header into Add Playlist,
  inspects enabled Add actions and nested focus, escapes to the manager,
  opens Programme Guides and returns to the page. **PASS** locally.
* `gtk_focus001_xtest_manage_sources_confirm_disposable_delete_and_fallback`
  natively opens Delete, confirms Keep Source preserves the disposable
  SQLite row, reopens confirmation and deliberately deletes only this
  self-created row, verifies SQLite became empty and checks manager focus.
  **RED**: after the valid deletion the manager stays open, but its GTK window
  focus owner is `None`. This is a newly proven async/rebind defect, not a
  database deletion failure or real-data operation. The starting Delete
  button was seeded by GTK; native D-pad route into that lower action row
  is already separately RED and must not be inferred from this test.
* `gtk_focus001_xtest_guides_add_blank_rejected_and_cancel_connected` opens
  the empty guide manager, native OK into the Add URL dialog, native OK on
  blank Save Guide (validation rejects it), native OK Cancel and checks
  mapped return. No URL or network operation occurs. **PASS** locally.
* `gtk_focus001_xtest_streams_populated_toolbar_compact_native` and
  `gtk_focus001_xtest_streams_populated_toolbar_tv_native` install two
  synthetic My Channels rows and inject four native arrows from each
  visible, sensitive toolbar control at **640×480** and **1280×800**.
  A native Left consumed by `GtkSearchEntry` caret editing is *not* a
  spatial transition even when `GtkText` child bounds differ from the
  SearchEntry parent. The corrected oracle explicitly recognizes that
  retained logical focus. Each size sampled **44 key events**; **PASS**
  locally after this correction. Independent full graph reachability,
  reverse paths and rendered focus pixels remain separate requirements.
* `gtk_focus001_xtest_streams_virtual_native_dpad_1484` complements the
  existing 284/10,000/15,000 row probes with 1,484 actual mapped GTK rows
  under 640×480, 50 native Down then 50 Up, checked selected row, mapped
  owner and source-qualified starting identity. **PASS** locally. Middle
  and last page are **not** reached by this test.

**This does not close the app-wide audit.** There is an explicit, finite
sampled-state inventory in `focus-coverage.tsv`; **91 additional specific
missing route/state entries** are marked `NOT_COVERED` with evidence `NONE`.
Those are outstanding test tasks, not product failures and not additional
runner cases. The numbers are not a coverage percentage; a large count of
required tests can coexist with major *untested* classes. A genuine RED is
valid for a tested route, whereas a helper-invoked menu/card focus is not
proof that a remote can reach that action from its previous screen.

### Whole-app control/state coverage reconciliation

| Surface | Existing native or added evidence | Remaining `NOT_COVERED` native classes |
| --- | --- | --- |
| Playlist and guide managers | Empty/three-row source manager, full 19-enabled-action geometric probes (32/58 wrong), nested Rename/Defaults/Replace/Delete safe dialogs, one synthetic confirmed deletion, three-guide enabled grid, empty guide Add URL invalid/Cancel | Fully connected D-pad entry into every lower control, native source toggle/reorder/rename/replace/save/refresh with persistence, async failures, guide add/edit/default/removal commits, complete 640-pixel modal graph, Home/Back contracts |
| Programme mapping and facets | No-guide safe return, exact-guide selection/pager/Apply/Cancel, changed timestamp/removed guide rejection; same-second changed-catalogue RED | 0/1/many realistic candidate graph, explicit/playlist/shared guide scope variants, source/group/country/language selectors and clear, edited row disappearing and real concurrent update |
| On-screen keyboard | Three native text transactions and short QWERTY/QWERTZ/Shift navigation; QWERTZ same-session reopen RED | Complete key/row/symbol/caret edge graph, compact grid, form-specific edits, process-restart preference, rendered per-key focus |
| Switcher, toolbar and popovers | Native three-tab wrong-origin portal RED, full empty toolbar graphs, seven Mediathek filters, two new **populated My Channels** viewport arrow samples | Populated Live/Mediathek first/middle/last result routes at both sizes, nested facets/sort/popovers, all-tab entry/reverse, Home versus Back paths, true connected toolbar reachability |
| Cards and playback return | Native Live OK→Play RED / Right test has cross-host variation, My Channels first-OK then seeded explicit fake Play and Escape, star remote RED | Every Live/Mediathek/card action from header to primary and back, native first/middle/last results, safe fake playback failure/cancel, result paging and loading/error transitions |
| Favourites and virtual lists | Favourites ON/OFF/ON SQLite persistence with lost focus RED; native first 28/50 rows/reversal for 284/1,484/10,000/15,000 | Last-favourite removal, focused row disappearing, async rebind/source reload, genuinely native middle/last 10k/15k traversal, latency/RSS budget; helper `scroll_to` RED is **not** native long jump |
| Painted focus and semantics | Actual X11 Live keyboard cue pixel oracle RED with working visible and hidden-ring controls, owner/direction negative canaries PASS | Pixel probes on filter/star/card/footer/manager/guide/OSK, local-versus-CI timing/theme equivalence; final Fedora Bigscreen Wayland physical acceptance is distinct |

### Read-only action presentation inventory

| View | Concrete action markup | Current accessible/visual concern | Consistent proposed treatment for *later* implementation |
| --- | --- | --- | --- |
| Live card | `src/live/card.blp` primary `Gtk.Button` uses `play-symbolic`, localized *Play* tooltip, circular style | First OK leaves focus off Play in current native test; tooltip does not itself prove a distinct accessible name or painted ring | Keep Play icon, deliberate OK only when already on Play, explicit accessible name, visible ring and adequate mapped target |
| Mediathek card | `src/mediathek/card.blp` `Adw.SplitButton` uses `play-symbolic`; Copy `copy-symbolic`, Download `document-save-symbolic`, Website `globe-alt2-symbolic`, all with tooltips | More action types than Live; split-button menu and reverse edges need direct native testing | Same Play semantics; retain task-specific Copy, Download, Website icons with explicit accessible names and predictable split-button navigation |
| My Channels card | `src/streams/view.rs` expanded actions are **text** buttons `Play`, `Edit Metadata`, `Programme mapping`; collapsed star uses glyph `☆`/`★` with tooltip and `set_size_request(36, 36)` | Star is too small and native-inaccessible; equivalent Play differs visually from Live/Mediathek; tooltip alone is inadequate proof of accessible naming | `play-symbolic`, `document-edit-symbolic`, `view-list-symbolic`, `non-starred-symbolic` / `starred-symbolic` (Edit/List/star icons verified in the restored Adwaita theme); provide explicit changing accessible name, tooltip, ≥48×44 hit target, persistent visible focus |
| Playlist/guide managers | Multi-line **text** button grids (Refresh, Defaults, Rename, Replace, Delete and move arrows; guides use Default/Refresh/Edit/Remove) | Enabling, keyboard order and row wraps vary; 32/58 native source-arrow edge mismatches; newly proven deletion focus loss | Keep unambiguous text or icons with accessible labels; repair spatial navigation and async focus ownership in a separately authorized product pass |

The icon inventory is **read-only source evidence**, not a promise all
runtime icon names render with the same appearance on the owner's target
compositor, nor permission to edit Blueprint/CSS/production components. The
chosen proposed Edit/Mapping/star symbolic assets were present in the
restored Linux Adwaita icon directory. Final visual and hit-target
verification belongs to the implementation review and owner N100 check.

### Consolidated correction register (proposed, not authorized changes)

* **P0 — Primary focus and reachability:** Live first OK→Play, selected My
  Channels Left to global tab, collapsed favourite star reachability, async
  favourites footer disappearing focus; manager lower-row Replace/Delete
  32/58 wrong arrows and manager losing all focus after confirmed source
  deletion. Reproduction is retained in existing and new `gtk_focus001_*`
  cases. Suspected production handlers: `src/window.rs`, `src/live/view.rs`,
  read-only `src/live/card.rs`, `src/streams/view.rs`,
  `src/streams/management.rs`.
* **P1 — Form, scoping and cue consistency:** Mediathek reverse-edge
  failures, incorrect global switcher current-tab portal, guide/map picker
  stale-same-second Apply despite changed catalogue (data correctness), OSK
  QWERTZ per-dialog reset, focus paint missing under current Xvfb capture,
  My Channels Play/Edit/Mapping/star presentation and accessible/size
  consistency. Suspected product paths: `src/mediathek/view.rs`,
  `src/streams/editor.rs` and guide identity/version checks outside the
  test allowlist, `src/keyboard.rs`, `src/window.rs`, cards/Blueprint and
  stylesheet, plus editor/EPG DB binding paths as authorized separately.
* **P2 — Long-list identity and portability:** helper-seeded GTK
  virtualization jump leaves focus on an unmapped recycled list-item at
  sampled 284/1,484/10,000/15,000 indices; native first 28/50 rows PASS but
  middle/last remains uncovered. Live Search Down and Right-to-Play
  PASS/RED status differs by host at identical baseline revisions. Require
  bounded source-qualified row restoration and measured test input timing;
  investigate `src/streams/view.rs` and GTK/XTest fixture/compositor.

Proposed future production correction allowlist, subject to **separate owner
approval and independent review**, is `src/window.rs`, `src/live/view.rs`,
`src/live/card.rs`, `src/mediathek/view.rs`, `src/streams/view.rs`,
`src/streams/management.rs`, `src/streams/editor.rs`, `src/keyboard.rs`,
appropriate Blueprint/CSS/icon resources and exactly the M1 guide-binding
integrity paths required by an independently scoped stale-CAS correction.
Avoid database migration changes, quota expansion, changed security or
refresh policy merely to make focus tests green.

The test suite remains intentionally blocking until actual product fixes
pass; the test author must not mark the whole-app test/audit **DONE** while
one of the explicitly feasible inventory classes still reads `NOT_COVERED`.
This work does not replace the required independent integrated code review or
the owner's KDE Plasma Bigscreen N100/Wayland remote and rendered-focus test.


## Continuation-008 — mapped source-manager portal and cross-view connected toolbar evidence

This revision begins from the full, genuine, clean public FOCUS head
`71439e3a7574559bc0bf6ee6c270ef03b25e28b9`, Git tree
`a6aac1d39b3934865bfddcb39b6cc2173d141f3e`, independently staged
and checked via the canonical verified Git bundle and source-sync script.
The historically bundled E4 checkout was **not used** as source. No runtime
routing, CSS, production data, Cargo dependencies, migration or input quota
changed; all Rust additions remain inside the existing `#[cfg(test)]` module.

The two *new connected manager-launcher cases* were executed in isolated,
mapped Xvfb/XTest processes and PASS at 640×480 and 1280×800. With three
synthetic SQLite playlist sources, the native route starts at Search and uses
`Down, Down, Down, Right` (toolbar indices 0→2→5→8→9), then native Return
opens the real Manage Sources dialog. The manager has mapped Add Playlist,
Programme Guides and Close buttons; native Escape restores a mapped My
Channels owner. Test discovery seeds source controls only to build the
survey graph; **the recorded successful route is separately replayed from
Search using native key events**, not claimed from seeding Manage focus.
Native keyboard delivery is independently proved by actual SearchEntry text.
This closes the original `AUDIT_GAP_AS01` route at the two sampled viewports;
it does **not** establish that every manager card action is accessible.

Six further new cases independently run each enabled toolbar graph at compact
and TV size: Live **8** surveyed native arrows per viewport (Search + OSK
launcher), Mediathek **28** per viewport (seven currently enabled toolbar
controls), My Channels **44** per viewport (twelve-target inventory including
populated synthetic cards, with available enabled controls). All six locally
PASS with no native missing Search-to-toolbar route; every claimed reachable
control is visited by a separately replayed XTest path starting from Search.
A disabled control is not counted as reachable. These cases do not claim
loaded Mediathek results, Live first/middle/last rows, popup actions, reverse
arrow symmetry, stable rendered ring, or async card/state transitions.

### Exact original 91-gap reconciliation

The original 91 `AUDIT_GAP_*` IDs are retained unrenumbered in
`focus-coverage.tsv` and every row now specifies its missing test fixture,
entry/route/state and owning test-only source path or exclusive device
constraint. Current evidence closes exactly **1 of 91** (`AS01`) via both
native manager-launcher tests. The remaining 90 entries are **NOT_COVERED**
(89 feasible native omissions and one truly owner-physical N100 Wayland
check). Categories: A Sources **1/18** closed, A Guides **0/9**, B mapping
**0/14**, C OSK **0/9**, D switcher/composite graph **0/10**, E cards
**0/11**, F favourites/virtualization **0/11**, G actual rendered focus
**0/9**. Group D remains open despite newly verified toolbar subgraphs,
because its specific rows require populated result/card, switcher,
reverse/Back or modal combined journeys. The B/F/G exceptions similarly
cannot be closed by previously available helper-only, structural focus or
first-50-row evidence. `NOT_COVERED_PRECISE_BLOCKER` in column 9 is a test
implementation requirement, **not a claim that owner approval is needed for
feasible #[cfg(test)] test additions**. Only `AUDIT_GAP_G09` is genuinely
physical-only. New missing states, if found, must be added without silently
reducing this denominator.

This is **PARTIAL test/audit coverage**. Existing public known product RED
canaries remain blocking, including Live first-OK Play focus, Mediathek
reverse routing, source manager lower Replace/Delete and post-delete owner,
My Channels star/footer focus, QWERTZ reopen, same-second XMLTV mapping CAS,
rendered Live X11 focus cue, detached recycled list owner and host-dependent
Live behavior. No existing RED expectations or runner failure status were
relaxed. The remaining 90 gap rows are not phantom tests and must not be
counted as passes, observed defects, or 90 new required test functions.

### Consolidated production correction boundary (read-only recommendations)

**P0 remote interaction:** restore native first-OK Play focus in Live, reach
My Channels collapsed star and source-manager lower Replace/Delete; retain
source manager owner after deleting the only synthetic source; preserve
favourites footer focus across SQLite reload. Handlers for separate owner
approval: `src/window.rs`, `src/live/view.rs` (read-only `src/live/card.rs`),
`src/streams/view.rs`, `src/streams/management.rs`.

**P1 navigation/storage:** repair Mediathek up/down/Back graph and current-tab
switcher focus; persist OSK layout across reopen. The independently
reproduced same-second source-catalogue change still needs a monotonic
SQLite revision guard in the separately scoped EPG binding/ingest paths,
not a last-success-second timestamp comparison. Read-only candidate owners:
`src/window.rs`, `src/mediathek/view.rs`, `src/keyboard.rs`,
`src/streams/editor.rs`, `src/epg/bindings.rs`, `src/epg/ingest.rs`.

**P2 rendered/scale:** diagnose host-dependent Live XTest differences,
X11/GTK focus-ring cues, icon/tooltip/target-size inconsistencies and
GTK virtualization far-list selected-owner recycling. Keep CSS/icons,
production card code and GTK blueprint read-only for this test assignment.
Separate N100 Wayland/physical remote validation remains owner-only.

Individual actual key traces, reproduced source owners and the existing
cross-view Play/Copy/Download/Edit/Mapping/star icon/accessibility inventory
remain in earlier sections of this README and the public route ledger.
No code review, M1 acceptance, E5 activation, merge or release follows from
these tests.

### Additional source-switch coverage candidate (continuation-008)

Two isolated native XTest cases traverse Search → Manage Sources → a source
`GtkSwitch` using actual arrows/Return/Space and check disposable SQLite.
The one-source reverse-action case is **RED**: the initial OFF write persists,
but async manager rebind does not return focus to the same mapped switch, so
remote-only reversal is not assured. The three-source case is **PASS** for one
source-qualified OFF mutation, preservation of two other sources, and a mapped
manager focus owner after async reload. These were tested locally on a
`CANDIDATE_DIRTY` complete 10dc checkout, not yet CI or a published clean
commit. Only baseline `AUDIT_GAP_AS04` (native RED evidence) and
`AUDIT_GAP_AS05` (native PASS) have specific new executable closure evidence;
all other `NOT_COVERED` ledger entries remain open. The known product RED is
blocking and must not be suppressed or made an expected success.

## Synthetic source-manager native supplement (2026-10-27)

Five further individually isolated GTK/XTest cases join the two switch tests in the
blocking mapped runner (seven in the combined set). All use a temporary private SQLite fixture of one or
three fabricated sources; no user playlists, network refresh or owner data are
used. From page Search, native Down×3, Right, Return opens the mapped manager;
Down, Right reaches its first Switch. **No manager action or Switch is
programmatically focus-seeded for a claimed route.**

Local candidate (`10dc342abc85a4fa99f63c9d9e327be09662817c` plus test-only
diff) evidence: 4 PASS / 3 RED across these seven cases, with ordinary
locked/offline tests 325 PASS, Clippy, fmt and metadata PASS. The first Switch
can be toggled and reenabled through Escape and native manager reentry; SQLite
records both writes. The three-source case verifies an exactly one-row SQLite
change, three asynchronously refreshed mapped Switches and focus remaining in
the manager. At both 640×480 and 1280×800, Right traverses the upper action
row Switch → Refresh → Defaults → Rename with two verified Left reversals.

**Do not interpret these PASSes as full manager coverage.** Distinct mapped REDs
remain: after disabling the only source, focus does not return to that same
Switch for immediate reversal; from first-source Refresh, native Down does
not reach a lower-row source action at either viewport. Existing deletion,
source-edit/replace, source-reorder, other rows and multi-view audit gaps
remain separate and unclosed. Public CI/Flatpak and clean published-source
validation for this new candidate must be recorded separately after
publication. The blocking runner must remain nonzero for all REDs.


### Additional cross-surface mapped native probes (post-979 candidate)

The native runner includes source Defaults/Rename nested modal portals at
640×480 and 1280×800, source lower-row Replace/Delete/reorder reachability,
programme-guide default confirmation and removal cancellation with disposable
SQLite, and distinct Home/Escape modal handling for source and guide manager.
Actual individual results and original 91-row audit verdicts must remain
separate; each source/guide case enters a mapped GTK window with verified XTest.
Tests are deliberately blocking on unreachable actions or focus loss, not
expected-fail skips. This candidate does not authorize runtime fixes.


### Extended mapped app-wide coverage (post-979 test candidate)

Seven further isolated cases verify native OSK horizontal traversal for every
alphanumeric and symbol key row at two viewports, real My Channels card
Play/Edit/Mapping action portals by OK versus Right, and source-qualified
selector/Clear Filters behavior with a disposable three-source registry.
The OSK graph deliberately seeds the first key of each row; it proves native
horizontal row edges but does not by itself prove native inter-row entry,
rendered pixels, or process-persistent QWERTZ. Missing contracts remain open.


### Mediathek actual loaded-results native tests

Three further cases install synthetic `mediathekviewweb::models::Item` rows
through a test-only constructor into the *production* Mediathek ListBox and
card template. The source uses only `example.invalid` URLs; no upstream search,
file download or player action runs. Native XTest walks from Search through
filters to five actual cards at both viewports, reverses the list, and probes
OK → immediate Play focus on a middle card. Known REDs remain blocking.

### Additional full native audit work (exact 979 baseline, unpublished candidate)

The test-only continuation adds 22 genuine isolated mapped XTest cases across
source-manager nested Defaults/Rename/Replace/Delete/reorder, guide default and
remove/Keep, Home/Escape semantics, every letter/symbol OSK horizontal cell,
My Channels expanded-card Play portal, a real populated source facet,
Mediathek five synthetic loaded results at compact/TV size, and programme
mapping entry without pre-seeding the Mapping button. All are isolated with
synthetic disposable SQLite/ShowObject data and no live programme stream or
source URL. **15 PASS / 7 RED** individually, with no native environment or
case timeouts. Some early Mediathek attempts had an invalid asynchronous test
fixture (a search reset the synthetic items); those attempts were corrected
and retested and are not counted as application REDs.

Specific reproduced REDs: three native source-manager lower-row actions
unreachable; guide default persisted but no GTK focus owner after Confirm;
My Channels card OK/Right does not place focus on Play; source card native
Right x10 cannot reach Programme mapping without a focus seed. The source
manager, guide manager, and OSK positive cases cover only the actually asserted
routes, not all other actions of those widgets.

A separate eight-case seeded middle/last-row virtualized-list experiment was
removed rather than published: it failed **before native arrows started** on
an already known virtualized-focus setup, duplicating the existing large-list
REDs and providing no new boundary coverage. The original 91-ID inventory is
retained, with only individually justified completed native routes counted.
No full native 97-case matrix has been executed on this dirty candidate yet.
Every prior blocking RED and negative-control test remains present and required.


### Historical-source recovery native graph suite (new candidate)


### Recovery candidate on complete historical source

Twenty new native test-only probes cover source/guide manager directional
connectivity, three-view OSK vertical/reversal transitions and 132-step
GTK virtual page-boundary traversal. The fixture source is the genuine,
verified complete Git commit `979af365c59bea06421949c7734394aeaf6b9830`.
This is not the latest PR branch; publication and exact-current-source
validation are explicitly blocked until the full current SHA is transported.
A locally seeded action is **not** global route coverage: the graph auditor
requires BFS connectivity from the native modal entry, and far-last-row
scrolling remains unproven by 132-step page-boundary checks.

**Execution on complete dirty 979 source:** The 20 new native XTest
cases executed as 16 PASS, 4 RED, 0 timeout/environment/unrun. The four
REDs are 1/3-source connected manager cases at compact/TV sizes:
3/9 and 3/25 mapped actions reachable from the genuine modal entry.
Empty source managers, all four guide internal graph cases, six OSK
vertical cases and all four virtual page-boundary cases PASS. For the
large lists, key batches are native XTest events and a mapped owner and
logical index are checked after each bounded batch. This is NOT a
whole-list traversal to the last item. No automatic 91-gap closure or
current published-SHA acceptance is claimed. Existing product REDs and
test-only scope remain unchanged.

### Connected 2-source, nested guide and X11 painted ring probes (recovery)

Twelve additional independent native cases target the complete two-row source graph at compact/TV viewports, the actual mapped source manager to programme-guide manager entrance and reverse modal path with no/three synthetic guides, and native XTest pixel contrasts across Live, Mediathek and My Channels at both viewports. Positive and suppressed-ring CSS are applied **in tests only** as screenshot oracle controls. These are registered tests, not coverage credits before execution. Missing or RED results remain blocking.

### Rich-facet XTest candidate (not yet executed)
Eight tests use a disposable three-playlist SQLite catalogue with genuine normalized
group, language, country and region metadata; each tests native Space/Down/Return
and Clear Filters at compact and TV sizes. Do not claim equivalent complete
facet intersection/search/scroll coverage without additional mapped routes.

Two additional pending Mediathek cases traverse the four *actual* expanded
action controls (Play, Copy Video URL, Download, Open Website), discover the
native directional graph, replay each route from Play and check Escape identity
at both compact and TV viewports. They never activate an external URL.

## Distinct physical remote Back/Home canary — continuation recovery 2026-10-27

The prior `gtk_focus001_xtest_source_manager_home_escape_semantics` and
`gtk_focus001_xtest_guide_manager_home_escape_semantics` cases send the ordinary
X11 `Home` keysym. The application has a separate `HomePage` GDK remote action,
which maps from X11 `XF86HomePage`: the old tests do **not** prove remote-Home
correctness. Xvfb independently reported `XF86Back` and `XF86HomePage` keycodes
166 and 180, versus ordinary `Home` keycode 110, in this local session.

Two supplemental isolated mapped tests send the actual `XF86Back` and
`XF86HomePage` keys, assert dialog dismissal, source/guide return and the mapped
home view, and preserve native `Escape` as a separate semantics check. This
adds explicit regression oracles but **does not** close the broader AS16/AG08
manager route gaps merely through naming tests. Full native A–G closure remains
outstanding unless all required evidence is actually executed and recorded.
All fixtures use disposable source/guide records, without external network.


Local dirty-candidate supplemental XTest result, 2026-10-27: both genuine

targeted `XF86Back`/`XF86HomePage` tests **PASS (2/2)** using the verified

X11 emitter and mapped GTK focus assertions; no network/owner data used.

These are **CANDIDATE_DIRTY** checks on exact c2eb source plus 141-case test

patch, not clean published-source CI, and cannot on their own close compact

manager entrance/exit and rendered-cue routes.

Runner failure classification correction: XTest-dependent `recovery_*` and
rendered-indicator-negative-control cases are now explicitly guarded by the
XTest compilation preflight; absent compiled emitter or unsupported X11
keysym emits **ENVIRONMENT**, not a misleading application **RED**. The
original blocking test oracles and all runner cases are unchanged.

## Continuation-008 recovered candidate — full mapped native evidence (2026-10-27)

This is **CANDIDATE_DIRTY** evidence against exact complete source commit
`c2eb77da83c3ac9396bb36c8e1c0bf10347f62fa` / tree
`9d85e8a24ed68215d6dec7460f12453d1b8876fa`; it is not a
published-commit clean rerun, a CI pass, or a declaration that FOCUS-001
is complete. This recovered 141-case candidate was rebuilt from the
verified source-export Git bundle plus the saved additive test-only diff.
The full 141 independently isolated Xvfb/XTest native cases were executed
locally with the same test binary and true X11 emitter, using six concurrent
independent `/tmp` fixture directories and a 120-second per-case bound:

- **141 required distinct tests:** 93 PASS, **48 product RED**, 0 TIMEOUT,
  0 ENVIRONMENT, 0 UNRUN. The fail-closed aggregate status is **FAIL** as
  expected from deliberately retained product-regression assertions.
- Ordinary locked/offline Cargo metadata, `cargo fmt --check`, strict all-target
  Clippy `-D warnings`, and `cargo test --locked --offline` all passed;
  ordinary Cargo ran 325 tests and ignored 190 GUI tests. These ignored
  GUI tests are counted **only** through their separate mapped native run.
- The TSV contains **142 `gtk_focus001_*` rows for 141 distinct tests**:
  one existing test legitimately records two different route/state observations.
  Every named test row now records its fresh local result, while historical
  published evidence is retained in the verification field.
- Of the **91 original `AUDIT_GAP_*` IDs**, this conservative case-by-case
  reconciliation records **6 PASS / 11 RED / 74 NOT_COVERED**. Thirteen
  formerly uncovered IDs gained directly mapped native evidence; tests with
  only seeded focus, partial modal journeys, first-page virtualization, or
  incomplete action graphs did **not** close their broader route IDs.

The new PASS evidence includes native mapped genuine `XF86Back` and
`XF86HomePage` (distinct from keyboard `Home`), native letter and symbol
OSK row traversal, eight persisted group/language/country/region facet
checks across compact/TV viewports, bounded 284/1484/10000/15000-row
page-boundary traversal, and nested guide-manager modal routes. Bounded
native page-boundary traversal **does not demonstrate middle/last 15k row
reachability**, and the 74 open gaps are not silently reclassified as
physical-only.

The new RED evidence includes the three-page switcher's wrong current-tab
activation, source-manager connected lower grid/row failures, inaccessible
collapsed star, Mediathek Play-to-Copy/Download/Website disconnection,
My Channels Play/Edit/Mapping graph disconnection, same-second guide
catalogue stale Apply, and actual X11 rendered focus-ring pixel failures
in all three views. The X11 pixel oracle separately verifies a synthetic
positive outline changes pixels and a deliberately suppressed outline
does not. It does not substitute for the owner's N100 Fedora Wayland
remote/contrast acceptance or a separate production correction.

`tests/focus/run-mapped.sh` remains the blocking serialized CI entrypoint;
the faster local verification uses independent Xvfb and `/tmp` per-case
sandboxes but does not modify or replace required CI. This paragraph
corrects prior *pending/unvalidated* notes and records the executed state;
no runtime GUI/CSS/router code, production SQLite behavior, release, or
CI gate was changed. **FOCUS-001 test/audit still PARTIAL.**

## CONT-008 search-rendered focus native probes — candidate only

Six additional source-pinned ignored XTest probes check Live, Mediathek and My Channels SearchEntry rendered focus at 640×480 and 1280×800. They must prove native Right→Left focus ownership, native X11 pixel change on the SearchEntry, an intentionally painted positive outline and a deliberately suppressed negative control. These are **unexecuted candidate tests** until run; they cannot close G01 or app-wide painted focus. No runtime CSS or GTK route changed.

### CONT-008 local SearchEntry ring evidence — exact parent 21e71fd, CANDIDATE_DIRTY

Six new separate Xvfb/XTest native Search Right→Left and SearchEntry outer-border pixel measurements completed **3 PASS, 3 product-route RED, 0 TIMEOUT/ENVIRONMENT/UNRUN** after correcting an invalid full-widget oracle that measured the GtkText caret as a focus ring. Measured four border strips, synthetic positive ring and deliberately suppressed-ring negative only where the native route reached the SearchEntry. Mediathek compact/TV and Live TV PASS. Live compact and My Channels compact/TV RED on native geometric focus return or entrance, *before* rendered-ring assertion; do not describe them as painted-ring failures. Audit G01 can be marked **NATIVE_MAPPED RED**, with production navigation correction still required. These are local **dirty candidate** results, not a new clean published commit, CI pass, or owner Wayland device acceptance.

## CONT-008 manager painted-focus partial probe — candidate only

Four extra mapped native XTest cases examine genuine source-manager and synthetic guide-manager initial upper button, local Right/Left reversal and actual X11 framebuffer focus cue at 640×480 and 1280×800, calibrated with visible/suppressed synthetic focus outlines. They are **NOT RUN** until isolated native checks complete. This is not G05/G06 closure: lower manager actions, nested confirmation and OSK cell focus still need native evidence.

### CONT-008 actual manager painted-focus results — CANDIDATE_DIRTY 21e71fd

Four additional native mapped Xvfb/XTest pixel-calibrated manager upper-toolbar probes completed **4 PASS, 0 RED/TIMEOUT/ENVIRONMENT/UNRUN**: source manager and programme-guide manager at 640×480 and 1280×800. True source-manager launcher traversed from mapped Search; guide modal opened with disposable test fixture then first-to-second action focus was native. Upper-row focus pixels change; deliberate positive ring changes and suppressed-ring control remains identical. This is **partial G05/G06 evidence only**, not coverage of lower-row manager, nested dialogs, file/guide chooser or OSK cells. No production changes, owner Wayland acceptance or published clean-source run implied.

## CONT-008 source-card row portal tests — candidate only

Four additional test-only native cases attempt genuine XTest launcher→source manager→first mapped ListBoxRow entry and distinct **OK** versus **Right/Left** card portals at compact/TV size. These deliberately reject a helper-only `grab_focus()` on the desired action. Synthetic SQLite data only; no real source replacement, deletion or remote retrieval. They are NOT_COVERED until executed; failures remain product RED, not skipped.

### CONT-008 actual source-card portal REDs — dirty candidate 21e71fd

Four genuine native mapped manager first-source ListBoxRow portal tests completed **0 PASS / 4 reproducible product RED / 0 TIMEOUT/ENVIRONMENT/UNRUN**. The manager's actual native first Add Playlist→Down path focuses the ListBoxRow at both 640×480 and 1280×800 (not an artificial focused button). A deliberate native Return leaves focus on that row instead of its enabled first GtkSwitch; native Right does not enter the lower Replace/Delete/order action row. These are D38 geometric/primary-action defects, recorded under **AS02/AS03 NATIVE_MAPPED RED** rather than mistaking unsupported focus seeding for coverage. No source data or real user playlists were touched; source fixtures are disposable SQLite.

## CONT-008 real five-row Live ListBox — candidate only

Six test-only probes add five uniquely identified synthetic `TvLiveCard` rows to the mapped production `GtkListBox` and attempt genuine native Search→first/middle/last Down and Up, plus separate middle/last first OK and Right portals into primary Play without a second media action. The setup never accesses a provider or user stream. E01/E02 remain NOT_COVERED pending actual isolated native results.

### CONT-008 real five-card Live native results (CANDIDATE_DIRTY 21e71fd)

Six isolated mapped Xvfb/XTest cases with five **unique real `TvLiveCard`/ListBox rows**, injected only synthetic channel objects, completed **1 PASS / 5 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN**. Both viewports correctly enter the **first** mapped Live row directly from Search Down, but continuing native Down erroneously focuses an inner GtkButton instead of the next Live ListBoxRow. A native middle-row OK also leaves focus on TvLiveCard rather than primary Play. Only TV-sized Right entry passes the complete sampled journey. These are **E01/E02 NATIVE_MAPPED RED**, not finished implementation or a permission to alter production. E01 far-last and E02 remaining actions require retest after product correction. No actual stream/network/media playback occurred.

## CONT-008 F06 full 284-row native traversal candidate

New ignored isolated mapped-XTest test `gtk_focus001_cont008_virtual284_full_native_roundtrip` attempts 283 genuine D-pad Down keys and 283 genuine Up keys on an actual virtualized 284-row GtkListView. Fixture/page model creation occurs only once before native input; later helper jumps and `grab_focus()` are forbidden. At eight-key checkpoints assert mapped focus/real selected index; at the far last row assert bounded page cache; at return verify original source-qualified logical ID. **NOT YET EXECUTED** on the new candidate, so original gap F06 remains NOT_COVERED and no PASS or RED is inferred.

### CONT-008 completed F06 native 284 far-boundary check (CANDIDATE_DIRTY 21e71fd)

An isolated X11/XTest ignored GTK process **PASSED** on the real mapped 284-row GtkListView: actual 283 Down and 283 Up keys, with focus ownership and selected index checked after every <=8 input events. Real selection reached index 283, reversed to 0, kept its original source-qualified ID, and logged 1 cached page/284 slots and 40.72 s total; no subsequent fixture scroll or helper focus was used. `AUDIT_GAP_F06` is PASS for this exact **284-row native path**, not an inference about rendering contrast, async filter removal, 1484/10000/15000 far-last reachability, CI or N100 Wayland physical testing. Existing 132-key samplers remain separate. The native executable printed `1 passed; 0 failed` and exit verification uses that unambiguous test log; an outer tool orchestration timeout was not interpreted as a test failure.

## CONT-008 OSK genuine remote special-key semantics candidate

Four new isolated mapped XTest cases cover Live and Mediathek at compact/TV sizes. Each opens the real OSK through **Search Right + Return** (no direct launcher grab/click), checks native Down focus containment and separately applies XF86Back, Escape, and XF86HomePage. All three should dismiss the OSK and retain the original page without navigating behind the modal. **Not executed yet; C09 still NOT_COVERED across all nested editor/guide/My Channels contexts.**

### CONT-008 OSK genuine Back/Escape/Home local XTest results

Four new mapped-native cases executed twice after allowing normal AdwDialog focus restoration. Mediathek compact and TV: **2 PASS in both executions**; Live compact/TV each **FLAKY** across runs (one pass and one RED each). The two distinct Live failures were (1) after the modal closed, the focused owner was still unmapped; and (2) reopening from Search, real Right focused unmapped `GtkText` rather than mapped keyboard launcher. Real XTest input was confirmed; no run timed out or had environment faults. Native router and modal-asynchronous focus interaction need a later separately authorized production correction investigation. **C09 remains NOT_COVERED**, as My Channels and nested source/guide editors are not covered by these four tests. Do not smooth over the flip by longer sleeps/skipping tests.

### Continued native far-list key characterization (candidate only)

Three additional isolated tests use native X11 PageDown, End and Home on 1,484/10,000/15,000 synthetic mapped GtkListView rows. These **keyboard-only** shortcuts are not presumed present on the physical remote and do **not** close native D-pad far-last routes. Their outcome is a candidate until actually executed and entered in the ledger; existing RED tests remain blocking.

The full 1,484-row remote D-pad first-to-last-to-first path is a separate new isolated candidate test, with every logical selection checked; neither its result nor passage time is assumed before the full process exits. The 284-row case is preserved through a shared identical native traversal helper.

Two new My Channels native OSK remote-modal probes attempt to reach the keyboard from Search using actual Right keys, then test XF86Back, Escape, XF86HomePage, focus confinement and return. These are candidate-only until they run and do not cover editor OSK contexts.

Six connected nested guide-manager return tests traverse Search→Manage Sources→Programme Guides using native GTK spatial paths, independently checking Escape, XF86Back and XF86HomePage at compact and TV sizes. Pending execution, not automatically credited.


### Continuation-008 partial native findings — same verified 21e71fd parent, dirty candidate

- F07 **NATIVE_MAPPED PASS**: 1,484 rows, actual XTest 1,483 Down and 1,483 Up, midpoint/final/return selected indices, source-qualified identity, ~193s. 284-row route also PASS after helper refactor. Observed 1,484 created slots and one cached page: bounded by the fixture count but **not proof of bounded memory/recycling** at 10k/15k; F08/F09/F10/F11 remain open.
- Additional keyboard-only native PageDown/End/Up/Down/Home routes on 1,484, 10k, 15k PASS (3/3, zero environment/timeout). X11 physical keysyms were emitted, but ordinary keyboard End/PageDown are **not assumed remote control capabilities**, and this does not close far-last D-pad gaps.
- AG08 connected Search -> source manager -> guide manager with native Escape and XF86Back returns PASS at both viewport sizes; native XF86HomePage **RED at both** because nested source/guide modal remains visible. Measured 4 PASS/2 RED, no environment/timeout. The production defect is preserved, not corrected by test code.
- My Channels Search -> keyboard native route via Right/Return and actual XF86Back/Escape/XF86HomePage passes at both viewports (2/2); C09 remains NOT_COVERED for nested source/guide editor text lifecycle and Live OSK intermittent return.
- All outcomes above are **CANDIDATE_DIRTY on exact published parent 21e71fd**, not published-commit or CI results. All other previous 141 native cases and their blocking REDs are preserved. X11 tests do not claim Fedora N100 Wayland physical acceptance.

### CONT-008 native global application-menu Back/Escape/Home sampled graph (candidate)

Eighteen isolated cases exercise real XTest Menu opening from the focused Live, Mediathek and My Channels SearchEntry at compact/TV sizes. Each verifies six menu-model actions, native Down focus confinement, and separate real XF86Back, Escape or XF86HomePage dismissal plus restored mapped page owner. No MenuButton direct grab/popover popup is permitted as entry evidence. Test cases are **NOT_COVERED until isolated execution**; this is a bounded partial D08/D09 graph, not a completed action-by-action activation inventory.

### CONT-008 native app-menu action activation (candidate)

Four additional isolated cases, at compact/TV size, use physical Menu and Down×1/2 then Return to activate Add Playlist/Manage Sources from real My Channels Search. They require the newly opened dialog to own mapped focus and physical Back to restore My Channels. They use only disposable private SQLite, no external URLs; evidence **NOT_COVERED until actually executed**. Remaining Preferences/Fullscreen/Shortcuts/About menu actions remain untested.

### CONT-008 native channel/source dropdown popup dismissal (candidate)

Twelve isolated cases for Mediathek broadcaster and My Channels source GtkDropDown, two viewports and physical XF86Back/Escape/XF86HomePage. They reach the actual dropdown from a SearchEntry via bounded real XTest arrows (no target grab), then Space, Down, and dismissal; check focus confinement, selected value and active page. **NOT_COVERED until executed**. Does not cover every facet dropdown or asynchronous refresh.

### Continuation-009 menu-action test-oracle correction (unpublished candidate)

The six-menu-item action path is native XTest `Menu` → 0–5 `Down` events →
`Return`, with the actual focused `GtkModelButton` text verified before
activation. A registered `TvApplication` with the same `app.window()` is
required: an unregistered, separately allocated window can skip disabled
application menu items and send app-owned dialogs to another window. The
unregistered fixed-index menu oracle was invalid and must not be treated as a
product failure. The registered 12-case Xvfb sample (six enabled actions at
640×480 and 1280×800) returned 12 native RED: ten modal actions dismissed
with native Back but left focus on the global switcher rather than the
invoking My Channels view, while two fullscreen cases activated fullscreen
but could not reopen the menu for the reverse action. These are candidate
observations, not a complete app-wide D08 verdict or a published-CI result.

### Continuation-009 complete local native test evidence (2026-10-28, unpublished)

The same exact complete public parent `21e71fd7467b539f5e9ad597c2c9d61ec6d29d48`
was used for every fresh, isolated Xvfb/XTest **CANDIDATE_DIRTY** case. The
original 212-case sequential native suite ran all names and returned **145
PASS / 67 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN**. A first 220-case
scratch parallel run returned 119/101 but had **26 invalid fixture REDs**:
its temporary `XDG_DATA_HOME` was not under `/tmp`. That diagnostic aggregate
must never be used for application defect conclusions. After correcting
per-case `/tmp` isolation the complete candidate2 220-case native suite ran
**145 PASS / 75 RED / 0 TIMEOUT / 0 ENVIRONMENT / 0 UNRUN** with the
required nonzero exit. These executions are not CI or clean published-SHA
validation. GitHub published PR #46 remains at 21e; the existing CI focus
gate is deliberately blocking.

The six My Channels source dropdown cases previously RED were invalid
fixed-step oracles. A bounded **native XTest** Search → Group → Source route
now verifies the genuine GtkDropDown popup and distinct Back/Escape/Home
close behavior, with all six independently passing. The initial app menu
activation fixture was likewise invalid: an unregistered application did not
present the same focusable menu actions. Twelve **separate registered
application** tests (six named menu items at two viewport sizes) positively
verify the exact focused GtkModelButton label before native Return. All 12
returned RED: ten dialog actions activated but native Back restored focus
to the global tab instead of the My Channels origin; fullscreen toggled on
but the native Menu key failed to reopen the popover for two reverse cases.
These 12 were **not part of the corrected candidate2 220-case integrated run**;
they belong to the later candidate4 source with a registered fixture. Do not
combine the cohorts into one falsely integrated claim.

The TSV records one case row per one of the 220 unique runner names, with
current observed local verdicts, **FLAKY** for contradictory runs and the
original 91 `AUDIT_GAP_*` IDs preserved as an independent acceptance ledger.
The original 91 gap ledger is still **8 PASS / 17 RED / 66 NOT_COVERED**;
case-level PASS/RED alone never upgrades a gap. The full A–G connected
paths, visual focus positive/negative controls and N100 physical Wayland
evidence remain independent acceptance gates. Cross-host divergence remains
unresolved. Do not weaken the blocking runner or alter production source to
make it green. The latest private Coordination FOCUS-001 handoff contains
per-case evidence and an explicit future production-correction matrix.

All 18 three-view app-menu dismissal tests and all 12 Mediathek/My Channels
dropdown dismissal tests now have actual isolated PASS evidence with XTest
keys; their per-case TSV evidence level is `NATIVE_MAPPED`, not the pre-run
`NONE`. These passing *dismissal-only* cases do not demonstrate native
activation of every enabled menu action or cover all nested facet popovers,
so original D08 and D09 remain separately `NOT_COVERED`.

### Final same-source candidate4 220 native result (2026-10-28)

A subsequent **complete integrated** 220-case run of the same registered-menu
candidate4 source has now finished: **142 PASS / 78 RED / 0 TIMEOUT / 0
ENVIRONMENT / 0 UNRUN**. Each case used its own mapped Xvfb process, real
XTest key emitter and `/tmp` XDG/GSettings fixture; all names are accounted.
The three candidate2→candidate4 changes from PASS to RED were Live OSK return
at 640×480 and 1280×800, and compact Live five-row traversal. These remain
cross-run variation, not evidence that production was changed. All twelve
registered-menu action tests remain RED, while 18 cross-view app-menu
dismissal and 12 native channel/source dropdown dismissal cases are PASS.
The last long-run detached shell did not preserve its process exit-status file;
the captured 220-case results and blocking-script logic independently prove
78 RED, not a passing gate. No public CI run at candidate4 exists. This run
supersedes any cross-candidate composite when reporting overall native totals.

### Continuation-010 source reordering probes (candidate-only, 2026-10-28)

Two isolated native XTest probes (AS12 and AS13) reached the real source manager
through the observed Search → Manage Sources D-pad route and surveyed 25 mapped
header/source controls. Neither the middle card Move Up (640×480) nor Move Down
(1280×800) was reachable by real arrows from manager entry: both are **RED**.
The same tests then **explicitly seeded** the otherwise unreachable button and
sent a real XTest Return, proving synthetic SQLite ordering persisted and manager
focus remained valid. Seeded action success is a *functional diagnostic*, not
remote reachability and not complete closure of either original audit ID.

A first Move Up run timed out with ~73 seconds of cold Cargo compilation before
the native action completed; a warm rerun finished in 98.80 seconds with the
actual RED above. Move Down completed in 86.63 seconds with RED. The test-only
assertions intentionally retain the disconnected-path failure. The published
PR #46 has not been modified; other 220 previously executed case outcomes
retain their earlier candidate4 provenance.

#### Continuation-010 candidate (unpublished, full 21e Git parent)

The five added individual native cases have **not** been rerun together with all
220 predecessor cases. The original same-source 220-case result remains
**142 PASS / 78 RED** and must not be presented as the complete new 225-case
result. Four of five new cases have been isolated in a verified genuine XTest
mapped-GTK environment; their outcomes are as follows:

- **AG09:** both `guide_manager_nested_compact` at 640×480 and
  `guide_manager_nested_tv` at 1280×800 PASS. They traverse the actual
  My Channels → Source Manager → Programme Guides native controls, the
  Add URL dialog with real OK and Escape, then replay 14 control paths
  from authentic guide-manager entry with physical arrow reversals. This
  upgrades original AG09 to PASS only for this requirement, **not** painted
  pixel contrast (G06), guide editing, persistence, or independent Wayland.
- **AS12/AS13:** native manager entry and real arrow graph inspection find
  no connected path to second-source Move Up/Move Down. The separate
  explicitly **seeded-only** native Return diagnostics show working SQLite
  reorder and retained manager focus. Both combined cases stay RED, while
  complete original AS12/AS13 journeys remain NOT_COVERED; a seeded action
  is not a reachable remote action.
- **D06:** genuine Live Search Up at 640×480 focuses a mapped global
  `AdwViewSwitcherButton`, but native Return selects **Mediathek** instead
  of the active Live page. The new D06 test is RED before its later full
  first/last tab navigation could run. The original full D06 remains
  NOT_COVERED with this partial regression evidence. Earlier attempted
  fixed-order and stationary end-stop oracles were invalid and do not count
  as independent product regressions. No visual pixel-ring claim is made.

Every new case uses a separate native Xvfb/XTest process and disposable
`/tmp` XDG profile. The current source remains CANDIDATE_DIRTY; ordinary
`cargo test` ignores all such display-dependent GTK cases. The future-only
production correction register is retained in PRIVATE Coordination/Library;
this test-only branch does not repair application handlers, source quotas, UI
or CSS. A complete 225-case native run and separate CI/Wayland acceptance are
still outstanding.

Cont012 candidate adds a connected native guide settings Enabled/Apply disable persistence/focus probe. It is UNRUN until the verified candidate is compiled and executed; AG02 original full enable/disable coverage remains NOT_COVERED.

- AG02 two-way Enabled tests (compact disable / TV enable) use only disposable SQLite guides and real D-pad replay from mapped manager and settings origin. Both are UNRUN until exact-source isolated native confirmation; initial disabled state is a synthetic fixture, not counted as a remote action.

- AG04 prospective compact native guide Edit OSK test: XTest reach the guide Name keyboard launcher, type a key using actual mapped OSK and physical Return, Escape OSK, Apply from native editor route, check synthetic SQLite name and focus after repaint. UNRUN; do not promote audit until evidenced.

### Additional mapped-facet and source-management canaries

The new `gtk_focus001_cont011_*facet*` cases test physical XTest directional paths from the My Channels Search entry into a populated source, group, language or country drop-down at compact and TV viewports. They do not imply that a text OSK, multi-filter intersection, Clear Filters or cross-process persistence was tested. The `*manager_all_action_reachability*` cases measure enabled per-source row actions and replay arrows from the actual manager entry; missing action paths remain blocking RED rather than being bypassed by a seeded `grab_focus`. All fixtures use disposable synthetic SQLite.


### Continuation-012: compound persisted facet navigation (test-only candidate, not yet PR-published)

Two newly registered isolated GTK/XTest cases, `gtk_focus001_cont012_four_facet_intersection_clear_{compact,tv}`, start at the mapped My Channels Search entry, trace actual physical directional navigation to four real GtkDropDown controls, select each using native Space/Down/Return, and check three disposable SQLite playlist source identities through result changes `12 → 4 → 2 → 2 → 2`. They navigate to Clear Filters using XTest and verify a fresh async query restores all 12 rows from three source IDs, with per-step mapped/sensitive focus ownership. Both 640×480 and 1280×800 cases independently measured PASS against the same dirty test binary. A diagnostic-only graph survey seeds widgets **before** the claimed journey; **no intermediate seeded focus is counted as an executed remote route**. No production code, network source or real user playlist is used.

This directly adds a true connected group/source/language/country intersection and clear route relevant to original B10, **not** the unrelated guide mapping picker Apply/Cancel. It does **not** finish original F05 (sort and overlapping async reload), test rendered ring pixels for every selected facet, or prove a real Wayland/N100 remote run. Predecessor 243-case evidence and this distinct 2-case candidate have different full dirty-diff hashes; do not sum them into a same-source 245-case result until the final integrated native run. Historical original audit 91=9 PASS/18 RED/64 NOT_COVERED remains the immutable pre-continuation baseline.

### Continuation-012 F05 independent compounded filter/sort/async variants

The compact and TV `four_facet_sort_async_empty_restore` variants extend the real native
connected Search→four facets→Sort journey with XTest typing of a no-match string,
a debounced SQLite reload to zero rows, native Backspace restoring the
filtered geometry. Clear Filters after the changed Sort label reflows the
toolbar is deliberately not credited; its changed directional graph needs
separate measured connected traversal. The
fixture contains only local synthetic playlists (`example.invalid`). The expected
query sequence is 12→4→2→2→2→2→0→2, with real focus-owner checks after each
async query. Results are *F05 partial evidence* until executed and do not test
overlapping out-of-order in-flight replies, a painted pixel ring or full app scope.

### Continuation-012 isolated downstream persistence clauses

`streams::tests::focus001_cont012_component_three_source_edit_reorder_delete_reopen`
and `...component_last_favourite_removed_and_mode_reopen` are deliberately
**MODEL_ONLY / COMPONENT_ONLY**. They exercise disposable SQLite changes and
store reopen behind already documented disconnected manager-card actions; they
do not simulate remote access, a real app process restart or rendered focus.
Keep original AS08/09/12/13/14 and F02/03/04 classifications open until
full connected native acceptance or a proven product blocker is recorded.

### Continuation-012 populated Mediathek D10 Back versus Up

Two isolated mapped XTest cases enter five disposable programme rows from the
real Mediathek Search field via physical Down, verify Up from the first result
returns to Sort and Down reverses to that same card, then separately send
**XF86Back** from the same result and check its distinct broadcaster browse
portal and page containment. The tests do not conflate Back with Up or use a
synthetic router call instead of the mapped GTK controller. They remain D10
partial evidence until the complete duration/sort graph and painted cue are
measured.

Continuation-012 D08 expansion: 12 test-only native XTest cases independently activate all six enabled global menu actions from Live (compact 640x480) and Mediathek (TV 1280x800), including dialog focus/Back return and fullscreen restoration. Native results must be measured separately; existing My Channels menu cases do not certify other pages. Original D08 remains NOT_COVERED until the three-page action matrix is evaluated with original contract and rendered focus limitations.

D08 test-harness correction before classification: the first native probe accidentally used an unshown Live loading toolbar, causing invalid XTest text-delivery failures; also corrected the intentional Add Playlist/Manage Sources switch to My Channels according to the actual window action handlers. First 12-case run INVALID_HARNESS for Live and overly strict destination for those actions; archive diagnostic separately, do not count as product RED. Other real focus jumps after Back remain RED canaries pending rerun against corrected candidate.

### Continuation-012 D08 final third-page native action matrix (candidate)

Six further isolated `gtk_focus001_cont012_menu_streams_*` cases exercise the
same actual mapped global Menu→arrow selection→Return→XF86Back paths from the
My Channels Search origin at 640×480, with an empty disposable local SQLite
source fixture. They cover Preferences, Add Playlist, Manage Sources, Toggle
Fullscreen, Keyboard Shortcuts, and About. Add/Manage intentionally navigate to
My Channels, while other actions must restore focus to the invoking page.
**These six cases are UNRUN until the exact candidate binary has been executed.**
Do not promote the original D08 ID merely from activating each entry: the
return, fullscreen, reversibility and painted-indicator clauses still apply.

Corrected prior WIP23 AG02/AG04 ledger-only error: three already-executed
native guide enable/disable/Edit tests are RED with their source-specific log
SHA-256 fingerprints in the ledger, although the original original metadata
incorrectly retained UNRUN. No repeated native guide graph traversal is
necessary on unchanged production source, and reachability RED does not prove
that unexecuted Apply/OSK/persistence subclauses passed.

## Continuation-015: original-ID clause crosswalk (test-only, additive)

This clause map links existing registered native cases to *parts* of the
original FOCUS-001 acceptance IDs, not to a new acceptance verdict. It does
not modify the immutable 91-ID registry or replace physical Wayland review.
For the additional `src/window.rs` assertions below, the exact starting source
is `48b539d71c6ba0a9c1b587545e1874b1626a5c3b`; outcomes must be measured
on a subsequently published commit before being attributed to that commit.
`PASS` at one assertion level never means a compound original ID is PASS.

| Original ID / clause | Registered native case / viewport | Real fixture and test level | Observable assertion and expected outcome | Remaining blocker |
| --- | --- | --- | --- | --- |
| `AUDIT_GAP_B07`: populated Source selection and Clear focus ownership | `gtk_focus001_xtest_streams_source_selector_populated_native_clear`, existing default mapped viewport | Three disposable SQLite playlist sources, real GTK dropdown, XTest selection and Clear; **COMPONENT_ONLY** because test seeds focus on Source and Clear | Candidate adds `assert_owner` for selected Source and surviving Clear; **PASS** if both remain mapped, enabled and actual window focus owner, **RED** if not. Existing registered case remains blocking. | Native Search→Source route, searchable/scrollable popup, clear and complete origin restoration have not been established by this case. |
| `AUDIT_GAP_B08`: Country selector full native keyboard/OSK/clear/return | `gtk_focus001_cont011_rich_country_facet_compact` / `_tv` (640×480, 1280×800) | Three synthetic SQLite sources with persisted country facets; real XTest dropdown selection from Search after surveyed direction route; **NATIVE_MAPPED_PARTIAL** | Existing assertions expect nonzero selected country and retained mapped focus; any failed native path is **RED**, never skipped. | Country keyboard search, OSK, clear and inverse origin route not fully joined; no full-ID PASS. |
| `AUDIT_GAP_B09`: Language selector full native keyboard/OSK/clear/return | `gtk_focus001_cont011_rich_language_facet_compact` / `_tv` (640×480, 1280×800) | Same disposable data and actual XTest on Language dropdown; **NATIVE_MAPPED_PARTIAL** | Existing selected language and focused owner assertions may PASS individually; a blocked path remains **RED**. | Language keyboard search, OSK, clear and inverse origin route not fully joined; no full-ID PASS. |
| `AUDIT_GAP_B10`: Source→Group→Language→Country intersection and Clear | `gtk_focus001_cont012_four_facet_intersection_clear_compact` / `_tv` (640×480, 1280×800); F05-sort/empty cases share the helper | Twelve synthetic persisted channels across three sources, four real XTest dropdowns; **NATIVE_MAPPED**, async SQLite query and Clear | Existing 12→4→2→2→2→12 query/count, source-partition and selected-state checks must PASS. Candidate additionally asserts that each non-editor intermediate physical edge reverses with opposite D-pad key and replays to the same focus owner; a wrong owner is **RED**. | Other post-reflow reverse edges, native OSK/search in facets, rendered focus-pixel evidence, and physical Wayland remain separate. |

**Negative/exception handling:** `assert_owner` fails on an unexpected real
focus owner, including when an inverse edge strands focus at its starting
control; the separately registered
`gtk_focus001_negative_control_detects_stolen_focus` exercises an intentional
focus theft. This extension does **not** claim to add a new painted-ring
negative control. Native text editors may legitimately consume caret arrows;
the B10 inverse probe excludes Search and only asserts non-editor toolbar edges.
The B10 dropdown graph is sampled for route discovery, then the asserted path
is replayed from actual Search by native XTest; that survey is *not* counted
as successful end-to-end traversal on its own. All cases retain the current
fail-closed registration and their original names, so the **267-case sorted
registry and original 141-case subset remain unchanged**.
