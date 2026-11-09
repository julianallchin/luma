"""Dump what the Python reference builder makes of each spec, for `fixture-parity`.

Run with the fixtures checkout's interpreter and its `tools/reference_builder`
on `sys.path`. Reads spec paths from stdin, one per line, and writes one JSON
object per line to stdout, in the builder's own space (Y up, base at y = 0):

  {"path", "fallback", "nodes": [{"name", "parent", "world", "joint", "aabb"}],
   "cells": N, "cell_emitters": {k: n}, "cell_centres": {k: [x, y, z]}}

or {"path", "crash"} when the builder itself raised. `world` is the node's
rest matrix, row-major. `aabb` is the box of the meshes the node owns (the
vertices its triangles use), in fixture space, or null. Cell emitters and centres count only
the emitter meshes that survive the build (a round face drops some after they
are made, but the builder's own counts include them).
"""
import json
import sys

import numpy as np

import build

captured = []


class Recorder(build.Rig):
    def __init__(s):
        super().__init__()
        s.emits = []
        captured.append(s)

    def _record(s, k, c):
        j = s.cell_em.get(k, 0)
        p = s.T @ np.array([c[0], c[1], c[2], 1.0])
        s.emits.append((f'cell_{k}_em_{j}', k, p[:3]))

    def emit_cell(s, k, c, *a, **kw):
        s._record(k, c)
        return super().emit_cell(k, c, *a, **kw)

    def emit_mesh(s, k, mesh, c, *a, **kw):
        s._record(k, c)
        return super().emit_mesh(k, mesh, c, *a, **kw)


build.Rig = Recorder


def dump(path):
    captured.clear()
    spec = json.load(open(path))
    _, extras, P = build.build_spec(spec)
    b = captured[-1]
    boxes = {}
    for name, mesh, parent in b.items:
        used = mesh.vertices[np.unique(mesh.faces)] if len(mesh.faces) else mesh.vertices
        v = used @ b.W[parent][:3, :3].T + b.W[parent][:3, 3]
        lo, hi = v.min(0), v.max(0)
        if parent in boxes:
            lo, hi = np.minimum(lo, boxes[parent][0]), np.maximum(hi, boxes[parent][1])
        boxes[parent] = (lo, hi)
    nodes = []
    for name, parent, _, ex in b.nodes:
        box = boxes.get(name)
        nodes.append({'name': name, 'parent': parent, 'world': b.W[name].flatten().tolist(),
                      'joint': ex.get('joint'), 'element': ex.get('element'),
                      'aabb': [box[0].tolist(), box[1].tolist()] if box else None})
    alive = {it[0] for it in b.items}
    counts, sums = {}, {}
    for name, k, p in b.emits:
        if name not in alive:
            continue
        counts[k] = counts.get(k, 0) + 1
        sums[k] = sums.get(k, 0) + p
    return {'path': path, 'fallback': P.get('fallback_error'), 'nodes': nodes,
            'cells': b.next_cell, 'cell_emitters': {str(k): v for k, v in sorted(counts.items())},
            'cell_centres': {str(k): (sums[k] / counts[k]).tolist() for k in sorted(counts)}}


for line in sys.stdin:
    path = line.strip()
    if not path:
        continue
    try:
        out = dump(path)
    except Exception as e:   # the builder itself crashed (build3 catches most)
        out = {'path': path, 'crash': repr(e)}
    sys.stdout.write(json.dumps(out) + '\n')
    sys.stdout.flush()
