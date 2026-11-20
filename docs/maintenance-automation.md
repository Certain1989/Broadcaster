<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: CC0-1.0 -->
# Maintenance monitoring

Broadcaster now has lightweight checks for native Flatpak dependencies, upstream public APIs and RustSec advisories. These workflows are scheduled **only after the owner merges** the reviewed implementation to the default branch. No workflow merges dependency changes or plays any media.

## What the checks do

- **Native sources:** the weekly `.github/workflows/native-dependencies.yml` runs the digest-pinned Flatpak External Data Checker against the four existing tagged sources in `build-aux/io.github.Certain1989.Broadcaster.json`: mpv, libass, libplacebo and blueprint-compiler. The edit-only checker is followed by at most one open **draft PR** (no force-push or auto-merge). Review every proposed tag, pinned commit, build and security change before integration. Its separate LuaJIT check compares the pinned commit with the upstream `v2.1` branch and opens a manual-review issue on differences; different SHAs do **not** establish ancestry or suitability. LuaJIT is never automatically updated.
- **Provider APIs:** `.github/workflows/api-compatibility.yml` probes MediathekViewWeb channels and one bounded search, plus Zapp's channel list and selected programme information daily. The script validates essential JSON response contracts. It does not fetch streams, guarantee content correctness, or use credentials or private playlists. Contract failures raise issues immediately; transient network/5xx/rate-limit failures are retried and require a previous recent failing *scheduled* run before a new issue is opened. Individual workflow runs still fail on errors.
- **RustSec:** the existing weekly RustSec job now has a separate incident-reporting job, which marks failure via a deduplicated issue and closes it on healthy recovery.
- **Issue lifecycle:** `tools/maintenance/issue_sync.py` uses unique HTML markers to update existing open issues rather than duplicate them, and closes recovered incidents. It includes only a bounded generic diagnostic and the public run link. GitHub Actions needs `issues: write`; draft PRs also need `contents: write` and `pull-requests: write`. Actions-created PRs must be allowed in repository settings. The default `GITHUB_TOKEN` generally does **not** trigger ordinary CI on its newly created PRs: manually trigger required validation on the exact candidate before owner-approved integration.

## Validation and limitations

```sh
python3 -m compileall -q tools/maintenance
python3 -m unittest discover -s tools/maintenance -p 'test_*.py' -v
```

Offline tests cover valid and invalid provider responses, transient retry bounds, issue deduplication and recovery, and LuaJIT pin drift. Actual provider responses, GitHub issue/PR permissions and the digest-pinned checker runtime need live validation after integration. GitHub may disable scheduled workflows in inactive public repositories; this project deliberately does not add an external watchdog. A CI workflow run is not a substitute for testing the pinned Flatpak builds on the intended architectures.
