#!/usr/bin/env python3
"""Make and verify a complete, independent local Luma library backup."""
import argparse
import datetime
import hashlib
import json
import shutil
import sqlite3
from pathlib import Path


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(8 * 1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def snapshot(source, destination):
    source = source.resolve()
    destination = destination.resolve()
    if destination == source or source in destination.parents:
        raise ValueError('Backup must be outside the source library')
    destination.mkdir(parents=True, exist_ok=False)
    files = []
    for item in sorted(source.rglob('*')):
        relative = item.relative_to(source)
        if item.is_dir():
            (destination / relative).mkdir(exist_ok=True)
            continue
        if item.name.endswith(('-wal', '-shm')):
            continue  # included consistently by SQLite's online backup API
        target = destination / relative
        if item.suffix == '.db':
            with sqlite3.connect(item.as_uri() + '?mode=ro', uri=True) as src:
                with sqlite3.connect(target) as dst:
                    src.backup(dst)
                    result = dst.execute('PRAGMA integrity_check').fetchall()
                    if result != [('ok',)]:
                        raise RuntimeError(f'{relative}: {result}')
            print(f'Backed up and integrity-checked {relative}', flush=True)
            checksum = digest(target)
        else:
            before = item.stat()
            shutil.copy2(item, target)
            checksum = digest(target)
            if digest(item) != checksum or item.stat().st_mtime_ns != before.st_mtime_ns:
                raise RuntimeError(f'Source changed while copying {relative}; backup incomplete')
        files.append({'path': str(relative), 'bytes': target.stat().st_size, 'sha256': checksum})
        if len(files) % 50 == 0:
            print(f'Verified {len(files)} files', flush=True)
    manifest = {'source': str(source), 'created_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                'files': files, 'bytes': sum(f['bytes'] for f in files)}
    (destination / 'snapshot-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(json.dumps({'backup': str(destination), 'files': len(files), 'bytes': manifest['bytes']}), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    snapshot(args.source, args.destination)
