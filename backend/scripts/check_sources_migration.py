#!/usr/bin/env python3
"""Compare old and new source evaluators before producing a migration.

Both evaluator executables accept NDJSON; see patterns/examples/source_parity.rs.
This tool never changes the input database or writes to the network.
"""
import argparse
import json
import math
import pathlib
import sqlite3
import subprocess
from migrate_sources import convert_clip, convert_document


def difference(a, b):
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return abs(a - b)
    if isinstance(a, dict) and isinstance(b, dict) and a.keys() == b.keys():
        return max((difference(a[k], b[k]) for k in a), default=0)
    if isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        return max((difference(x, y) for x, y in zip(a, b)), default=0)
    return 0 if a == b else math.inf


def contains_audio(value):
    if isinstance(value, dict):
        return value.get('type') == 'audio' or any(map(contains_audio, value.values()))
    return isinstance(value, list) and any(map(contains_audio, value))


def evaluate(binary, requests):
    process = subprocess.run([binary], input='\n'.join(map(json.dumps, requests)) + '\n', text=True, capture_output=True, check=True)
    results = [json.loads(line) for line in process.stdout.splitlines()]
    if len(results) != len(requests):
        raise RuntimeError(f'{binary}: expected {len(requests)} results, got {len(results)}')
    return results


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def migration_sql(statements):
    # Refuse a stale plan atomically rather than leaving unconverted rows behind.
    return ('BEGIN IMMEDIATE;\n'
            'CREATE TEMP TABLE sources_migration_guard (changed INTEGER CHECK (changed = 1));\n'
            + '\n'.join(statement + '\nINSERT INTO sources_migration_guard VALUES (changes());' for statement in statements)
            + '\nDROP TABLE sources_migration_guard;\nCOMMIT;\n')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--database', required=True)
    parser.add_argument('--old', required=True)
    parser.add_argument('--new', required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('--old-presets', help='Original presets.json, to compare shipped looks too')
    args = parser.parse_args()
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f'file:{pathlib.Path(args.database).resolve()}?mode=ro', uri=True)
    db.row_factory = sqlite3.Row
    clips = []
    statements = []
    for row in db.execute('SELECT * FROM clips ORDER BY id'):
        clip = dict(graph=row['graph'], start=row['start'], duration=row['duration'], seed=int(row['seed']), inputs=json.loads(row['inputs_json']))
        clips.append((row['id'], clip))
        new = convert_clip(clip)
        payload = json.dumps(new['inputs'], separators=(',', ':'))
        if new != clip:
            statements.append(f"UPDATE clips SET graph={quote(new['graph'])}, inputs_json={quote(payload)} WHERE id={quote(row['id'])} AND graph={quote(row['graph'])} AND inputs_json={quote(row['inputs_json'])};")
    draft_count = 0
    for row in db.execute('SELECT id,base_json,state_json FROM drafts ORDER BY id'):
        changes = []
        for column in ('base_json', 'state_json'):
            old = json.loads(row[column])
            new = convert_document(old)
            for key, clip in old.get('clips', {}).items():
                clips.append((f'draft:{row["id"]}:{column}:{key}', clip))
            if new != old:
                changes.append(f'{column}={quote(json.dumps(new, separators=(",", ":")))}')
        if changes:
            draft_count += 1
            statements.append(f"UPDATE drafts SET {', '.join(changes)} WHERE id={quote(row['id'])} AND base_json={quote(row['base_json'])} AND state_json={quote(row['state_json'])};")
    if args.old_presets:
        for preset in json.loads(pathlib.Path(args.old_presets).read_text())['presets']:
            clips.append((f'preset:{preset["form"]}:{preset["name"]}', dict(graph=preset['form'], inputs=preset['inputs'], start=0, duration=16, seed=41)))
    report = dict(checked=0, validated_audio=0, skipped_audio=[], errors=[], mismatches=[], maximum_error=0, draft_rows=draft_count)
    cells = [dict(id=f'fixture{n//4}:{n%4}', group='all', world=[n%5,n//5,3], uvz=[n%5,n//5,3]) for n in range(20)]
    pending = [(key, clip) for key, clip in clips if not contains_audio(clip)]
    report['skipped_audio'] = [key for key, clip in clips if contains_audio(clip)]
    audio = [(key, clip) for key, clip in clips if contains_audio(clip)]
    for offset in range(0, len(audio), 64):
        batch = audio[offset:offset+64]
        requests = [dict(clip=clip, cells=[], beats=[]) for _, clip in batch]
        old = evaluate(args.old, requests)
        new = evaluate(args.new, [dict(r, clip=convert_clip(r['clip'])) for r in requests])
        for (key, clip), request, a, b in zip(batch, requests, old, new):
            if 'error' in a or 'error' in b:
                report['errors'].append(dict(id=key, old=a.get('error'), new=b.get('error')))
            else:
                report['validated_audio'] += 1
    for offset in range(0, len(pending), 64):
        batch = pending[offset:offset+64]
        requests = [dict(clip=clip, cells=cells, beats=[clip['start'] + clip['duration'] * i / 64 for i in range(64)] + [math.nextafter(clip['start'] + clip['duration'], clip['start']), clip['start'] + clip['duration']]) for _, clip in batch]
        old = evaluate(args.old, requests)
        new = evaluate(args.new, [dict(r, clip=convert_clip(r['clip'])) for r in requests])
        for (key, clip), request, a, b in zip(batch, requests, old, new):
            if 'error' in a or 'error' in b:
                report['errors'].append(dict(id=key, old=a.get('error'), new=b.get('error')))
                continue
            error = difference(a, b)
            report['maximum_error'] = max(report['maximum_error'], error)
            report['checked'] += 1
            if error > 1e-6:
                samples = [dict(beat=beat, error=difference(x, y), beats_before_end=clip['start'] + clip['duration'] - beat)
                           for beat, x, y in zip(request['beats'], a['ok'], b['ok']) if difference(x, y) > 1e-6]
                report['mismatches'].append(dict(id=key, error=error, samples=samples))
        print(f'{min(offset+64,len(pending))}/{len(pending)} compared', flush=True)
    (out/'report.json').write_text(json.dumps(report, indent=2) + '\n')
    (out/'proposed_changes.sql').write_text(migration_sql(statements))
    print(json.dumps({k:len(v) if isinstance(v,list) else v for k,v in report.items()}))
    if report['errors'] or report['mismatches']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
