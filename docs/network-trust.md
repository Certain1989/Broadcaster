<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Media and playlist network trust

Broadcaster treats provider-supplied Live/Mediathek video, subtitle, and
website/download targets as untrusted network references. Only well-formed
HTTP(S) URLs with a host and without embedded user information are admitted
before the managed mpv or externally selected player/downloader is launched.
Imported IPTV stream URLs retain the playlist parser's HTTP(S) admission rule.
Raw paths, local files, RTMP, EDL, lavfi and ytdl-style primary inputs are not
admitted from these untrusted origins. Deliberately configured global mpv
arguments remain user-controlled and are not restricted by these checks.

The initial media URL filter does **not** sandbox every protocol or remote
resource a player may encounter while processing HLS/DASH manifests and their
segments. Broadcaster does not automatically add a demuxer protocol whitelist
or `--ytdl=no`, because nested streams and user options need separate
compatibility testing. Treat external player and downloader configuration as
locally trusted applications.

## Playlist LAN access

Each remote playlist starts with **private LAN access disabled**. On the URL
entry form, or the existing source's *Edit Source Defaults* form, explicitly
select **Allow this playlist to connect to private LAN addresses** to permit
RFC1918/ULA destinations. Replacing a playlist URL requires a new choice.
Stored consent is specific to that source and exact URL and is invalidated
when the database file is replaced during snapshot recovery. Existing sources
and restored backups are not automatically consented.

Revoking access writes a separate, atomic and fsynced denial ledger in the
private application data directory before removing the previous GSettings
grant. This prevents a stale grant from being reactivated after a normal
restart when GSettings rejects the removal. The ledger contains hashed
source/database identities, not playlist URLs, and is not part of the SQLite
backup. A new explicit grant replaces the denial only after its setting has
been saved. If the ledger is corrupt or unreadable, playlist LAN access is
denied; if revocation cannot be persisted, the application reports an error
and denies access for the current process. **When all durable writes fail, an
old grant may reappear after restarting**; retry the revocation while storage
is writable before exiting. This guarantee does not extend to deliberate
out-of-band deletion or rollback of both local permission stores.

A public-looking DNS name resolving to private LAN addresses also requires
this consent before the first request. DNS answers are checked for mixed
classes, and every HTTP(S) hop uses the checked address set for its actual
connection. Loopback, link-local, CGNAT, documentation, reserved, and other
ambiguous destinations remain denied, even with playlist LAN consent.
Public-to-LAN redirects are refused. Consented LAN-to-LAN redirects must stay
on the same origin; a LAN-to-public redirect can proceed under normal public
checks, but it cannot subsequently redirect back into the LAN. HTTPS-to-HTTP
downgrades remain forbidden.

Programme-guide downloads preserve their separate explicit-LAN scope and
redirect rules; logos remain public-only. These network fetchers disable
system/environment proxies to keep each checked DNS answer tied to the dialled
socket. This means a required corporate proxy or split-proxy network may not
work with playlist/guide/logo fetching, and traffic travels directly rather
than through the system's configured proxy. This behaviour is intentional,
not a guarantee of anonymity or a claim that media players use the same path.
