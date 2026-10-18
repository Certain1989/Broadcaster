<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# My Channels — manual TV and Flatpak regression checklist

For a real-device acceptance test, use a **development** build on an N100 or
comparable Linux TV system with Fedora KDE Plasma Bigscreen and Wayland.
Use only non-sensitive, synthetic fixtures or openly distributable test streams.
Record the exact build/commit, hardware, OS, display scale, runtime and
test outcome. Do not copy credential-bearing playlist addresses into reports.

- [ ] Confirm the three tabs (Live, Mediathek, My Channels) switch with
  the remote, mouse and keyboard; Alt+1/2/3; and the main menu.
- [ ] Add a fabricated local M3U file using the file picker. Restart the
  Flatpak and refresh it. If the document portal grant has expired, verify
  the reselect/reauthorize flow preserves the old catalogue on failure.
- [ ] Add and refresh a fabricated local HTTP(S) playlist. Check network
  offline/time-out/invalid-format behavior and retention of prior channels.
- [ ] Test 100, 1,000 and 10,000 synthetic channels: search, source/group/
  language/country/(conditional region)/unknown facets, sorting, selection,
  list scroll and editing. Report observed latency and memory, not estimates.
- [ ] From the first list result press Up. Confirm visible focus moves to Add
  Playlist and remains visible while navigating search, dropdowns, actions,
  and back to results. Test Left/Escape/Back and first Enter (expand, not play).
- [ ] Check very long names, multiple-country labels, German text and 720p/
  1080p display widths for clipping, focus loss or oversized controls.
- [ ] Test valid logo, missing logo, unsupported format and offline behavior;
  verify a failed logo never blocks playback.
- [ ] Start playback and return: correct tab, source, selected channel and
  focus restored. Repeat after a search/filter, audio/subtitle track changes,
  and mpv overlay open/close. Verify old Live and Mediathek behavior.
- [ ] Confirm source rename/defaults/enable/disable/reorder, channel override
  survival after refresh, deletion confirmation and cancellation. Exercise
  suspicious 20->4 shrink (confirm/decline), and 20->5 threshold.
- [ ] Examine Flatpak permissions: network and portal use are expected;
  blanket home filesystem access is not. Never paste private URL/query tokens,
  local paths or personal playlist content into reports.

Automated GTK/Xvfb tests do not demonstrate physical Wayland compositor
behavior, audio/video playback or local portal persistence. Mark every
device-only check **NOT PERFORMED** until someone runs it.
