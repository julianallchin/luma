#!/usr/bin/env python3
"""Time phase from turns to degrees (data migration 2026-10-01).

    migrate_clip_phase_degrees.py --database <copy of luma.db> --out <dir>
        [--old <clip_graph_parity before the change>]
        [--new <clip_graph_parity after the change>]
    migrate_clip_phase_degrees.py --presets backend/crates/patterns/src/presets.json

`time.phase` was in turns: 1 was one event. It is now in degrees: 360 is
one event. Every number that feeds a phase is multiplied by 360, so each
clip plays exactly as before:

    time1.phase = 0.25                      →  time1.phase = 90
    curve1(low=0.5, high=1) → time1.phase   →  curve1(low=180, high=360)
    curve1()                → time1.phase   →  curve1(high=360)

The walk goes up from each phase input through the nodes that make its
value: a curve scales its low and high (an empty low is 0 and stays empty;
an empty high is 1 and becomes 360) and keeps its shape, whose points are
0-1 of low to high; a value node scales its value; a `+`, `-`, `max` or
`min` math node scales every item; a `*` math node scales one item (its
first number, else its first wire). A node on that walk that also feeds an
input that is not on it cannot change in place: then the phase input takes
a new `math` node (`*`, values [the old wire, 360]) and the note says so
(none on 2026-10-01). Every other node, id and value stays, and key order
is kept.

Not idempotent: run it once, on graphs that still hold turns. The guarded
UPDATEs refuse a row that changed since the read.

With --database: writes <dir>/clips.sql and <dir>/drafts.sql (`-- expected:
N`, guarded UPDATEs for `clip_graphs_apply`, which queues them for
PowerSync so Supabase follows) and <dir>/report.json with every changed
clip's score, name and each number before and after. With --old and --new
it plays every changed clip before (old graph, old build) and after (new
graph, new build) and lists any difference. Opens the database read-only.

With --presets: rewrites the shipped presets file in place, one line per
clip as it is laid out.
"""
import argparse
import collections
import copy
import json
import pathlib
import sqlite3
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_math as cm  # noqa: E402
from migrate_clip_graphs import compact, quote  # noqa: E402

TURN = 360


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def degrees(value):
    """`value` turns in degrees, without float noise: 0.7222222222222222
    turns is 260.0."""
    out = round(value * TURN, 9)
    return int(out) if isinstance(value, int) else float(out)


def wire(value):
    return value["node"] if isinstance(value, dict) and "node" in value else None


def phase_inputs(nodes):
    """(time id) for every time node with a phase."""
    return [node_id for node_id, node in nodes.items()
            if node.get("kind") == "time" and "phase" in (node.get("inputs") or {})]


def plan(nodes):
    """The edges the walk scales, as (node id, input, list index or None),
    and the nodes it changes. A `*` math node follows one item."""
    edges, changed = [], []
    stack = [(t, "phase", None) for t in phase_inputs(nodes)]
    seen_edges, seen_nodes = set(), set()
    while stack:
        edge = stack.pop()
        if edge in seen_edges:
            continue
        seen_edges.add(edge)
        edges.append(edge)
        node_id, name, index = edge
        held = nodes[node_id]["inputs"][name]
        held = held[index] if index is not None else held
        source = wire(held)
        if source is None or source in seen_nodes or source not in nodes:
            continue
        seen_nodes.add(source)
        changed.append(source)
        node = nodes[source]
        inputs = node.get("inputs") or {}
        kind = node.get("kind")
        if kind == "curve":
            for bound in ("low", "high"):
                if bound in inputs:
                    stack.append((source, bound, None))
        elif kind == "value":
            stack.append((source, "value", None))
        elif kind == "math":
            items = inputs.get("values") or []
            op = (node.get("settings") or {}).get("op", "*")
            if op == "*":
                pick = next((i for i, v in enumerate(items) if is_number(v)), None)
                if pick is None:
                    pick = next((i for i, v in enumerate(items) if wire(v)), None)
                picks = [] if pick is None else [pick]
            else:
                picks = range(len(items))
            stack.extend((source, "values", i) for i in picks)
    return edges, changed


def shared(nodes, edges, changed):
    """Nodes on the walk that also feed an input not on it."""
    followed = {(n, name, i) for n, name, i in edges}
    out = set()
    for node_id, node in nodes.items():
        for name, held in (node.get("inputs") or {}).items():
            items = list(enumerate(held)) if isinstance(held, list) else [(None, held)]
            for i, value in items:
                source = wire(value)
                if source in changed and (node_id, name, i) not in followed:
                    out.add(source)
    return out


def rewrite(graph, notes, changes=None):
    """The graph with every phase in degrees; the same graph when no time
    node has a phase. `changes` collects (where, before, after)."""
    nodes = graph.get("nodes", {})
    if not phase_inputs(nodes):
        return graph
    new = copy.deepcopy(graph)
    nodes = new["nodes"]
    edges, changed = plan(nodes)
    if shared(nodes, edges, changed):
        # Scale nothing in place: each phase takes a product with 360.
        for time_id in phase_inputs(nodes):
            held = nodes[time_id]["inputs"]["phase"]
            if is_number(held):
                nodes[time_id]["inputs"]["phase"] = degrees(held)
                record(changes, f"{time_id}.phase", held, degrees(held))
                notes["phase number"] += 1
                continue
            n = 1
            while f"math{n}" in nodes:
                n += 1
            nodes[f"math{n}"] = {"kind": "math", "settings": {"op": "*"},
                                 "inputs": {"values": [held, TURN]}}
            nodes[time_id]["inputs"]["phase"] = {"node": f"math{n}"}
            record(changes, f"{time_id}.phase", held, f"math{n} = {held['node']} * {TURN}")
            notes["shared: phase through a new product with 360"] += 1
        return new
    for node_id, name, index in edges:
        inputs = nodes[node_id]["inputs"]
        held = inputs[name][index] if index is not None else inputs[name]
        if not is_number(held):
            if wire(held) is None:
                notes[f"left alone: {node_id}.{name} holds {type(held).__name__}"] += 1
            continue
        where = f"{node_id}.{name}" + (f"[{index}]" if index is not None else "")
        if index is None:
            inputs[name] = degrees(held)
        else:
            inputs[name][index] = degrees(held)
        record(changes, where, held, degrees(held))
        notes["phase number" if nodes[node_id]["kind"] == "time" else f"{nodes[node_id]['kind']} number"] += 1
    for node_id in changed:
        node = nodes[node_id]
        if node.get("kind") == "curve" and "high" not in (node.get("inputs") or {}):
            node.setdefault("inputs", {})["high"] = TURN
            record(changes, f"{node_id}.high", "empty (1)", TURN)
            notes["empty high set to 360"] += 1
        elif node.get("kind") not in ("curve", "value", "math"):
            notes[f"left alone: a {node.get('kind')} node into a phase"] += 1
    return new


def record(changes, where, before, after):
    if changes is not None:
        changes.append({"input": where, "before": before, "after": after})


def rewrite_document(value, notes):
    """A score document (or any JSON) with every version 3 graph rewritten,
    keys in their order."""
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            graph_here = k == "graph" and isinstance(v, dict) and v.get("version") == 3 \
                and "nodes" in v
            out[k] = rewrite(v, notes) if graph_here else rewrite_document(v, notes)
        return out
    if isinstance(value, list):
        return [rewrite_document(v, notes) for v in value]
    return value


def rewrite_presets(path):
    """Rewrite the presets file in place, one clip per line as laid out.
    Returns the names of the changed presets."""
    path = pathlib.Path(path)
    lines, changed = path.read_text().split("\n"), []
    for i, line in enumerate(lines):
        body = line.strip()
        if not body.startswith('{"name"') or '"graph"' not in body:
            continue
        comma = body.endswith(",")
        clip = json.loads(body.rstrip(","))
        new = rewrite(clip["graph"], collections.Counter())
        if new is clip["graph"]:
            continue
        clip["graph"] = new
        indent = line[:len(line) - len(line.lstrip())]
        lines[i] = indent + json.dumps(clip) + ("," if comma else "")
        changed.append(clip["name"])
    path.write_text("\n".join(lines))
    return changed


def labels(db):
    """Score labels by score id: the track's title and the score's name."""
    try:
        rows = db.execute("SELECT s.id, s.name, t.title FROM scores s "
                          "LEFT JOIN tracks t ON t.id = s.track_id").fetchall()
    except sqlite3.OperationalError:
        rows = db.execute("SELECT id, name, NULL FROM scores").fetchall()
    return {r[0]: " / ".join(x for x in (r[2], r[1]) if x) for r in rows}


def run(database, out, old_bin=None, new_bin=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    scores = labels(db)
    rows = db.execute("SELECT id, score_id, name, start, duration, seed, blend_mode, graph_json "
                      "FROM clips ORDER BY id").fetchall()
    notes, statements, changed, pairs = collections.Counter(), [], [], []
    for row in rows:
        old = json.loads(row["graph_json"])
        changes = []
        new = rewrite(old, notes, changes)
        if new != old:
            statements.append(f"UPDATE clips SET graph_json = {quote(compact(new))} "
                              f"WHERE id = {quote(row['id'])} "
                              f"AND graph_json = {quote(row['graph_json'])};")
            changed.append({"id": row["id"], "score": scores.get(row["score_id"], row["score_id"]),
                            "clip": row["name"], "start": row["start"], "changes": changes})
            pairs.append((row["id"], cm.clip_json(row, old), cm.clip_json(row, new)))
    (out / "clips.sql").write_text("\n".join([f"-- expected: {len(statements)}", *statements]) + "\n")
    drafts, draft_notes, draft_ids = [], collections.Counter(), []
    for row in db.execute("SELECT id, base_json, state_json FROM drafts ORDER BY id"):
        sets = []
        for column in ("base_json", "state_json"):
            document = json.loads(row[column])
            new = rewrite_document(document, draft_notes)
            if new != document:
                sets.append(f"{column} = {quote(compact(new))}")
        if sets:
            draft_ids.append(row["id"])
            drafts.append(f"UPDATE drafts SET {', '.join(sets)} WHERE id = {quote(row['id'])} "
                          f"AND base_json = {quote(row['base_json'])} "
                          f"AND state_json = {quote(row['state_json'])};")
    (out / "drafts.sql").write_text("\n".join([f"-- expected: {len(drafts)}", *drafts]) + "\n")
    report = {"clips": len(rows), "changed": len(statements), "drafts_changed": len(drafts),
              "notes": dict(notes), "draft_notes": dict(draft_notes),
              "changed_drafts": draft_ids, "changed_clips": changed}
    if old_bin and new_bin:
        report["parity"] = parity(old_bin, new_bin, pairs)
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    return report


def parity(old_bin, new_bin, pairs):
    """Play each clip's old graph on the old build and its new graph on the
    new build, on both stand-in rigs; the largest difference per clip."""
    v3 = cm.v3
    worst, errors = {}, []
    for rig, cells in v3.RIGS.items():
        before = v3.play(old_bin, [old for _, old, _ in pairs], cells)
        after = v3.play(new_bin, [new for _, _, new in pairs], cells)
        for (clip_id, _, _), a, b in zip(pairs, before, after):
            if "error" in a or "error" in b:
                errors.append({"id": clip_id, "rig": rig, "old": a.get("error"),
                               "new": b.get("error")})
                continue
            x, y = v3.v2.values(a["ok"]), v3.v2.values(b["ok"])
            gap = max((abs(p - q) for p, q in zip(x, y)), default=0.) if len(x) == len(y) \
                else float("inf")
            worst[clip_id] = max(worst.get(clip_id, 0.), gap)
    exact = sum(1 for gap in worst.values() if gap <= 1e-6)
    return {"compared": len(worst), "exact": exact,
            "different": {k: v for k, v in worst.items() if v > 1e-6}, "errors": errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database")
    parser.add_argument("--out")
    parser.add_argument("--old", help="clip_graph_parity built before the change")
    parser.add_argument("--new", help="clip_graph_parity built from this checkout")
    parser.add_argument("--presets", help="rewrite this presets.json in place")
    args = parser.parse_args()
    if args.presets:
        print(json.dumps(rewrite_presets(args.presets), indent=1))
    if args.database:
        print(json.dumps(run(args.database, args.out, args.old, args.new), indent=1))


if __name__ == "__main__":
    main()
