<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Contributing

Broadcaster is a small hobby application. Issues and patches may be considered, but review, merge or maintenance is not guaranteed.

Before proposing a change, check that it preserves desktop, TV-remote and Flatpak usability. Keep patches narrow, explain user-visible behaviour, and avoid unrelated dependency or architectural churn. Please do not include credentials, private stream URLs, local logs or machine-specific paths in issues or commits.

Useful checks when your system provides the GTK/Wayland build dependencies:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
lua tests/mpv-remote.lua
meson setup _build -Dprofile=development
meson compile -C _build
meson test -C _build --print-errorlogs
reuse lint
```

Release candidates additionally need both architecture-specific Flatpak builds and real remote-control testing where relevant. CI is a check, not a guarantee of hardware behaviour.

Respect existing copyright headers, third-party attribution and SPDX declarations; contributions must be compatible with the applicable GPL-3.0-or-later and third-party licences. See `REUSE.toml` and [artwork provenance](docs/artwork-and-attribution.md). Do not replace inherited copyright notices with new project branding.

The main branch tracks accepted releases; propose ongoing changes through a reviewable branch. Significant project code and review work has been LLM-assisted; human reviewers and contributors remain responsible for checking their submissions.
