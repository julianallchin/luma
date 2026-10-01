#!/usr/bin/env python3
"""Products as one math node (graph version 3, data cleanup 2026-10-01).

    migrate_clip_math.py --database <copy of luma.db> --out <dir>
        [--new <v3 clip_graph_parity>] [--presets <presets.json>]

Saved clips still multiply the version 2 way: a number curve with low 0
(or empty) and a wired `high`, often in a chain (Slash: curve4 → curve5.high
→ curve6.high → brightness). The curve reads `low + v·(high − low)`, so with
low 0 it is `high × v`: the same as `high × curve(x, low=0, high=1)`. Each
such chain becomes one `*` math node with every factor (n-ary):

    brightness = curve6(high = curve5(high = curve4))
    →  brightness = math1(curve4, curve5, curve6)   (curve5, curve6 lose low and high)

A curve is rewritten only when it is a number curve, its low is empty or
the number 0, and its high is a wire. A link of the chain is absorbed into
the product only when the chain is its one reader: a curve of the same
form, or a `*` math node (its items join the product; it keeps its id for
the new node). Otherwise it stays one factor. The factors keep the order of
evaluation (the innermost first), so each light reads the same numbers
bit for bit: `v·h == h·v` and the math node multiplies left to right. A
single reader that is a `*` math node takes the product in place of the
curve when that keeps the order (the curve is its first or second item).
Ids stay; a new math node takes the absorbed math node's id, else the next
free `mathN` (a numbered id, so Python writes it inline as `a * b * c`).
Idempotent: a rewritten graph has no curve of this form left.

Writes <dir>/clips.sql and <dir>/drafts.sql (`-- expected: N`, guarded
UPDATEs for `clip_graphs_apply`), <dir>/report.md and <dir>/report.json.
With --new it runs the checker on every clip and plays every clip before
and after on both stand-in rigs (1/8 beat steps); a rewritten clip must
play exactly as before. Opens the database read-only; never writes it or
the network. With --presets it reports the presets it would rewrite and
writes <dir>/presets.json.
"""
import argparse
import collections
import copy
import json
import pathlib
import sqlite3
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_graphs_v3 as v3  # noqa: E402
from migrate_clip_graphs import compact, quote  # noqa: E402

number, wire, w = v3.number, v3.wire, v3.w


def readers(nodes, node_id):
    """[(reader id, input name, list index or None)] for every wire into
    `node_id`."""
    out = []
    for nid, node in nodes.items():
        for name, value in node.get("inputs", {}).items():
            if wire(value) and value["node"] == node_id:
                out.append((nid, name, None))
            elif isinstance(value, list):
                out += [(nid, name, i) for i, item in enumerate(value)
                        if wire(item) and item["node"] == node_id]
    return out


def scaled(node):
    """A number curve with low empty or 0 and a wired high: high × v."""
    if node.get("kind") != "curve":
        return False
    if node.get("settings", {}).get("kind", "number") != "number":
        return False
    inputs = node.get("inputs", {})
    low = inputs.get("low", 0)
    return number(low) and low == 0 and wire(inputs.get("high"))


def product(node):
    return node.get("kind") == "math" and node.get("settings", {}).get("op", "*") == "*" \
        and isinstance(node.get("inputs", {}).get("values"), list)


def only_reader(nodes, node_id, reader, name):
    return readers(nodes, node_id) == [(reader, name, None)]


def factors(nodes, top, absorbed, maths):
    """The product's items for the scaled curve `top`, innermost first.
    Records the curves and math nodes the product absorbs."""
    absorbed.append(top)
    high = nodes[top]["inputs"]["high"]["node"]
    if scaled(nodes[high]) and only_reader(nodes, high, top, "high"):
        inner = factors(nodes, high, absorbed, maths)
    elif product(nodes[high]) and only_reader(nodes, high, top, "high"):
        maths.append(high)
        inner = list(nodes[high]["inputs"]["values"])
    else:
        inner = [w(high)]
    return inner + [w(top)]


def is_top(nodes, node_id):
    """A scaled curve that no other scaled curve absorbs."""
    rs = readers(nodes, node_id)
    return not (len(rs) == 1 and rs[0][1] == "high" and scaled(nodes[rs[0][0]]))


def rewrite(graph):
    """(new graph, notes): every chain of scaled curves as one `*` math node."""
    graph = copy.deepcopy(graph)
    nodes = graph.get("nodes", {})
    notes = collections.Counter()
    while True:
        tops = sorted(i for i, n in nodes.items() if scaled(n) and is_top(nodes, i))
        if not tops:
            break
        top = tops[0]
        absorbed, maths = [], []
        items = factors(nodes, top, absorbed, maths)
        for curve in absorbed:
            inputs = nodes[curve]["inputs"]
            inputs.pop("low", None)
            inputs.pop("high", None)
        rs = readers(nodes, top)
        notes[f"product of {len(items)}"] += 1
        notes["curves rewritten"] += len(absorbed)
        if len(rs) == 1 and rs[0][2] in (0, 1) and product(nodes[rs[0][0]]):
            # The single reader is a product: splice the items in, first,
            # so the order of evaluation stays (a·(b·c) == (b·c)·a).
            reader, name, index = rs[0]
            values = nodes[reader]["inputs"][name]
            nodes[reader]["inputs"][name] = items + values[:index] + values[index + 1:]
            notes["spliced into a reader's math node"] += 1
            for m in maths:
                del nodes[m]
            notes["nodes removed"] += len(maths)
            continue
        math_id = maths[0] if maths else v3.free_id(nodes, "math")
        for m in maths:
            del nodes[m]
        notes["nodes removed"] += len(maths)
        nodes[math_id] = {"kind": "math", "settings": {"op": "*"}, "inputs": {"values": items}}
        if not maths:
            notes["math nodes added"] += 1
        for reader, name, index in rs:
            if index is None:
                nodes[reader]["inputs"][name] = w(math_id)
            else:
                nodes[reader]["inputs"][name][index] = w(math_id)
    return graph, notes


def rewrite_document(value, notes):
    """A score document (or any JSON) with every version 3 graph rewritten."""
    if isinstance(value, dict):
        graph = value.get("graph")
        if isinstance(graph, dict) and graph.get("version") == 3 and "nodes" in graph:
            new, why = rewrite(graph)
            notes.update(why)
            return {**{k: rewrite_document(v, notes) for k, v in value.items() if k != "graph"},
                    "graph": new}
        return {k: rewrite_document(v, notes) for k, v in value.items()}
    if isinstance(value, list):
        return [rewrite_document(v, notes) for v in value]
    return value


def clip_json(row, graph):
    return {"name": row["name"] or "clip", "start": row["start"], "duration": row["duration"],
            "seed": int(row["seed"]), "blend_mode": row["blend_mode"], "graph": graph}


def run(database, out, new_bin=None, presets=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    rows = db.execute("SELECT id, name, start, duration, seed, blend_mode, graph_json "
                      "FROM clips ORDER BY id").fetchall()
    statements, notes, pairs, changed = [], collections.Counter(), [], set()
    node_change = collections.Counter()
    for row in rows:
        old = json.loads(row["graph_json"])
        new, why = rewrite(old)
        notes.update(why)
        if new != old:
            changed.add(row["id"])
            node_change[len(new["nodes"]) - len(old["nodes"])] += 1
            statements.append(f"UPDATE clips SET graph_json = {quote(compact(new))} "
                              f"WHERE id = {quote(row['id'])} "
                              f"AND graph_json = {quote(row['graph_json'])};")
        pairs.append((row, clip_json(row, old), clip_json(row, new)))
    (out / "clips.sql").write_text("\n".join([f"-- expected: {len(statements)}", *statements]) + "\n")
    drafts, draft_notes = [], collections.Counter()
    for row in db.execute("SELECT id, base_json, state_json FROM drafts ORDER BY id"):
        sets = []
        for column in ("base_json", "state_json"):
            document = json.loads(row[column])
            new = rewrite_document(document, draft_notes)
            if new != document:
                sets.append(f"{column} = {quote(compact(new))}")
        if sets:
            drafts.append(f"UPDATE drafts SET {', '.join(sets)} WHERE id = {quote(row['id'])} "
                          f"AND base_json = {quote(row['base_json'])} "
                          f"AND state_json = {quote(row['state_json'])};")
    (out / "drafts.sql").write_text("\n".join([f"-- expected: {len(drafts)}", *drafts]) + "\n")
    report = {"clips": len(rows), "changed": len(statements), "drafts_changed": len(drafts),
              "forms": dict(notes), "draft_forms": dict(draft_notes),
              "node_count_change": {str(k): v for k, v in sorted(node_change.items())}}
    if presets:
        document = json.loads(pathlib.Path(presets).read_text())
        preset_notes = collections.Counter()
        new = rewrite_document(document, preset_notes)
        report["presets_changed"] = sum(1 for a, b in zip(document["clips"], new["clips"]) if a != b)
        report["preset_forms"] = dict(preset_notes)
        (out / "presets.json").write_text(json.dumps(new, indent=1) + "\n")
    examples = collections.defaultdict(list)
    for row, _, _ in pairs:
        if row["id"] in changed:
            examples["changed"].append(f"{row['id']} {row['name']!r}")
    if new_bin:
        errors = collections.Counter()
        for (row, _, clip), result in zip(pairs, v3.v2.evaluate(new_bin, [c for _, _, c in pairs],
                                                                 "--check")):
            if "error" in result:
                errors[result["error"][:90]] += 1
                examples["check: " + result["error"][:90]].append(row["id"])
        report["checker_errors"] = dict(errors)
        report.update(parity(new_bin, pairs, changed, examples))
    report["examples"] = {k: v[:5] for k, v in examples.items()}
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    lines = ["# Products as one math node", ""]
    lines += [f"- {k}: {v}" for k, v in report.items() if not isinstance(v, dict)]
    for key, value in report.items():
        if isinstance(value, dict) and key != "examples":
            lines += ["", f"## {key}", ""] + ([f"- {k}: {v}" for k, v in value.items()] or ["- none"])
    lines += ["", "## examples", ""] + [f"- {k}: {', '.join(map(str, v))}"
                                         for k, v in report["examples"].items()]
    (out / "report.md").write_text("\n".join(lines) + "\n")
    return report


def without_audio(clip):
    """`clip` with each audio node read as `time()` over the clip: the
    stand-in rigs have no analyzed track, and the rewrite never touches
    audio nodes."""
    clip = copy.deepcopy(clip)
    for node_id, node in clip["graph"]["nodes"].items():
        if node["kind"] == "audio":
            clip["graph"]["nodes"][node_id] = {"kind": "time"}
    return clip


def parity(new_bin, pairs, changed, examples):
    """Plays every clip before and after on both stand-in rigs. A rewritten
    clip passes only when every number is the same (bit for bit counts as
    `identical`, within v3.EXACT as `exact`). A clip that reads audio plays
    with each audio node as `time()`, before and after alike."""
    rank = {"identical": 0, "exact": 1, "not played (same error before and after)": 2,
            "failed": 3, "failed: errors differ": 3}
    classes = collections.Counter()
    worst = {}
    for cells in v3.RIGS.values():
        olds = v3.play(new_bin, [a for _, a, _ in pairs], cells)
        news = v3.play(new_bin, [b for _, _, b in pairs], cells)
        for (row, a, b), x, y in zip(pairs, olds, news):
            if "analyzed track data" in x.get("error", "") and x.get("error") == y.get("error"):
                x, y = (v3.play(new_bin, [without_audio(c)], cells)[0] for c in (a, b))
            if "error" in x or "error" in y:
                kind = "not played (same error before and after)" \
                    if x.get("error") == y.get("error") else "failed: errors differ"
            elif v3.lights(x["ok"]) == v3.lights(y["ok"]):
                kind = "identical"
            else:
                kind = "exact" if v3.difference(x["ok"], y["ok"])[0] <= v3.EXACT else "failed"
            if row["id"] not in worst or rank[kind] > rank[worst[row["id"]]]:
                worst[row["id"]] = kind
    for row, _, _ in pairs:
        group = "rewritten" if row["id"] in changed else "unchanged"
        kind = worst[row["id"]]
        classes[f"{group}: {kind}"] += 1
        if kind.startswith("failed"):
            examples[f"parity {group} {kind}"].append(f"{row['id']} {row['name']!r}")
    return {"parity": dict(classes),
            "parity_rule": "every clip before and after, both stand-in rigs "
                           f"({', '.join(f'{k} {len(v)} heads' for k, v in v3.RIGS.items())}), "
                           f"1/8 beat steps; identical: every number equal; exact: within {v3.EXACT}"}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--new", help="clip_graph_parity built from this checkout")
    parser.add_argument("--presets", help="presets.json to rewrite into <out>/presets.json")
    args = parser.parse_args()
    report = run(args.database, args.out, args.new, args.presets)
    print(json.dumps({k: v for k, v in report.items() if k != "examples"}, indent=1))


if __name__ == "__main__":
    main()
