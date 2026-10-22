# SPDX-FileCopyrightText: d-k-bo <d-k-bo@mailbox.org>
# SPDX-FileCopyrightText: 2026 Televido contributors
# SPDX-FileContributor: Claude (Anthropic)
# SPDX-License-Identifier: CC0-1.0

project(
  'broadcaster',
  'rust',
  version: '0.2.1',
  meson_version: '>= 0.61.0',
)

i18n = import('i18n')
gnome = import('gnome')

BASE_APP_ID = 'io.github.Certain1989.Broadcaster'

if get_option('profile') == 'development'
  PROFILE = 'Devel'
  vcs_tag = run_command('git', 'rev-parse', '--short', 'HEAD', check: false).stdout().strip()
  if vcs_tag == ''
    version_suffix = '-devel'
  else
    version_suffix = '-@0@'.format(vcs_tag)
  endif
  APP_ID = '@0@.@1@'.format(BASE_APP_ID, PROFILE)
else
  PROFILE = 'Release'
  version_suffix = ''
  APP_ID = BASE_APP_ID
endif

meson.add_dist_script(
  'build-aux/dist-vendor.sh',
  meson.project_build_root() / 'meson-dist' / meson.project_name()
  + '-'
  + meson.project_version(),
  meson.project_source_root(),
)

CONFIG = {
  'PROJECT_NAME': meson.project_name(),
  'VERSION': meson.project_version() + version_suffix,
  'PROFILE': PROFILE,
  'APP_ID': APP_ID,
  'BASE_APP_ID': BASE_APP_ID,
  'APP_NAME': 'Broadcaster',
  'AUTHOR': 'Broadcaster contributors',
  'PROJECT_URL': 'https://github.com/Certain1989/Broadcaster',
  'ISSUE_URL': 'https://github.com/Certain1989/Broadcaster/issues',
  'LOCALEDIR': get_option('prefix') / get_option('localedir'),
  'PKGDATADIR': get_option('prefix') / get_option('datadir') / meson.project_name(),
}

subdir('data')
subdir('po')
subdir('src')

gnome.post_install(
  glib_compile_schemas: true,
  gtk_update_icon_cache: true,
  update_desktop_database: true,
)