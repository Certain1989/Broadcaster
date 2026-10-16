# SPDX-FileCopyrightText: d-k-bo <d-k-bo@mailbox.org>
# SPDX-License-Identifier: CC0-1.0

# The approved night-friendly artwork is raster; do not install the old Televido SVG.
icon_dir = 'hicolor' / '128x128' / 'apps'
install_data(
  icon_dir / ('@0@.png').format(BASE_APP_ID),
  install_dir: get_option('datadir') / 'icons' / icon_dir
)

symbolic_dir = 'hicolor' / 'symbolic' / 'apps'
install_data(
  symbolic_dir / ('@0@-symbolic.svg').format(BASE_APP_ID),
  install_dir: get_option('datadir') / 'icons' / symbolic_dir
)