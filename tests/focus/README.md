<!-- SPDX-License-Identifier: CC0-1.0 -->
# FOCUS-001 mapped GTK regression suite (independent, test-only)

From the repository root run `bash tests/focus/run-mapped.sh`. Requires Cargo,
`xvfb-run`, `glib-compile-schemas`, GTK4/libadwaita and normal Broadcaster build
dependencies. The script compiles an isolated GSettings schema and runs each
ignored mapped-window GTK test in a **separate process**, with a 120-second bound.
A failing assertion exits nonzero and blocks the pull-request CI job. RED tests
must be fixed by a *different* authorized application worker, never skipped.

These probes operate on the real mapped `TvWindow` and call its production
window-level capture handler. They **do not synthesize hardware XTest/GDK
key events**. This distinction is important: native propagation, physical
remote keysyms, Wayland compositor visuals and screenshot contrast require
additional independent validation. The oracle checks window focus owner,
correct descendant ancestry, mapped/visible/sensitive state, and reports
GTK `has_visible_focus()` and state flags without treating either as universal
proof of painted CSS focus. The deliberate focus-theft negative control
shows that a wrong focus owner is caught.

`focus-coverage.tsv` records what was exercised and what remains untested.
No live playlist, XMLTV corpus, network stream or private environment values
are used by the tests.
