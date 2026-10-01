#!/usr/bin/env python3
"""The stripe width of "Middle-out chase" moves from the space into the
curve (data cleanup 2026-10-01, one clip).

    migrate_clip_chase_stripe.py --database <copy of luma.db> --out <dir>
        [--new <v3 clip_graph_parity>]

The clip's space hides the stripe width in its scale (0.084859331449) and
its box curve is 1 on [0, 1). A line space with `at` 0 gives
x = (a − shift) / scale, so the box is lit when 0 ≤ a − shift < scale. The
clean form has no scale (x = a − shift) and a box that is 1 on [0, W) with
W = the old scale: `[[0, 0], [0, 1], [W, 1], [W, 0], [1, 0]]` (a curve's
last point must sit at x 1). A curve holds its end values outside its
points and reads the value after a jump, so the new box is 0 for x < 0 and
for x ≥ W, as before. Only a head exactly on an
edge can differ (float rounding of the divide).

Rewritten only when the clip has exactly the expected old form. Ids, names
and every other input stay. Idempotent: a rewritten clip has no scale.

Writes <dir>/clips.sql (`-- expected: N`, a guarded UPDATE for
`clip_graphs_apply`) and <dir>/report.json. With --new it plays the clip
before and after on both stand-in rigs (1/8 beat steps) with
migrate_clip_front's parity rule. Opens the database read-only.
"""
import argparse
import copy
import json
import pathlib
import sqlite3
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_front as front  # noqa: E402
from migrate_clip_graphs import compact, quote  # noqa: E402

NAME = "Middle-out chase"
BOX = [[0, 0], [0, 1], [1, 1], [1, 0]]


class LeftAlone(Exception):
    """The clip is not in the expected old form."""


def rewrite(graph):
    """The new graph, or LeftAlone with the reason."""
    nodes = graph.get("nodes", {})
    space, box = nodes.get("space1"), nodes.get("curve2")
    if not space or space.get("kind") != "space" or not box or box.get("kind") != "curve":
        raise LeftAlone("no space1 or curve2")
    settings, inputs = space.get("settings", {}), space.get("inputs", {})
    if settings.get("kind", "line") != "line" or settings.get("wrap", "no") != "no":
        raise LeftAlone("space1 not an unwrapped line")
    if inputs.get("at", 0) != 0:
        raise LeftAlone("space1 at not 0")
    scale = inputs.get("scale")
    if not front.number(scale) or not 0 < scale < 1:
        raise LeftAlone("space1 scale not a number in (0, 1)")
    if front.readers(nodes, "space1") != [("curve2", "x", None)]:
        raise LeftAlone("space1 read by more than curve2's x")
    if box.get("settings", {}).get("kind", "number") != "number" \
            or front.points(box) != BOX:
        raise LeftAlone("curve2 not the number box [0, 1)")
    new = copy.deepcopy(graph)
    del new["nodes"]["space1"]["inputs"]["scale"]
    shape = new["nodes"]["curve2"]["inputs"]["shape"]
    shape["points"] = [[0, 0], [0, 1], [scale, 1], [scale, 0], [1, 0]]
    return new


def run(database, out, new_bin=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    rows = db.execute("SELECT id, name, start, duration, seed, blend_mode, graph_json "
                      "FROM clips WHERE name = ?", (NAME,)).fetchall()
    if len(rows) != 1:
        raise SystemExit(f"expected one clip named {NAME!r}; found {len(rows)}")
    row = rows[0]
    old = json.loads(row["graph_json"])
    report = {"clip": row["id"]}
    try:
        new = rewrite(old)
    except LeftAlone as why:
        report["left alone"] = str(why)
        new = old
    statements = []
    if new != old:
        statements.append(f"UPDATE clips SET graph_json = {quote(compact(new))} "
                          f"WHERE id = {quote(row['id'])} "
                          f"AND graph_json = {quote(row['graph_json'])};")
        report["new space1"] = new["nodes"]["space1"]["inputs"]
        report["new curve2"] = new["nodes"]["curve2"]["inputs"]
    (out / "clips.sql").write_text("\n".join([f"-- expected: {len(statements)}", *statements]) + "\n")
    report["changed"] = len(statements)
    if new_bin and statements:
        a, b = front.clip_json(row, old), front.clip_json(row, new)
        examples = {}
        report.update(front.parity(new_bin, [(row, a, b)], {row["id"]}, examples))
        report["largest difference"] = {
            rig: front.v3.difference(*(r["ok"] for r in front.v3.play(new_bin, [a, b], cells)))[0]
            for rig, cells in front.v3.RIGS.items()}
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
