# SPDX-FileCopyrightText: d-k-bo <d-k-bo@mailbox.org>
# SPDX-FileCopyrightText: 2026 Televido contributors
# SPDX-FileContributor: Claude (Anthropic)
# SPDX-License-Identifier: CC0-1.0

desktop_file = i18n.merge_file(
  input: BASE_APP_ID + '.desktop.in',
  output: BASE_APP_ID + '.desktop',
  type: 'desktop',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'applications',
)

desktop_utils = find_program('desktop-file-validate', required: false)
if desktop_utils.found()
  test('Validate desktop file', desktop_utils, args: [desktop_file])
endif

appstream_file = i18n.merge_file(
  input: BASE_APP_ID + '.metainfo.xml.in',
  output: BASE_APP_ID + '.metainfo.xml',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'metainfo',
)

# The Rust build script embeds translated AppStream metadata in OUT_DIR.
appstreamcli = find_program('appstreamcli', required: false, disabler: true)
if appstreamcli.found()
  test(
    'Validate appstream file',
    appstreamcli,
    args: ['validate', '--no-net', '--explain', appstream_file],
  )
endif

install_data(
  BASE_APP_ID + '.gschema.xml',
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas',
)

gnome.compile_schemas(
  depend_files: files(
    BASE_APP_ID + '.gschema.xml',
  ),
)

compile_schemas = find_program('glib-compile-schemas', required: false, disabler: true)
if compile_schemas.found()
  test(
    'Validate schema file',
    compile_schemas,
    args: ['--strict', '--dry-run', meson.current_source_dir()],
  )
endif

# Remote-friendly mpv shortcuts, private to Broadcaster's managed player.
install_data(
  'broadcaster-mpv-input.conf',
  'broadcaster-mpv-remote.lua',
  install_dir: get_option('datadir') / meson.project_name(),
)

subdir('icons')