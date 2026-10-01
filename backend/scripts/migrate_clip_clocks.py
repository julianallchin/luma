#!/usr/bin/env python3
"""Graph version 2: a band becomes a shifted space (docs/specs/clip-graphs.md 8.7).

    migrate_clip_clocks.py --database <copy of luma.db> --out <dir>
        [--old <v1 clip_graph_parity>] [--new <v2 clip_graph_parity>]

Version 1 `space` nodes carried a band: `offset` and `width` cut a stroke
out of the axis, and a curve over the space read `x = (a - offset) / width`
inside it and `low` (or black) outside. Version 2 `space` has `shift` and
`length`: it gives `x = (a - shift) / length` (the difference wrapped first
on a ring), with no inside or outside. Per space with a band, with `S` the
shape of each curve that reads it:

- no band (offset 0, width 1): drop the inputs.
- a still band inside 0..1 (numbers): `S` placed on [offset, offset +
  width] with jumps to 0 outside; the space keeps no inputs.
- any other band: `shift` = the old offset and `length` = the old width,
  unchanged (numbers or the same wires, eases kept); `S` gains jumps to 0
  at x 0 and x 1, so it reads `low` outside as before.

No node is added: each graph keeps its node count. Writes <dir>/clips.sql
and <dir>/drafts.sql (`-- expected: N`, guarded UPDATEs for
`clip_graphs_apply`), <dir>/report.md and <dir>/report.json. With --old and
--new it also plays every clip before and after on a stand-in rig and sorts
them into exact, edge-only and approximate. Opens the database read-only;
never writes it or the network.
"""
import argparse
import collections
import copy
import json
import math
import pathlib
import sqlite3
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from migrate_clip_graphs import compact, quote  # noqa: E402

VERSION = 2
OUT = 0.0  # a shape value that reads the curve's low
# Heads spread on U, V and Z, as the clip graphs parity check uses.
CELLS = [dict(id=f"fixture{n // 4}:{n % 4}", group="all",
              world=[n % 5, n // 5, 3 + (3 * n) % 7 / 4],
              uvz=[n % 5, n // 5, 3 + (3 * n) % 7 / 4]) for n in range(20)]


class Refused(Exception):
    """A graph this migration does not convert, with the reason."""


# ---- shapes ----

def points_of(curve):
    shape = curve.get("inputs", {}).get("shape")
    return copy.deepcopy(shape["points"]) if shape else [[0, 0], [1, 1]]


def clean(x):
    x = round(x, 12)
    return int(x) if x == int(x) else x


def tidy_shape(points):
    """Drops a point equal to the one before it; a jump (two points at one
    x) and the last point keep no ease."""
    out = []
    for p in points:
        p = [clean(p[0]), clean(p[1]), *p[2:]]
        if out and p[:2] == out[-1][:2]:
            out[-1] = p if len(p) > 2 else out[-1]
            continue
        out.append(p)
    out[-1] = out[-1][:2]
    for i in range(len(out) - 1):
        if out[i][0] == out[i + 1][0]:
            out[i] = out[i][:2]
    return {"points": out}


def outside(points):
    """`points` with jumps to 0 at x 0 and x 1: the shape on [0, 1] closed
    and the curve's low outside, as a version 1 band read."""
    return tidy_shape([[0, OUT], *points, [1, OUT]])


def place(points, start, end):
    """The shape on [start, end] inside 0..1 and 0 elsewhere, with jumps.
    Scaling x keeps every segment's ease, so this is exact."""
    mapped = [[start + p[0] * (end - start), *p[1:]] for p in points]
    return tidy_shape([[0, OUT], [start, OUT], *mapped, [end, OUT], [1, OUT]])


# ---- graphs ----

def number(value):
    return isinstance(value, (int, float))


def convert_space(nodes, space_id, notes):
    space = nodes[space_id]
    inputs = space.setdefault("inputs", {})
    offset, width = inputs.pop("offset", 0), inputs.pop("width", 1)
    wrap = space.get("settings", {}).get("wrap") == "yes"
    users = [node for node in nodes.values() if node["kind"] == "curve"
             and (node["inputs"].get("x") or {}).get("node") == space_id]
    if offset == 0 and width == 1:
        notes.add("no band")
    elif any(user.get("settings", {}).get("kind") == "color" for user in users):
        raise Refused("a color curve over a band (black outside the band)")
    elif number(offset) and number(width) and not wrap and width > 0 and \
            0 <= offset and offset + width <= 1:
        notes.add("still band: a region of the curve")
        for user in users:
            user["inputs"]["shape"] = place(points_of(user), offset, offset + width)
    else:
        notes.add("band: shift and length")
        if offset != 0:
            inputs["shift"] = offset
        if width != 1:
            inputs["length"] = width
        for user in users:
            user["inputs"]["shape"] = outside(points_of(user))
    if not inputs:
        del space["inputs"]


def convert(graph):
    """(version 2 graph, notes). Raises Refused."""
    if graph.get("version") == VERSION:
        return graph, set()
    if graph.get("version") != 1:
        raise Refused(f"graph version {graph.get('version')}")
    nodes = copy.deepcopy(graph["nodes"])
    for node in nodes.values():
        if node["kind"] == "time" and set(node.get("inputs", {})) - {"clock", "phase"}:
            raise Refused("a version 1 time node with delay or length")
    notes = set()
    for space_id in [i for i, n in nodes.items() if n["kind"] == "space"]:
        convert_space(nodes, space_id, notes)
    return {"version": VERSION, "nodes": nodes}, notes


def convert_document(value):
    """A score document (or any JSON) with every version 1 graph converted."""
    if isinstance(value, dict):
        if value.get("version") == 1 and isinstance(value.get("nodes"), dict):
            return convert(value)[0]
        return {k: convert_document(v) for k, v in value.items()}
    if isinstance(value, list):
        return [convert_document(v) for v in value]
    return value


# ---- the run ----

def clip_json(row, graph):
    return {"name": row["name"] or "clip", "start": row["start"], "duration": row["duration"],
            "seed": int(row["seed"]), "blend_mode": row["blend_mode"], "graph": graph}


def beats(clip):
    start, end = clip["start"], clip["start"] + clip["duration"]
    steps = math.ceil(clip["duration"] * 8)
    return [start + k / 8 for k in range(steps) if start + k / 8 < end]


def evaluate(binary, requests, flag=None):
    process = subprocess.run([binary] + ([flag] if flag else []),
                             input="".join(json.dumps(r) + "\n" for r in requests),
                             text=True, capture_output=True, check=True)
    return [json.loads(line) for line in process.stdout.splitlines()]


def values(frames):
    """Every number of every frame, in order."""
    out = []

    def walk(v):
        if isinstance(v, bool) or v is None:
            return
        if isinstance(v, (int, float)):
            out.append(float(v))
        elif isinstance(v, dict):
            for k in sorted(v):
                walk(v[k])
        elif isinstance(v, list):
            for item in v:
                walk(item)
    walk(frames)
    return out


def compare(old, new):
    """('exact' or 'differs', the largest difference)."""
    a, b = values(old), values(new)
    if len(a) != len(b):
        return "differs", math.inf
    worst = max((abs(x - y) for x, y in zip(a, b)), default=0.0)
    return ("exact" if worst <= 1e-6 else "differs"), worst


# Off the 1/8 beat grid, so no sample lands exactly on a band's end.
NUDGE = 1e-4 * math.sqrt(2)


def run(database, out, old_bin=None, new_bin=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    statements, failures = [], collections.Counter()
    notes, growth, examples = collections.Counter(), collections.Counter(), collections.defaultdict(list)
    pairs, checked = [], []
    rows = db.execute("SELECT id, name, start, duration, seed, blend_mode, graph_json "
                      "FROM clips ORDER BY id").fetchall()
    for row in rows:
        old = json.loads(row["graph_json"])
        try:
            new, why = convert(old)
        except Refused as e:
            failures[str(e)] += 1
            examples[str(e)].append(row["id"])
            continue
        for n in why:
            notes[n] += 1
        growth[len(new["nodes"]) - len(old["nodes"])] += 1
        if new != old:
            text = compact(new)
            statements.append(f"UPDATE clips SET graph_json = {quote(text)} "
                              f"WHERE id = {quote(row['id'])} AND graph_json = {quote(row['graph_json'])};")
        pairs.append((row, clip_json(row, old), clip_json(row, new)))
    (out / "clips.sql").write_text("\n".join([f"-- expected: {len(statements)}", *statements]) + "\n")
    drafts = []
    for row in db.execute("SELECT id, base_json, state_json FROM drafts ORDER BY id"):
        sets = []
        for column in ("base_json", "state_json"):
            document = json.loads(row[column])
            try:
                new = convert_document(document)
            except Refused as e:
                failures[f"draft: {e}"] += 1
                continue
            if new != document:
                sets.append(f"{column} = {quote(compact(new))}")
        if sets:
            drafts.append(f"UPDATE drafts SET {', '.join(sets)} WHERE id = {quote(row['id'])} "
                          f"AND base_json = {quote(row['base_json'])} "
                          f"AND state_json = {quote(row['state_json'])};")
    (out / "drafts.sql").write_text("\n".join([f"-- expected: {len(drafts)}", *drafts]) + "\n")
    report = {"clips": len(rows), "converted": len(pairs), "refused": sum(failures.values()),
              "changed": len(statements), "drafts_changed": len(drafts),
              "forms": dict(notes), "node_count_change": {str(k): v for k, v in sorted(growth.items())}, "refusals": dict(failures)}
    if new_bin:
        errors = collections.Counter()
        results = evaluate(new_bin, [c for _, _, c in pairs], "--check")
        good = []
        for (row, a, b), result in zip(pairs, results):
            if "error" in result:
                errors[result["error"].split(";")[0][:90]] += 1
                examples["check: " + result["error"][:90]].append(row["id"])
            else:
                good.append((row, a, b))
        report["checker_errors"] = dict(errors)
        if old_bin:
            classes, worst = collections.Counter(), {}
            olds = evaluate(old_bin, [dict(clip=a, cells=CELLS, beats=beats(a)) for _, a, _ in good])
            news = evaluate(new_bin, [dict(clip=b, cells=CELLS, beats=beats(b)) for _, _, b in good])
            again = []
            for (row, a, b), x, y in zip(good, olds, news):
                if "error" in x or "error" in y:
                    kind = "not played: " + (x.get("error") or y.get("error"))[:60]
                    classes[kind] += 1
                    examples[kind].append(row["id"])
                    continue
                kind, diff = compare(x["ok"], y["ok"])
                if kind == "exact":
                    classes[kind] += 1
                else:
                    again.append((row, a, b, diff))
            # A clip that differs only on the beat grid differs only where a
            # head sits exactly on a band's end ("edge").
            nudged = lambda c: dict(clip=c, cells=CELLS,  # noqa: E731
                                    beats=[t + NUDGE for t in beats(c)])
            olds = evaluate(old_bin, [nudged(a) for _, a, _, _ in again])
            news = evaluate(new_bin, [nudged(b) for _, _, b, _ in again])
            for (row, _, _, diff), x, y in zip(again, olds, news):
                kind, off = compare(x["ok"], y["ok"])
                kind = "edge" if kind == "exact" else "approximate"
                classes[kind] += 1
                examples[kind].append(f"{row['id']} ({max(diff, off):.3g})")
                worst[row["id"]] = max(diff, off)
            report["parity"] = dict(classes)
    report["examples"] = {k: v[:5] for k, v in examples.items()}
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    lines = ["# Graph version 2: bands to shifted spaces", ""]
    lines += [f"- {k}: {v}" for k, v in report.items() if not isinstance(v, dict)]
    for key in ("forms", "node_count_change", "refusals", "checker_errors", "parity"):
        lines += ["", f"## {key}", ""]
        lines += [f"- {k}: {v}" for k, v in sorted(report.get(key, {}).items(),
                                                   key=lambda kv: -kv[1])] or ["- none"]
    lines += ["", "## examples", ""] + [f"- {k}: {', '.join(v)}" for k, v in report["examples"].items()]
    (out / "report.md").write_text("\n".join(lines) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--old", help="clip_graph_parity built from a version 1 checkout")
    parser.add_argument("--new", help="clip_graph_parity built from this checkout")
    args = parser.parse_args()
    report = run(args.database, args.out, args.old, args.new)
    print(json.dumps({k: v for k, v in report.items() if k != "examples"}, indent=1))


if __name__ == "__main__":
    main()
