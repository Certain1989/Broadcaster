<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: CC0-1.0 -->

# Synthetic SQLite v5 storage measurements (bounded host sample)

Reproduce with `python3 tools/bench_epg_sqlite.py --rows 1000 8000 24000`. The script uses **all five actual repository migration SQL files**, 64 synthetic guide channels, fixed synthetic programme titles/descriptions, enabled FK checks and disposable databases. It measures SQLite `PRAGMA integrity_check` plus `foreign_key_check`, and separately a **schema proxy** (application ID, schema version and `sqlite_master` lookup); the proxy is **not** Broadcaster's Rust `connect()` or complete schema-shape validator. The first integrity reading is **not guaranteed cold**: filesystem page cache cannot be controlled from the unprivileged worker. These numbers are not GTK launch time or device results.

Environment: Linux x86_64 ephemeral worker filesystem, Python 3.13.5, SQLite 3.46.1, 4 KiB SQLite page size. All bytes below are actual file lengths in this run; timings are milliseconds and should not be extrapolated to an N100 or 1–6 GiB production guide. Captured from a second complete run on 2026-11-02.

| Journal mode | Programmes | Main before delete | Peak sampled main+journal/WAL | First / repeat integrity+FK (ms) | Schema proxy (ms) | Main after delete | Freed pages after delete |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| DELETE | 1,000 | 954,368 B | 1,718,224 B | 8.4 / 12.1 | 3.2 | 954,368 B | 180 |
| DELETE | 8,000 | 6,062,080 B | 11,945,912 B | 52.1 / 70.0 | 1.8 | 6,062,080 B | 1,427 |
| DELETE | 24,000 | 17,780,736 B | 35,411,320 B | 162.2 / 142.2 | 0.8 | 17,780,736 B | 4,288 |
| WAL | 1,000 | 217,088 B* | 1,008,160 B | 11.8 / 6.7 | 0.8 | 954,368 B | 180 |
| WAL | 8,000 | 6,062,080 B* | 11,162,672 B | 119.0 / 90.2 | 6.9 | 6,062,080 B | 1,427 |
| WAL | 24,000 | 17,072,128 B* | 34,054,768 B | 199.0 / 183.3 | 2.7 | 17,780,736 B | 4,288 |

*WAL rows also had a separate nonzero `-wal` file before deletion; see script JSON output for that mode's exact sizes. In the 24,000-row WAL case the `-wal` file grew to 17,691,312 B after deletion and fell to zero after an explicit **fixture-only** `wal_checkpoint(TRUNCATE)`; production code was not changed to checkpoint or compact automatically. The SQLite main file remained 17,780,736 B after rows were removed. For DELETE mode, observed peaks include rollback journals sampled **inside** insert/delete transactions. All peaks are sampled rather than guaranteed instantaneous maxima. The sample demonstrates that treating the 6 GiB main+WAL import guard as a **total** disk bound would be wrong.

**Interpretation:** even these smaller synthetic databases show integrity scans are substantially slower than simple schema metadata reads, but whether the full application's startup check is unacceptable at 1/3/6 GiB remains **UNMEASURED**. No security checks were removed or cached; no VACUUM, automatic pruning of recovery snapshots or change to journal mode was introduced. A controlled 1–6 GiB benchmark with verified native build, real storage class and if desired owner N100 device measurements is the next evidence gate before changing SQLite initialization policy. The host exercise shows freed pages retained for reuse; it does not justify destructive compaction.
