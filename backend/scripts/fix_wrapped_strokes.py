#!/usr/bin/env python3
"""Data fix for decision 42 of docs/specs/clip-graphs.md.

    fix_wrapped_strokes.py --database <copy of luma.db> --out <file.sql>

The clip graphs converter mapped an old wrapped stroke whose middle ran the
whole axis once per event (offset ramp 0 → 1, boundary wrap) to
`space(wrap yes)` with an offset curve from −w/2 to 1 − w/2. At every event
start that stroke sat half on each end of the axis: a ghost half pill at the
far end. The converter now makes such a stroke enter and leave: wrap no, the
offset from −w to 1. This script finds migrated clips still in the old form
and writes that change as guarded updates for `clip_graphs_apply`:

    UPDATE clips SET graph_json = <new> WHERE id = <id> AND graph_json = <old>;

The first line is `-- expected: N`; the apply tool rolls back unless N rows
change. Opens the database read-only; never writes it.
"""
import argparse
import json
import pathlib
import sqlite3

RAMPS = ([[0, 0], [1, 1]], [[0, 1], [1, 0]])
TOLERANCE = 1e-9


def full_turn_offset(nodes, space):
    """The offset curve's node id when `space` is a wrapped line stroke whose
    offset runs −w/2 → 1 − w/2 (either way) over the event, else None."""
    settings = space.get("settings", {})
    if settings.get("kind", "line") != "line" or settings.get("wrap") != "yes":
        return None
    inputs = space.get("inputs", {})
    width, offset = inputs.get("width"), inputs.get("offset")
    if not isinstance(width, (int, float)) or not isinstance(offset, dict):
        return None
    curve = nodes.get(offset.get("node"), {})
    if curve.get("kind") != "curve":
        return None
    ci = curve.get("inputs", {})
    x = nodes.get((ci.get("x") or {}).get("node"), {})
    points = (ci.get("shape") or {}).get("points")
    if x.get("kind") != "time" or x.get("inputs", {}).get("phase") or points not in RAMPS:
        return None
    low, high = ci.get("low", 0.0), ci.get("high", 1.0)
    if not isinstance(low, (int, float)) or not isinstance(high, (int, float)):
        return None
    if abs(low + width / 2) > TOLERANCE or abs(high - 1 + width / 2) > TOLERANCE:
        return None
    return offset["node"]


def fix(graph):
    """The graph with every full-turn wrapped stroke entering and leaving,
    or None when it has none."""
    nodes = graph.get("nodes", {})
    changed = False
    for node_id, node in nodes.items():
        if node.get("kind") != "space":
            continue
        curve_id = full_turn_offset(nodes, node)
        if curve_id is None:
            continue
        users = [n for n in nodes.values()
                 if any(isinstance(v, dict) and v.get("node") == curve_id
                        for v in n.get("inputs", {}).values())]
        if len(users) != 1:
            raise SystemExit(f"{node_id}: offset curve {curve_id} feeds {len(users)} inputs")
        width = node["inputs"]["width"]
        node["settings"]["wrap"] = "no"
        nodes[curve_id]["inputs"]["low"] = -width
        nodes[curve_id]["inputs"]["high"] = 1.0
        changed = True
    return graph if changed else None


def quote(text):
    return "'" + text.replace("'", "''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    db = sqlite3.connect(f"file:{pathlib.Path(args.database).resolve()}?mode=ro", uri=True)
    statements = []
    for clip_id, text in db.execute("SELECT id, graph_json FROM clips ORDER BY id"):
        graph = fix(json.loads(text))
        if graph is not None:
            new = json.dumps(graph, separators=(",", ":"))
            statements.append(f"UPDATE clips SET graph_json = {quote(new)} "
                              f"WHERE id = {quote(clip_id)} AND graph_json = {quote(text)};")
    pathlib.Path(args.out).write_text(
        "\n".join([f"-- expected: {len(statements)}", *statements]) + "\n")
    print(f"{len(statements)} clips -> {args.out}")


if __name__ == "__main__":
    main()
