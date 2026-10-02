#!/usr/bin/env python3
"""Noise period, counter-clockwise yaw, low band (data migration 2026-10-01).

    migrate_clip_period_yaw.py --database <copy of luma.db> --out <dir>
        [--old <clip_graph_parity before the change>]
        [--new <clip_graph_parity after the change>]

Three changes in one pass, so each clip plays exactly as before:

1. `noise.speed` is now `noise.period` (beats per turn). The input key is
   renamed in place; its value and key order stay.
2. Aim yaw turned clockwise seen from above. It now turns counter-clockwise,
   as the venue does (the right-hand rule about +z). Every yaw is negated:

    aim1.yaw = 20                          →  aim1.yaw = -20
    curve1(low=-25, high=25) → aim1.yaw    →  curve1(low=25, high=-25)
    curve1()                 → aim1.yaw    →  curve1(high=-1)
    curve1(low=lo, high=hi)  → aim1.yaw    →  curve1(low=hi, high=lo)
        (lo and hi value nodes that hold a and -a: the wires swap)

The walk goes up from each yaw input through the nodes that make its
value: a curve negates its low and high (an empty low is 0 and stays empty;
an empty high is 1 and becomes -1), or swaps them when they are wires to
value nodes that hold opposite numbers; a value node negates its value; a
`+` or `-` math node negates every item; a `max` or `min` math node
negates every item and becomes `min` or `max`; a `*` math node negates one
item (its first number, else its first wire). When a node on the walk also
feeds an input that is not on it, or the walk meets a node it cannot
negate, nothing changes in place: each wired yaw takes a new `math` node
(`*`, values [the old wire, -1]) and the note says so. Every other node, id
and value stays, and key order is kept.

3. An empty audio band was 40-100 Hz. It is now the low band (60-300 Hz).
   An audio node that leaves `low_hz` or `high_hz` empty gets the old
   value written in: `low_hz` 40, `high_hz` 100. Key order stays; a new
   key goes last.

Not idempotent: run it once, on graphs that still hold clockwise yaw. The
guarded UPDATEs refuse a row that changed since the read.

Writes <dir>/clips.sql and <dir>/drafts.sql (`-- expected: N`, guarded
UPDATEs for `clip_graphs_apply`, which queues them for PowerSync so
Supabase follows) and <dir>/report.json with every changed clip's score,
name and each value before and after. With --old and --new it plays every
changed clip before (old graph, old build) and after (new graph, new build)
and lists any difference. Opens the database read-only; never writes it or
the network. The shipped presets.json is already rewritten by hand.
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
import migrate_clip_phase_degrees as pd  # noqa: E402
from migrate_clip_graphs import compact, quote  # noqa: E402

is_number, wire, record = pd.is_number, pd.wire, pd.record
FLIP = {"max": "min", "min": "max"}
#: What an empty audio band input read before the low band.
OLD_EMPTY_BAND = {"low_hz": 40, "high_hz": 100}


def negate(value):
    """`-value`, without a negative zero."""
    return value if value == 0 else -value


def rename_period(nodes, notes, changes):
    """Rename every noise node's `speed` input to `period`, in place."""
    for node_id, node in nodes.items():
        inputs = node.get("inputs") or {}
        if node.get("kind") == "noise" and "speed" in inputs:
            node["inputs"] = {("period" if k == "speed" else k): v for k, v in inputs.items()}
            record(changes, f"{node_id}.speed", "speed", "period")
            notes["noise speed renamed to period"] += 1


def fill_band(nodes, notes, changes):
    """Write the old empty band into every audio node that leaves it empty."""
    for node_id, node in nodes.items():
        if node.get("kind") != "audio":
            continue
        inputs = node.setdefault("inputs", {})
        for name, hz in OLD_EMPTY_BAND.items():
            if name not in inputs:
                inputs[name] = hz
                record(changes, f"{node_id}.{name}", "empty", hz)
                notes[f"empty audio {name} set to {hz}"] += 1


def empty_band(nodes):
    return any(n.get("kind") == "audio" and not set(OLD_EMPTY_BAND) <= set(n.get("inputs") or {})
               for n in nodes.values())


def yaw_inputs(nodes):
    return [node_id for node_id, node in nodes.items()
            if node.get("kind") == "aim" and "yaw" in (node.get("inputs") or {})]


def opposite_values(nodes, inputs):
    """True when a curve's low and high are wires to value nodes that hold
    the numbers a and -a, a not 0."""
    held = []
    for bound in ("low", "high"):
        source = wire(inputs.get(bound))
        node = nodes.get(source) or {}
        value = (node.get("inputs") or {}).get("value")
        if node.get("kind") != "value" or not is_number(value):
            return False
        held.append(value)
    return held[0] != 0 and held[0] == -held[1]


def plan(nodes):
    """(edges, changed, swaps, blocked): the edges whose numbers the walk
    negates, as (node id, input, list index or None); the nodes it changes;
    the curves whose low and high wires swap; and the nodes it cannot
    negate."""
    edges, changed, swaps, blocked = [], [], [], []
    stack = [(a, "yaw", None) for a in yaw_inputs(nodes)]
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
        if source is None or source in seen_nodes:
            continue
        seen_nodes.add(source)
        if source not in nodes:
            blocked.append(source)
            continue
        changed.append(source)
        node = nodes[source]
        inputs = node.get("inputs") or {}
        kind = node.get("kind")
        if kind == "curve":
            if opposite_values(nodes, inputs):
                swaps.append(source)
            else:
                stack.extend((source, b, None) for b in ("low", "high") if b in inputs)
        elif kind == "value":
            stack.append((source, "value", None))
        elif kind == "math":
            items = inputs.get("values") or []
            if (node.get("settings") or {}).get("op", "*") == "*":
                pick = next((i for i, v in enumerate(items) if is_number(v)), None)
                if pick is None:
                    pick = next((i for i, v in enumerate(items) if wire(v)), None)
                picks = [] if pick is None else [pick]
            else:
                picks = range(len(items))
            stack.extend((source, "values", i) for i in picks)
        else:
            blocked.append(source)
    return edges, changed, swaps, blocked


def negate_through_products(nodes, notes, changes, why):
    """Each yaw negated by a new `*` math node; a number yaw is negated."""
    for aim_id in yaw_inputs(nodes):
        held = nodes[aim_id]["inputs"]["yaw"]
        if is_number(held):
            nodes[aim_id]["inputs"]["yaw"] = negate(held)
            record(changes, f"{aim_id}.yaw", held, negate(held))
            notes["yaw number"] += 1
            continue
        n = 1
        while f"math{n}" in nodes:
            n += 1
        nodes[f"math{n}"] = {"kind": "math", "settings": {"op": "*"},
                             "inputs": {"values": [held, -1]}}
        nodes[aim_id]["inputs"]["yaw"] = {"node": f"math{n}"}
        record(changes, f"{aim_id}.yaw", held, f"math{n} = {held.get('node')} * -1")
        notes[f"{why}: yaw through a new product with -1"] += 1


def rewrite(graph, notes, changes=None):
    """The graph with `period` for noise speed, every yaw negated and every
    empty audio band written in; the same graph when it has none of these.
    `changes` collects (where, before, after)."""
    nodes = graph.get("nodes", {})
    speeds = any(n.get("kind") == "noise" and "speed" in (n.get("inputs") or {})
                 for n in nodes.values())
    if not speeds and not yaw_inputs(nodes) and not empty_band(nodes):
        return graph
    new = copy.deepcopy(graph)
    nodes = new["nodes"]
    rename_period(nodes, notes, changes)
    fill_band(nodes, notes, changes)
    if not yaw_inputs(nodes):
        return new
    edges, changed, swaps, blocked = plan(nodes)
    if blocked:
        negate_through_products(nodes, notes, changes, "not negatable in place")
        return new
    if pd.shared(nodes, edges, changed):
        negate_through_products(nodes, notes, changes, "shared")
        return new
    for node_id, name, index in edges:
        inputs = nodes[node_id]["inputs"]
        held = inputs[name][index] if index is not None else inputs[name]
        if not is_number(held):
            continue
        where = f"{node_id}.{name}" + (f"[{index}]" if index is not None else "")
        if index is None:
            inputs[name] = negate(held)
        else:
            inputs[name][index] = negate(held)
        record(changes, where, held, negate(held))
        notes["yaw number" if nodes[node_id]["kind"] == "aim" else f"{nodes[node_id]['kind']} number"] += 1
    for node_id in changed:
        node = nodes[node_id]
        inputs = node.setdefault("inputs", {})
        if node_id in swaps:
            inputs["low"], inputs["high"] = inputs["high"], inputs["low"]
            record(changes, f"{node_id}.low/high", "low, high", "high, low")
            notes["curve low and high wires swapped"] += 1
        elif node.get("kind") == "curve" and "high" not in inputs:
            inputs["high"] = -1
            record(changes, f"{node_id}.high", "empty (1)", -1)
            notes["empty high set to -1"] += 1
        elif node.get("kind") == "math" and (node.get("settings") or {}).get("op") in FLIP:
            op = node["settings"]["op"]
            node["settings"]["op"] = FLIP[op]
            record(changes, f"{node_id}.op", op, FLIP[op])
            notes[f"math {op} became {FLIP[op]}"] += 1
    return new


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


def run(database, out, old_bin=None, new_bin=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    scores = pd.labels(db)
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
        report["parity"] = pd.parity(old_bin, new_bin, pairs)
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--old", help="clip_graph_parity built before the change")
    parser.add_argument("--new", help="clip_graph_parity built from this checkout")
    args = parser.parse_args()
    print(json.dumps(run(args.database, args.out, args.old, args.new), indent=1))


if __name__ == "__main__":
    main()
