<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Released v0.3.1 SQLite compatibility fixtures

These are **real SQLite database files**, not SQL text replayed under the new
engine. They were generated and exported using Broadcaster's **released**
v0.3.1 Rust source and `rusqlite 0.40.1` / bundled `libsqlite3-sys 0.38.1`
(**SQLite 3.53.2**) before the I1 dependency refresh. They contain **only
fabricated rows and `example.invalid` URLs**, no personal playlists, keys,
media streams or user paths.

## Exact generation provenance

- Source: PUBLIC `Certain1989/Broadcaster` commit
  `0fcdcec9a17c61e053ce0c5db1a2487b8baff245`, tree
  `2f5c88166cab1cbb947c736f3724a01a11e53186`, v0.3.1 release.
- Released Cargo.lock blob: `70dcd9a729c7d011e933c59f276989addd4550a2`,
  with `libsqlite3-sys 0.38.1`; its bundled amalgamation reports
  `SQLite 3.53.2` / source ID
  `2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24`.
- Generator source: [generate-released-v031.rs.txt](generate-released-v031.rs.txt).
  It is an isolated `#[cfg(test)]` module appended **only in the disposable
  source checkout** to the original released `src/streams/backup.rs`. It calls
  released `Storage::initialize`, inserts fixed fake schema-v5 records, then
  calls the original published `backup::export`; it asserts runtime SQLite
  3.53.2 and creates a standalone live DB and a genuine standalone backup export.
- Successful generation: [GitHub Actions run 42047343786](https://github.com/Certain1989/Broadcaster/actions/runs/42047343786),
  read-only `I1 create genuine v0.3.1 SQLite fixtures`, artifact
  `released-v031-sqlite-fixtures`. No write-capable workflow or PR merge was
  needed. Temporary workflow removed from proposed source diff after generation.

| Fixture | SHA-256 of committed .gz | SHA-256 after decompression |
| --- | --- | --- |
| `released-v031-live.sqlite3.gz` | `093be7113694508a137de6d3284b20db7b6ed540d770c7cab194a8c8e5edfcb8` | `cc0407471eacf314ae37b1cd1df24411b9409e9b1bbc55a011de3be88658d0ee` |
| `released-v031-export.sqlite3.gz` | `21301e893be457a5d6f0d1f8a16f8d5054a497dc164a7d3088f0dd857a93f6cb` | `14936fbb5c948515931891d03030223d94cd33262825f38a3508c35fc5b61cd9` |

The checksums were independently verified from the downloaded Action artifact,
both compressed and uncompressed. An independently compiled SQLite **3.53.2**
from the pinned `libsqlite3-sys 0.38.1` source also reopened both: reported
`PRAGMA application_id = 1112687444` (`0x42524354`), `user_version=5`,
and `integrity_check=ok`.

## Reproduction without using new SQLite

Starting from a clean checkout of the **release commit** above (not I1 HEAD),
obtain the generator source from the candidate and append its exact bytes to
`src/streams/backup.rs` in that disposable release checkout:

```bash
git rev-parse HEAD # must equal 0fcdcec9a17c61e053ce0c5db1a2487b8baff245
git rev-parse HEAD^{tree} # must equal 2f5c88166cab1cbb947c736f3724a01a11e53186
cat /path/to/generate-released-v031.rs.txt >> src/streams/backup.rs
umask 077
export BROADCASTER_V031_FIXTURE_OUT="$(mktemp -d)"
cargo test --locked --bin broadcaster \
  streams::backup::released_v031_fixture_generator::generate_released_v031_sqlite_fixtures \
  -- --exact --test-threads=1
cd "$BROADCASTER_V031_FIXTURE_OUT"
sha256sum released-v031-*.sqlite3
gzip -n -9 released-v031-live.sqlite3 released-v031-export.sqlite3
sha256sum released-v031-*.gz
```

Use Rust/GTK and crate dependencies appropriate to the pinned release; the
generator test's runtime version assertion fails if an incorrect SQLite engine
is used. Release-code generated data may have incidental SQLite page-layout
or build differences across environments: verify application semantics and
provenance as well as hashes, rather than asserting an untested universal
byte-for-byte reproducibility claim.

## Candidate acceptance coverage

The I1 regression in `src/streams/legacy_sqlite_v031_tests.rs`, included from
the existing backup tests, opens the old live database under **SQLite 3.54.1**,
checks all source/channel/guide/programme/mapping/favourite/settings rows,
schema IDs, integrity and foreign keys, reads the **old export** under the new
engine, rejects a corrupt copy without replacing the active database, stages
and confirms the old backup export, then executes the production serialized
**startup pre-open restore gate** and verifies that a durable recovery snapshot
preserves the previous active data. This test makes no network requests and
never opens a real user's database.
