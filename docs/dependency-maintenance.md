<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Dependency and release security maintenance

This project does not promise a regular update or support schedule.
Dependency changes should be proposed in small, reviewable PRs with
compatible runtime behavior and both architecture-specific Flatpak tests
where relevant.

## Rust, lockfiles and advisories

Dependabot is configured for weekly Cargo and monthly GitHub Actions
proposals; these PRs are **not** automatically merged. The release workflow
already runs `cargo audit --deny warnings`. A separate read-only
[weekly RustSec workflow](../.github/workflows/rustsec-weekly.yml) also runs
the same advisory policy on the current default-branch `Cargo.lock`, with
a manual dispatch option. Schedules only run after integration into
`main`, may be delayed/disabled by GitHub during inactivity, and rely on
the maintainer watching failed Actions runs; **no alert-delivery SLA**.

The exact `quick-xml`, `flate2` and `unicode-casefold` constraints
need conscious compatibility review if updated (XMLTV handling, gzip and
Unicode identity behavior are relevant test surfaces). Exact pins do not
categorically prevent Dependabot from proposing manifest changes. The
`rusqlite` `bundled` feature obtains SQLite through the locked crate;
review the actual upstream crate and bundled SQLite revision when updating,
rather than assuming the system SQLite version applies.

## Flatpak native component pins

The manifest pins explicit Git commits for mpv, libass, libplacebo,
blueprint-compiler and LuaJIT. The version-tagged sources include
`x-checker-data` for the upstream
[Flatpak External Data Checker](https://github.com/flathub-infra/flatpak-external-data-checker).
A maintainer may run it manually against
`build-aux/io.github.Certain1989.Broadcaster.json` to *inspect* newer
tags. **No checker bot, scheduled auto-update PR, automatic merge, signing
key or new dependency permission is installed.** Checker's suggestions
are not trusted code; separately inspect upstream release notes, tags,
commit provenance, transitive changes and compatibility.

LuaJIT is pinned to a commit rather than a tagged release. Check its
upstream history and candidate commit explicitly; no tag-based checker
metadata is added for it. An accepted update must retain a fixed reviewed
commit, update the tag/commit pair consistently where applicable, rebuild
both architectures, test HLS, subtitles and mpv invocation and use the
appropriate native GTK tests. Keep source/output provenance and tests
attached to the candidate PR; never upgrade merely because a newer tag
exists.

## Build-time networking and remaining supply-chain risk

The Flatpak manifest's `build-options.build-args` allows network access
during the build; this is **distinct** from the installed Flatpak's runtime
`finish-args`. Rust dependency resolution and external source retrieval
currently depend on the builder's environment. The existing
`build-aux/dist-vendor.sh` produces a Cargo vendor directory for Meson
distribution archives; it is **not integrated as a proven offline Flatpak
vendor stage**. Do not remove `--share=network` on the assumption that
the build works offline. A future change needs an actual offline,
cache-independent x86-64/AArch64 build proof, verified locked source inputs
and a fail-closed fixture. This R1 change makes **no** Flatpak network
sandbox policy change.

## Distribution, review and monitoring

Release checksums on their own do not prove authenticity. See
[release verification](release-verification.md) for tag-only provenance
and [SECURITY.md](../SECURITY.md) for private reporting. Future signing
keys, a hosted OSTree repository and automatic updates are intentionally
outside this maintenance change. A failed scheduled advisory check is a
signal for manual triage, not automatic removal or publication of software.
