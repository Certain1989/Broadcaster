#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""One-shot, behavior-preserving source hygiene on an isolated maintenance branch.

The script and its workflow remove themselves from the final source revision.
It does not touch persisted backup contracts or other feature branches.
"""
from pathlib import Path
import re

root = Path.cwd()
assert (root / "Cargo.toml").is_file()
paths = []
for section in ("src", "tests", "data", "build-aux", ".github", "po"):
    paths.extend(p for p in (root / section).rglob("*") if p.is_file() and
                 p.suffix.lower() in (".rs",".blp",".sh",".py",".yaml",".yml",".xml",".in",".css",".md",".toml",".sql",".lua",".txt"))
paths.extend(root / p for p in ("build.rs","Cargo.toml","meson.build","justfile") if (root/p).is_file())

exact = {
    "r07_native_focus_tests":"native_focus_navigation_tests",
    "r07_painted_focus_tests":"painted_focus_tests",
    "r07_mediathek_batch_tests":"native_mediathek_navigation_tests",
    "r07_facets_tests":"native_channel_filter_tests",
    "c1_native_service_tests":"native_source_manager_tests",
    "c2_native_tests":"native_epg_manager_tests",
    "c1_mapped_native_diagnostic":"native_navigation_test_support",
    "epg_capacity_r08_d33_support_floor_streamed":"epg_capacity_supported_floor_streamed",
    "r08-floor-gzip":"capacity-floor-gzip",
    "e2_revision_tests":"epg_cache_revision_tests",
    "r07_native_":"native_",
    "c1_native_manager_":"native_source_manager_",
    "c2_native_":"native_epg_manager_",
    "c4_native_":"native_channel_",
    "r07-":"focus-navigation-",
    "R07_FOCUS_LOCAL_":"BROADCASTER_NATIVE_TEST_",
    "R07_CAPTURE_X11":"NATIVE_FOCUS_CAPTURE_X11",
    "R07X11":"NativeFocusX11",
    "R07_":"NATIVE_FOCUS_",
    "m1_019_":"quality_guide_","M1_019_":"QUALITY_GUIDE_",
    "m1_016_":"guide_revision_","M1_016_":"GUIDE_REVISION_",
    "R07_G07_G08":"NATIVE_FOCUS_LIVE_SEARCH_PLAY",
    "R07_D01_D02":"NATIVE_FOCUS_LIVE_FULL_GRAPH",
    "AS12":"SOURCE_REORDER","AS03":"SOURCE_GRID","AS16":"SOURCE_MODAL_HOME",
    "AG08":"GUIDE_RETURN_FOCUS","G01":"PAINTED_FOCUS","G03":"PAINTED_CARDS",
    "G04":"FAVOURITES_PAINT","E02":"LIVE_PLAY_IDENTITY",
    "E04":"MEDIATHEK_ROWS","E05":"MEDIATHEK_ACTIONS",
    "D07":"PAGE_SWITCHER","R07":"NATIVE_FOCUS",
    "C1_EXPECTED_NEGATIVE_ORACLE":"SOURCE_MANAGER_EXPECTED_NEGATIVE_ORACLE",
    "C2_EXPECTED_NEGATIVE_ORACLE":"EPG_MANAGER_EXPECTED_NEGATIVE_ORACLE",
    "C4_EXPECTED_NEGATIVE_ORACLE":"GUIDE_MAPPING_EXPECTED_NEGATIVE_ORACLE",
}
lexemes = [
    ("r07","focus_navigation"),("c1","source_manager"),("c2","epg_manager"),
    ("c4","guide_mapping"),("m1","guide_matching"),("r02","matching_guard"),
    ("d29","sqlite_temp_capacity"),("d35","guide_picker"),("d38","facet_popup"),
    ("r08","import_capacity"),("e2","epg_cache"),("d6","storage_migration"),
    ("p1","mpv_http"),("g09","focus_graph"),("focus-001","focus_graph"),
]
upper = {
    "G09":"FOCUS_GRAPH","D28":"IMPORT_DURABILITY","D29":"SQLITE_TEMP_CAPACITY",
    "D33":"SUPPORTED_FLOOR","D35":"GUIDE_PICKER","D38":"FACET_POPUP",
    "R08":"IMPORT_CAPACITY","E2":"EPG_CACHE","R02":"MATCHING_GUARD",
    "M1":"GUIDE_MATCHING","P1":"MPV_HTTP","C1":"SOURCE_MANAGER",
    "C2":"EPG_MANAGER","C4":"GUIDE_MAPPING",
    "B07":"FILTER_POPUP","B08":"FILTER_POPUP","B09":"FILTER_POPUP",
    "B12":"GUIDE_PICKER","B13":"GUIDE_PICKER",
}
labels = {
    "R01-F01":"SOURCE_DELETE_ACTION","R01-F02":"SOURCE_DISABLE_REOPEN",
    "AS04":"SOURCE_DISABLE_TOGGLE","AS05":"SOURCE_REENABLE_TOGGLE",
    "AS06":"SOURCE_OFFLINE_REFRESH","AS07":"SOURCE_REFRESH_PERSISTENCE",
    "AS08":"SOURCE_DEFAULTS_DIALOG","AS09":"SOURCE_RENAME_VALIDATION",
    "AS11":"SOURCE_KEYBOARD","AS17":"SOURCE_REFRESH_MODAL",
    "AS18":"SOURCE_REFRESH_CANCEL",
    "AG02":"GUIDE_ENABLE_REOPEN","AG06":"GUIDE_ADD_URL",
    "AG07":"GUIDE_LOCAL_CHOOSER","AG09":"GUIDE_LIST_BOUNDARY",
    "G03":"PAINTED_CARD_FOCUS","G05":"SOURCE_MANAGER_PAINTED_FOCUS",
    "G06":"GUIDE_PICKER_PAINTED_FOCUS","G07":"LIVE_SEARCH_DOWN",
    "G08":"LIVE_PLAY_RIGHT",
    "B01":"CHANNEL_SEARCH_TRAVERSAL","B02":"ZERO_GUIDE_SUGGESTIONS",
    "B03":"EXACT_GUIDE_SELECTION","B04":"MULTIPLE_GUIDE_SUGGESTIONS",
    "B05":"GUIDE_PRECEDENCE","B06":"PLAYLIST_GUIDE_SELECTION",
    "E08":"CHANNEL_CARD_JOURNEY","R1":"COMPACT_VIEWPORT",
    "C06":"KEYBOARD_CONFIRMATION","K2":"ON_SCREEN_KEYBOARD",
    "D27":"XMLTV_CONTROL_OMISSION",
}
changed = []
for path in paths:
    try:
        original = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, ValueError):
        continue
    source = original
    for old, new in sorted(exact.items(), key=lambda pair:-len(pair[0])):
        source = source.replace(old, new)
    for old, new in lexemes:
        source = re.sub(r"(?<![a-zA-Z0-9])" + re.escape(old) + r"(?=[_\-]|\b)",new,source)
    for old, new in upper.items():
        source = re.sub(r"(?<![A-Za-z0-9])" + re.escape(old) + r"(?=[_\-]|\b)",new,source)
    if path.suffix.lower() in (".rs", ".blp", ".sh") or path.relative_to(root).as_posix() == ".github/workflows/ci.yaml":
        for old, new in sorted(labels.items(),key=lambda pair:-len(pair[0])):
            source = re.sub(r"(?<![A-Za-z0-9])" + re.escape(old) + r"(?![0-9A-Za-z])",new,source)
    if source != original:
        path.write_text(source, encoding="utf-8")
        changed.append(path.relative_to(root).as_posix())

rename = {
    "src/window/r07_native_focus_tests.rs":"src/window/native_focus_navigation_tests.rs",
    "src/window/r07_painted_focus_tests.rs":"src/window/painted_focus_tests.rs",
    "src/window/r07_mediathek_batch_tests.rs":"src/window/native_mediathek_navigation_tests.rs",
    "src/streams/management/r07_facets_tests.rs":"src/streams/management/native_channel_filter_tests.rs",
    "src/streams/management/c1_native_service_tests.rs":"src/streams/management/native_source_manager_tests.rs",
    "src/epg/desktop/c2_native_tests.rs":"src/epg/desktop/native_epg_manager_tests.rs",
    "tests/player/p1_mpv_http_probe.py":"tests/player/mpv_http_probe.py",
}
for old, new in rename.items():
    assert (root/old).is_file() and not (root/new).exists(), (old,new)
    (root/old).rename(root/new)

# Real Unicode C1 controls are not campaign names; preserve the public parser
# identifiers and literal XMLTV control semantics.
for rel in ("src/epg/xmltv.rs", "src/epg/ingest.rs",
            "src/epg/desktop.rs", "src/epg/capacity_bench.rs"):
    path = root/rel
    source = path.read_text()
    source = source.replace("omitted_source_manager_programmes","omitted_c1_programmes")
    source = source.replace("allow_source_manager_omission","allow_c1_omission")
    source = source.replace("parse_with_source_manager_omission","parse_with_c1_omission")
    path.write_text(source)

# \p1 is a protocol-level ASS overlay drawing command, not a player campaign.
for rel in ("data/broadcaster-mpv-remote.lua", "tests/mpv-remote.lua"):
    path=root/rel
    path.write_text(path.read_text().replace(r"\mpv_http",r"\p1"))
# Synthetic persisted row labels named "p1" are test data, not project stages.
for rel in ("src/streams/storage_tests.rs", "src/epg/matching_tests.rs",
            "src/epg/bindings_tests.rs"):
    path=root/rel
    path.write_text(path.read_text().replace('"mpv_http"','"p1"'))

path = root/"tests/player/mpv_http_probe.py"
source=path.read_text().replace("Broadcaster-MPV_HTTP-Probe/1","Broadcaster-MpvHttpProbe/1")
source=source.replace("Broadcaster-MPV_HTTP-Probe","Broadcaster-MpvHttpProbe")
path.write_text(source)
path = root/"src/epg/desktop/native_epg_manager_tests.rs"
source=path.read_text().replace("ag07-test.xml","selected-local-guide.xml")
source=source.replace("ag07.test","selected.local-guide.test")
path.write_text(source)
path = root/"tests/ci-fast-smoke.sh"
path.write_text(path.read_text().replace("full NATIVE_FOCUS CI","full native-focus CI"))

for rel, anchor in (
    (".github/workflows/ci.yaml", "      - name: Check formatting\n"),
    (".github/workflows/ci-fast.yaml", "      - name: Configure\n"),
):
    path = root/rel
    source=path.read_text()
    assert source.count(anchor)>=1,rel
    path.write_text(source.replace(anchor,"      - name: Validate campaign-neutral executable names\n        run: python3 tests/check_campaign_neutral_names.py\n"+anchor,1))
print("Changed",len(changed),"source files and renamed",len(rename),"test and probe files")
