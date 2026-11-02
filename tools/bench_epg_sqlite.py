#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Disposable synthetic Broadcaster v5 SQLite storage measurement.

Measures SQLite primitives, NOT GTK application startup or Rust schema validation.
For bounded comparison: --rows 1000 8000 24000 (default); --modes delete wal.
The first reading is not a provably cold OS filesystem cache.
"""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import tempfile
from time import perf_counter

MIGRATIONS = Path(__file__).resolve().parents[1] / 'src/streams/migrations'


def timed(action):
    before = perf_counter()
    value = action()
    return round((perf_counter() - before) * 1000, 3), value


def size(path):
    return path.stat().st_size if path.exists() else 0


def sizes(path):
    return {"main": size(path), "wal": size(Path(str(path) + '-wal')),
            "journal": size(Path(str(path) + '-journal'))}


def connect(path):
    db = sqlite3.connect(path)
    db.execute('PRAGMA foreign_keys=ON')
    db.execute('PRAGMA busy_timeout=5000')
    return db


def create(path, journal):
    db = connect(path)
    for version in range(1, 6):
        name = next(MIGRATIONS.glob(f'{version:03d}_*.sql'))
        db.executescript(name.read_text(encoding='utf-8'))
    assert db.execute('PRAGMA user_version').fetchone()[0] == 5
    assert db.execute('PRAGMA application_id').fetchone()[0] == 0x42524354
    assert db.execute(f'PRAGMA journal_mode={journal}').fetchone()[0].lower() == journal
    db.execute('INSERT INTO epg_sources(id,name,origin_kind,origin_locator,created_at,updated_at) '
               "VALUES('bench','Synthetic Guide','local_file','file:///synthetic',0,0)")
    db.executemany('INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES(?,?,?)',
                   [('bench', f'ch-{x:03}', f'Synthetic channel {x:03}') for x in range(64)])
    db.commit()
    return db


def run(rows, journal, directory):
    path = directory / f'v5-{journal}-{rows}.sqlite3'
    db = create(path, journal)
    # Same row shape as actual XMLTV storage, but synthetic and explicitly bounded.
    description = 'Synthetic programme description. ' * 16
    sql = ('INSERT INTO epg_programmes(source_id,channel_id,start_utc,stop_utc,title,description) '
           'VALUES(?,?,?,?,?,?)')
    observed_peak = sum(sizes(path).values())
    elapsed = 0.0
    for offset in range(0, rows, 1000):
        def insert():
            db.executemany(sql, (
                ('bench', f'ch-{n % 64:03}', 1700000000 + (n // 64) * 3600,
                 1700001800 + (n // 64) * 3600, f'Programme {n:09}', description)
                for n in range(offset, min(rows, offset + 1000))))
            transaction_size = sum(sizes(path).values())
            db.commit()
            return transaction_size
        ms, transaction_size = timed(insert)
        elapsed += ms
        observed_peak = max(observed_peak, transaction_size, sum(sizes(path).values()))
    before = sizes(path)
    db.close()
    coldish_ms, _ = timed(lambda: integrity(path))
    warm_ms, _ = timed(lambda: integrity(path))
    connection_ms, _ = timed(lambda: schema_proxy(path))
    db = connect(path)
    db.execute(f'PRAGMA journal_mode={journal}')
    def delete():
        db.execute("DELETE FROM epg_programmes WHERE source_id='bench'")
        allocated_during_delete = sum(sizes(path).values())
        db.commit()
        return allocated_during_delete
    ms_delete, delete_during_transaction = timed(delete)
    observed_peak = max(observed_peak, delete_during_transaction)
    after = sizes(path)
    freelist = db.execute('PRAGMA freelist_count').fetchone()[0]
    page_count = db.execute('PRAGMA page_count').fetchone()[0]
    if journal == 'wal':
        db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
    post_checkpoint = sizes(path)
    db.close()
    return {'journal': journal, 'rows': rows, 'insert_ms': round(elapsed, 2),
            'pre_delete_bytes': before, 'observed_peak_allocated_bytes': observed_peak,
            'first_integrity_and_fk_ms': coldish_ms, 'warm_integrity_and_fk_ms': warm_ms,
            'connect_and_schema_proxy_ms': connection_ms, 'delete_ms': ms_delete,
            'post_delete_bytes': after, 'post_checkpoint_bytes': post_checkpoint,
            'freelist_pages_after_delete': freelist, 'page_count_after_delete': page_count}


def integrity(path):
    db = connect(path)
    try:
        assert db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        assert db.execute('PRAGMA foreign_key_check').fetchone() is None
    finally:
        db.close()


def schema_proxy(path):
    db = connect(path)
    try:
        assert db.execute('PRAGMA user_version').fetchone()[0] == 5
        assert db.execute('PRAGMA application_id').fetchone()[0] == 0x42524354
        assert db.execute("SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','index')").fetchone()[0] > 0
    finally:
        db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rows', nargs='+', type=int, default=[1000, 8000, 24000])
    parser.add_argument('--modes', nargs='+', choices=('delete', 'wal'), default=['delete', 'wal'])
    args = parser.parse_args()
    if any(not 1 <= rows <= 100000 for rows in args.rows):
        parser.error('each synthetic population must contain 1..100000 rows')
    with tempfile.TemporaryDirectory(prefix='broadcaster-v5-bench-') as name:
        root = Path(name)
        result = {'python_sqlite': sqlite3.sqlite_version, 'python': __import__('sys').version.split()[0],
                  'os': os.uname().sysname if hasattr(os, 'uname') else os.name,
                  'storage': 'disposable temporary directory',
                  'warning': 'first reading is not a guaranteed cold filesystem cache; not application startup',
                  'results': [run(rows, mode, root) for mode in args.modes for rows in args.rows]}
        print(json.dumps(result, indent=2))

if __name__ == '__main__':
    main()
