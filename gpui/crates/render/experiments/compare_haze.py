#!/usr/bin/env python3
"""Compare frozen haze-lab captures; requires Pillow and numpy.

Usage: compare_haze.py CAPTURE_ROOT REFERENCE_DIRECTORY MODE [MODE ...]
Writes display-space error/stability/timing metrics and an HTML contact sheet.
These metrics measure agreement with the reference, not physical realism.
"""
import hashlib
import html
import json
from pathlib import Path
import sys

import numpy as np
from PIL import Image


def pixels(path):
    return np.asarray(Image.open(path).convert('RGB'), dtype=np.float32)


def rmse(a, b):
    return float(np.sqrt(np.mean((a - b) ** 2)))


def main():
    root, reference, *modes = sys.argv[1:]
    root = Path(root).resolve()
    rows, cards = [], []
    for ref_dir in sorted((root / reference).iterdir()):
        if not ref_dir.is_dir():
            continue
        ref = pixels(ref_dir / 'settled.png')
        ref_meta = json.loads((ref_dir / 'capture.json').read_text())
        mask = ref.max(axis=2) > 15
        images = [(reference, ref_dir / 'settled.png')]
        for mode in modes:
            path = root / mode / ref_dir.name
            if not (path / 'capture.json').exists():
                continue
            meta = json.loads((path / 'capture.json').read_text())
            if meta['case'] != ref_meta['case'] or meta['size'] != ref_meta['size']:
                raise ValueError(f'incompatible pose/size: {path}')
            actual = pixels(path / 'settled.png')
            row = dict(case=ref_dir.name, mode=mode, rmse=rmse(actual, ref),
                       lit_rmse=rmse(actual[mask], ref[mask]),
                       absolute_error_p99=float(np.percentile(np.abs(actual-ref),99)),
                       png_sha256=hashlib.sha256((path/'settled.png').read_bytes()).hexdigest())
            if (path / 'first.png').exists():
                row['first_rmse'] = rmse(pixels(path/'first.png'), ref)
                stills = [actual] + [pixels(p) for p in sorted(path.glob('still-*.png'))]
                row['still_stddev'] = float(np.sqrt(np.mean(np.var(stills, axis=0))))
            if (path/'pan.png').exists() and (ref_dir/'pan.png').exists():
                row['pan_rmse'] = rmse(pixels(path/'pan.png'), pixels(ref_dir/'pan.png'))
            if meta['frames']:
                for key in ['gpu_ms','haze_ms','grid_ms','scene_ms','encode_ms']:
                    values = [f[key] for f in meta['frames']]
                    row[key] = dict(median=float(np.median(values)), p95=float(np.percentile(values,95)))
            rows.append(row)
            images.append((mode, path/'settled.png'))
        cards.append(f'<h2>{html.escape(ref_dir.name)}</h2><div class="row">'+''.join(
            f'<figure><figcaption>{html.escape(label)}</figcaption><a href="{image.relative_to(root)}"><img src="{image.relative_to(root)}"></a></figure>'
            for label,image in images)+'</div>')
    artifact = dict(reference=reference, units='display RGB code values (0–255); GPU milliseconds',
                    note='Uniform-haze variants change the medium and are not accuracy comparisons against cloudy references.', rows=rows)
    (root/'comparison.json').write_text(json.dumps(artifact,indent=2))
    (root/'comparison.html').write_text('<!doctype html><meta charset="utf-8"><title>Haze comparison</title><style>body{background:#17171b;color:#eee;font:16px system-ui;margin:24px}.row{display:flex;overflow-x:auto;gap:16px}figure{margin:0;min-width:640px}img{width:640px}figcaption{padding:8px 0}h2{margin-top:32px}</style><h1>Frozen haze experiments</h1><p>Click an image to inspect native resolution. Compare shadow shafts below the truss, source highlights, and boundaries.</p>'+''.join(cards))
    for r in rows:
        print(f"{r['case']:26} {r['mode']:27} RMSE {r['rmse']:6.2f} pan {r.get('pan_rmse',float('nan')):6.2f} GPU {r.get('gpu_ms',{}).get('median',float('nan')):6.3f} ms")


if __name__ == '__main__':
    main()
