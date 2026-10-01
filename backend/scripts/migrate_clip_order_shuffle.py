#!/usr/bin/env python3
"""Order without shuffle becomes a shuffled order (data cleanup 2026-10-01).

    migrate_clip_order_shuffle.py --database <copy of luma.db> --out <dir>
        [--new <v3 clip_graph_parity>]

`space(kind="order")` used to rank heads by fixture id and head number. It
now sorts them along the direction. Julian decided that every clip whose
order space read all heads with no shuffle becomes random, as the other
order clips are: a new `shuffle` node (no inputs: one random order for the
clip, from the clip seed) feeds the space's heads.

    space1 = space(kind="order")
    →  shuffle1 = shuffle()
       space1 = space(heads=shuffle1, kind="order")

A `time` wire is not added: in these clips the time nodes read the order
space themselves (a phase fan), so a clock on the shuffle would make a loop,
and one order per clip keeps the fan still as before.

Rewritten only when an order space has no heads wire. An order space whose
heads wire has no shuffle on it is left alone and counted (none on
2026-10-01). The new node takes the first free id `shuffle<n>`; every other
node, id and value stays, and key order is kept. Idempotent: a rewritten
space has a heads wire.

Writes <dir>/clips.sql and <dir>/drafts.sql (`-- expected: N`, guarded
UPDATEs for `clip_graphs_apply`) and <dir>/report.json. With --new it runs
the checker on every rewritten clip. Opens the database read-only.
"""
import argparse
import collections
import copy
import json
import pathlib
import sqlite3
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_front as front  # noqa: E402
from migrate_clip_graphs import compact, quote  # noqa: E402


def is_order(node):
    return node.get("kind") == "space" and (node.get("settings") or {}).get("kind") == "order"


def shuffled(nodes, heads):
    """Whether the heads wire `heads` passes through a shuffle."""
    seen = set()
    while isinstance(heads, dict) and "node" in heads and heads["node"] not in seen:
        seen.add(heads["node"])
        node = nodes.get(heads["node"], {})
        if node.get("kind") == "shuffle":
            return True
        heads = node.get("inputs", {}).get("heads")
    return False


def free_id(nodes):
    n = 1
    while f"shuffle{n}" in nodes:
        n += 1
    return f"shuffle{n}"


def rewrite(graph, notes):
    """The graph with a shuffle before every order space that reads all
    heads; the same graph when there is none."""
    nodes = graph.get("nodes", {})
    targets = []
    for node_id, node in nodes.items():
        if not is_order(node):
            continue
        heads = node.get("inputs", {}).get("heads")
        if heads is None:
            targets.append(node_id)
        elif not shuffled(nodes, heads):
            notes["left alone: order over a heads wire with no shuffle"] += 1
    if not targets:
        return graph
    new = copy.deepcopy(graph)
    for node_id in targets:
        shuffle = free_id(new["nodes"])
        new["nodes"][shuffle] = {"kind": "shuffle"}
        space = new["nodes"][node_id]
        space["inputs"] = {"heads": {"node": shuffle}, **space.get("inputs", {})}
        notes["shuffle added"] += 1
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


def run(database, out, new_bin=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    rows = db.execute("SELECT id, name, start, duration, seed, blend_mode, graph_json "
                      "FROM clips ORDER BY id").fetchall()
    notes, statements, changed, rewritten = collections.Counter(), [], [], []
    for row in rows:
        old = json.loads(row["graph_json"])
        new = rewrite(old, notes)
        if new != old:
            statements.append(f"UPDATE clips SET graph_json = {quote(compact(new))} "
                              f"WHERE id = {quote(row['id'])} "
                              f"AND graph_json = {quote(row['graph_json'])};")
            changed.append(f"{row['id']} {row['name']!r}")
            rewritten.append(front.clip_json(row, new))
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
              "changed_clips": changed, "changed_drafts": draft_ids}
    if new_bin:
        errors = collections.Counter()
        for result in front.v3.v2.evaluate(new_bin, rewritten, "--check"):
            if "error" in result:
                errors[result["error"][:90]] += 1
        report["checker_errors"] = dict(errors)
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--new", help="clip_graph_parity built from this checkout")
    args = parser.parse_args()
    print(json.dumps(run(args.database, args.out, args.new), indent=1))


if __name__ == "__main__":
    main()
