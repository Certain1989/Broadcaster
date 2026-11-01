<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Signed Flatpak update remotes — nonproduction U1 prototype

This directory documents a **disposable local proof**, not a released update
service. Existing GitHub Releases provide standalone `.flatpak` bundles; they
do not automatically supply a persistent update origin. No background updater,
extra application sandbox privilege, stable signing key or public upload is
introduced here.

## Architecture and trust boundary

A normal update requires a long-lived, HTTP-served archive-z2 OSTree repository
with signed app commits and a signed summary. Clients pin the public signing
key through an onboarding `.flatpakrepo`/`.flatpakref`, and Flatpak performs
updates outside the application. A GitHub Release download notification is
**not** equivalent to this. Stable and Devel have different Flatpak IDs and
must never share a ref accidentally.

The U1 proof takes **two real Devel bundle artifacts** (`x86_64`, `aarch64`)
and an explicitly disposable GPG secret key. It imports both as signed
`app/io.github.Certain1989.Broadcaster.Devel/<arch>/master` refs with
`--no-update-summary`, checks both identities, and only then writes one
signed summary. It produces a local `file://` test remote and reports repository
bytes, object-file count and input bundle sizes. It does not contact a hosting
service, alter the application's executable or provision a production remote.
The signing key stays outside the staged output.

For a throwaway Linux host with `flatpak`, `ostree`, `gpg`, and two *verified*
architecture-specific CI bundles:

```sh
bash build-aux/u1/make-test-key.sh /tmp/broadcaster-u1-key
bash build-aux/u1/stage-signed-remote.sh \
  /tmp/broadcaster-u1-v1 - \
  dist/broadcaster-u1-x86_64.flatpak dist/broadcaster-u1-aarch64.flatpak \
  /tmp/broadcaster-u1-key
flatpak remote-add --user --from broadcaster-u1-test \
  /tmp/broadcaster-u1-v1/u1-test.flatpakrepo
flatpak remote-ls --user --arch='*' broadcaster-u1-test
```

A subsequent revision uses the *same test public key* and the preceding output
directory as the second argument. Staging refuses existing destinations,
missing/invalid bundles, unexpected refs, and changed test keys; failures
leave the prior output untouched. The output directory and private key must
never be published, committed or copied to a public Pages directory. The
`u1-public-key.gpg` file is public material only.

The U1 PR workflow constructs **two distinct packaged-content revisions** of
Broadcaster Devel independently on native x86-64 and ARM64 runners (the second
adds a named U1 test-only `/app` marker, without changing the application
binary). An isolated signed remote contains both architecture refs at each
revision. Clients on **both native architectures** install the first revision
and use ordinary `flatpak update` to reach the second, checking the changed
installed marker. The update is served from a loopback static HTTP server, not public hosting.
Replayed/mismatched architecture refs and unplanned test-key rotation are rejected
before signing a new summary. The **exact-HEAD 2026-11-01 proof** (commit
`ed203427eeee3d9fb178ae34c3a3450e7fcbbe74`, U1 run 41345527157)
passed on native x86-64 and AArch64: a fresh system-scope remote accepted a
summary re-signed with the **trusted** disposable key and listed the Devel ref,
but rejected installation of commits signed only by an **untrusted** key for a
signature-related reason. No app was left installed by the rejected attempt.
This isolates **commit trust** from successful remote onboarding; it does not
independently prove summary-signature enforcement. A prior experiment changed
only a signed `summary` byte while leaving valid signed commits and Flatpak
still installed them. Do not describe that experiment as a successful summary
signature rejection or a commit-signature bypass. OSTree's separate
`gpg-verify-summary` policy still needs independent distribution/security
review before production onboarding. This proves a
normal signed **content update** on both architectures and local HTTP transport
when the exact-head proof succeeds, not a distinct upstream release-version
upgrade or GitHub Pages delivery; both remain acceptance requirements.

## Production path — separate owner decision required

A future release pipeline must bind signing to a separately accepted source
commit and release tag, verify both architecture builds and their SHA-256
checksums, assemble objects/deltas in an unpublished staging area, validate
each architecture and trusted key, and **atomically switch the served summary
only after the entire repository is reachable**. A partial upload must leave
the old signed summary and every object reachable by it intact. Preserve old
objects for rollback; do not prune old commits before the retention decision.
Document historical key transitions and client public-key updates. A lost or
compromised private key requires an explicit owner-led recovery/rotation plan,
not an unattended CI reset. Release keys must live in restricted owner-controlled
custody, never in Git, artifact uploads, logs, or a public Pages build.

## Hosting feasibility remains unresolved

GitHub Pages documents a **1 GiB published-site limit**, **100 GiB/month soft
bandwidth limit**, and 10-minute deployment timeout. An archive-z2 repository
causes many small HTTP requests; static deltas trade additional storage for
fewer requests. For two architectures, measure staged repo size, file count,
from-empty and incremental delta bytes, HTTP keep-alive/cache behavior,
request/transfer totals, deployment churn and retained historical commits.
The workflow's METRICS.txt gives a first unhosted baseline only; the
loopback-only Python server logs actual HTTP response-body bytes sent
(including repeated requests, probes and retries) alongside GET counts.
At exact commit `ed203427eeee3d9fb178ae34c3a3450e7fcbbe74`, the native
x86-64 test served **33,745,755 bytes** and the AArch64 test **39,301,134
bytes**; each run included several installations, update probes and recovery
tests, so these are **not** one-user update sizes. The dual-arch repository
occupied about **20.38 MB / 142 files** at v1 and **42.05 MB / 855 files**
at v2. These measurements exclude request/response headers, TLS, CDN caches,
real WAN overhead, delta compression and historical-retention growth. No
actual GitHub Pages deployment or cache test was performed; a production
capacity decision **cannot** be inferred from this loopback baseline.
GitHub Release attachments alone are not an immutable path-preserving OSTree
object service. If Pages exceeds limits or cannot serve valid update semantics,
choose a suitable separately approved static host or repository service;
never silently replace updates with notifications.

## Migration and negative cases requiring owner-device/integration evidence

A standalone `.flatpak` installation may be under `--user` or `--system`.
Check `flatpak list --app --columns=application,installation,branch,origin`
and `flatpak info` first; explicitly opt into a matching-scope stable remote
while preserving the same stable application ID and user data directory.
Do not silently replace a system installation with a duplicate user install.
Devel test refs intentionally do not replace an existing stable install.
Reinstall/uninstall experiments must use disposable installations/data; confirm
persistent app data survives. On both native architectures, the
exact-HEAD U1 proof exercised **user- and system-scope** Devel v1→v2
changed-content updates, then installed a disposable standalone **Devel**
bundle, explicitly reinstalled it from a signed v1 remote, verified the
selected origin and updated normally to signed v2. A synthetic per-user
data marker survived both transitions and the independent system installation
was not altered by user-scope migration. This is **not** proof of migrating an
existing stable installation, SQLite database, GSettings, portal grants or a
real user profile. It also points the user-scope remote at an
unreachable loopback port before the update and requires the installed commit
to remain unchanged. Flatpak may return an error **or** a cached no-op in this
offline case; neither is misreported as a signature failure. Test
offline/partial publication, wrong GPG key,
tampered and missing signatures, absent ARM64 ref, mixed refs, rollback, key
rotation, interrupted update, absent runtime, and two **actual** application
versions on both architectures before claiming the full update objective.

See [Flatpak repository hosting](https://docs.flatpak.org/en/latest/hosting-a-repository.html),
[Flatpak bundle import](https://docs.flatpak.org/en/latest/single-file-bundles.html),
and [GitHub Pages limits](https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits).
