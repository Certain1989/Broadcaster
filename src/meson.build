# SPDX-FileCopyrightText: d-k-bo <d-k-bo@mailbox.org>
# SPDX-FileCopyrightText: 2026 Televido contributors
# SPDX-FileContributor: Claude (Anthropic)
# SPDX-License-Identifier: CC0-1.0

configure_file(
  input: 'config.rs.in',
  output: 'config.rs',
  configuration: CONFIG
)

fs = import('fs')

cargo = find_program('cargo')
cargo_opt = [ '--manifest-path', meson.project_source_root() / 'Cargo.toml' ]
cargo_env = { 'BROADCASTER_CONFIG_RS': meson.current_build_dir() / 'config.rs' }

if fs.exists('/.flatpak-info')
  cargo_env += { 'CARGO_HOME': meson.project_build_root() / 'cargo-home' }
endif

if get_option('profile') == 'release'
  cargo_opt += [ '--release' ]
  rust_target = 'release'
else
  rust_target = 'debug'
endif

# Meson executes command arrays directly, not through a shell. Use a small
# POSIX shell wrapper only where both Cargo and the final copy are required.
cargo_build_command = '"$1" build --manifest-path "$2"'
if get_option('profile') == 'release'
  cargo_build_command += ' --release'
endif
cargo_build_command += ' && cp "$3" "$4"'

custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: meson.project_name(),
  console: true,
  install: true,
  install_dir: get_option('bindir'),
  env: cargo_env,
  command: [
    find_program('sh'), '-eu', '-c', cargo_build_command,
    'sh', cargo, meson.project_source_root() / 'Cargo.toml',
    meson.project_source_root() / 'target' / rust_target / meson.project_name(), '@OUTPUT@',
  ],
)

test(
  'Cargo tests',
  cargo,
  args: ['test', cargo_opt],
  env: cargo_env,
  timeout: 1800,
)
# Exercise real mapped GTK widgets under Xvfb, never an unconditional
# headless cargo test. GitHub's native Meson CI already runs this suite.
if find_program('xvfb-run', required: false).found()
  test(
    'K1 mapped GTK keyboard',
    find_program('sh'),
    args: [meson.project_source_root() / 'tests/osk/run-mapped.sh'],
    env: cargo_env,
    timeout: 900,
  )
endif
