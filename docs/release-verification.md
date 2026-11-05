<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Verify standalone Flatpak release bundles

These steps apply only to a release whose **tag-triggered workflow actually
produced GitHub artifact attestations**. The historical v0.3.0 release predates
that workflow change; do **not** claim it has attestations. For v0.3.1,
verify the completed tag workflow and actual downloaded bundles before
claiming or relying on provenance.

1. Obtain release files from the project's [GitHub Releases](https://github.com/Certain1989/Broadcaster/releases). For version `X.Y.Z`, download both `broadcaster-X.Y.Z-x86_64.flatpak` and `broadcaster-X.Y.Z-aarch64.flatpak` and the accompanying `SHA256SUMS` into the same directory. Use the appropriate architecture for installation.
2. Run `sha256sum -c SHA256SUMS`. This only proves agreement with the downloaded checksum file; an attacker able to replace *both* can forge consistency.
3. For a provenance-enabled tag, use a current GitHub CLI (with access to the public repository's attestation API) to verify each **actual downloaded Flatpak**, the signer workflow, and the expected source/tag ref:

```sh
VERSION=X.Y.Z
for ARCH in x86_64 aarch64; do
  FILE="broadcaster-${VERSION}-${ARCH}.flatpak"
  gh attestation verify "$FILE" \
    --repo Certain1989/Broadcaster \
    --signer-workflow github.com/Certain1989/Broadcaster/.github/workflows/release.yaml \
    --source-ref "refs/tags/v${VERSION}"
done
```

Use `--source-digest <40-hex commit>` as an additional restriction once the
released annotated tag's **peeled commit** has been independently checked;
the annotated tag object SHA is *not* the source commit. Verify that the
expected tag points to the reviewed source and that the attested workflow
matches the version intended to install. Do not trust a passing attestation
for a *different* commit/tag merely because the filename is similar.

After future owner-controlled integration, the tag-only attestation job
downloads the assembled two-architecture artifact, verifies its exact
`SHA256SUMS`, and uses the pinned GitHub `actions/attest` action. It gates
creation of the **draft** GitHub release; public publication remains a
separate owner action. PR candidates do not receive production attestations.

An attestation verifies claims made by the GitHub Actions identity and
binds specific artifact bytes to that claim. It does **not** constitute an
owner-held GPG/minisign signature, an independently signed OSTree update
remote, or a guarantee against a compromised GitHub workflow/account,
dependency, compiler or runner. There are no promised automatic updates.

GitHub's [verification reference](https://cli.github.com/manual/gh_attestation_verify)
explains the available identity and source restrictions.
