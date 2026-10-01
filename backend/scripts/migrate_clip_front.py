#!/usr/bin/env python3
"""A sweeping front as a front (graph version 3, data cleanup 2026-10-01).

    migrate_clip_front.py --database <copy of luma.db> --out <dir>
        [--new <v3 clip_graph_parity>] [--presets <presets.json>]

Migrated Slash-type clips fake a hard front with a wide window that slides
in from off the rig: a line space with a scale of about 2 whose shift is a
curve from about −2 to −0.97, read by a pill (`[[0, 0], [0, 1], [1, 1],
[1, 0]]`, 1 on [0, 1)). The window's back edge `s` never enters the rig
(every shift ≤ 0), so a head at `a` is lit when `a < s + scale`: only the
front `F = s + scale` counts. The clean form is the v3 Slash cut: the space
along the reversed direction (`a' = 1 − a`, scale and at empty), the shift
`1 − F`, and a step up (`[[0, 0], [0, 1], [1, 1]]`, a jump reads the value
after): lit when `a' ≥ 1 − F`, that is `a ≤ F`. For Slash the shift curve
keeps its points and goes from 1 to −0.03 (the front from 0 to 1.03):

    space1 = space(direction=d, shift=curve(t, P, low=-2, high=-0.97), scale=2.000000002)
    curve5 = curve(space1, [[0, 0], [0, 1], [1, 1], [1, 0]])
    →  space1 = space(direction=-d, shift=curve(t, P, low=1, high=-0.03))
       curve5 = curve(space1, [[0, 0], [0, 1], [1, 1]])

`a < F` and `a ≤ F` differ only for a head exactly on the front, so a
rewrite needs every point of the front (the curve's held and turning
values) off the rig (`F > 1` or `F < 0`) or within SNAP of 0 above it (the
old scale's 2e-9 over 2: the near corner is lit at the first instant, as
`a ≤ 0` lights it). New low and high are `1 − scale − low` and `1 − scale −
high`, rounded to 6 places when that moves them by at most SNAP.

Rewritten only when: the space is a line, not wrapped, `at` empty or 0, a
literal direction, a number scale ≥ 1, read only by the pill's x; the pill
is a number curve with no low or high (or 0 and 1); the shift is a number
curve read only by this space, with number low and high and eases that do
not overshoot. Every other wide window read by a pill is left as it is and
counted with its reason. Ids stay; no node is added or removed. Idempotent:
a rewritten graph has no wide window left.

Writes <dir>/clips.sql and <dir>/drafts.sql (`-- expected: N`, guarded
UPDATEs for `clip_graphs_apply`), <dir>/report.md and <dir>/report.json.
With --new it runs the checker on every clip and plays every clip before
and after on both stand-in rigs (1/8 beat steps); a rewritten clip must
play the same (every number within EXACT). Opens the database read-only.
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

v3 = cm.v3
number, wire = v3.number, v3.wire
readers, clip_json, without_audio = cm.readers, cm.clip_json, cm.without_audio

PILL = [[0, 0], [0, 1], [1, 1], [1, 0]]
STEP_UP = [[0, 0], [0, 1], [1, 1]]
SNAP = 1e-8
EXACT = 1e-9
NAMED_EASES = {"linear", "sine-in", "sine-out", "sine-in-out", "ease-in", "ease-out",
               "ease-in-out", "hold"}


class LeftAlone(Exception):
    """A wide window this cleanup does not rewrite, with the reason."""


def points(curve):
    shape = curve.get("inputs", {}).get("shape")
    return shape.get("points") if isinstance(shape, dict) else None


def is_pill(curve):
    p = points(curve)
    return curve.get("kind") == "curve" and isinstance(p, list) \
        and all(isinstance(q, list) and len(q) == 2 for q in p) and p == PILL


def wide_windows(nodes):
    """[(space id, pill id)]: a space with a number scale ≥ 1 and a wired
    shift, read by a pill."""
    out = []
    for sid, space in sorted(nodes.items()):
        inputs = space.get("inputs", {})
        scale = inputs.get("scale")
        if space.get("kind") != "space" or not number(scale) or scale < 1 \
                or not wire(inputs.get("shift")):
            continue
        for rid, name, _ in readers(nodes, sid):
            if name == "x" and is_pill(nodes[rid]):
                out.append((sid, rid))
    return out


def does_not_overshoot(point):
    if len(point) < 3:
        return True
    ease = point[2]
    if isinstance(ease, str):
        return ease in NAMED_EASES
    return isinstance(ease, list) and len(ease) == 4 and all(0 <= y <= 1 for y in ease[1::2])


def snapped(x):
    r = round(x, 6)
    r = 0.0 if r == 0 else r
    return r if abs(r - x) <= SNAP else x


def plan(nodes, sid, pid):
    """The new (space, pill, shift curve id, shift curve) or LeftAlone."""
    space, pill = nodes[sid], nodes[pid]
    settings, inputs = space.get("settings", {}), space.get("inputs", {})
    if settings.get("kind", "line") != "line":
        raise LeftAlone(f"space kind {settings.get('kind')}")
    if settings.get("wrap", "no") != "no":
        raise LeftAlone("wrapped space")
    if inputs.get("at", 0) != 0:
        raise LeftAlone("space at not 0")
    direction = inputs.get("direction")
    if not (isinstance(direction, list) and len(direction) == 3 and all(map(number, direction))
            and any(direction)):
        raise LeftAlone("direction not a literal vector")
    if readers(nodes, sid) != [(pid, "x", None)]:
        raise LeftAlone("space read by more than the pill")
    if pill.get("settings", {}).get("kind", "number") != "number" \
            or pill["inputs"].get("low", 0) != 0 or pill["inputs"].get("high", 1) != 1:
        raise LeftAlone("pill with low, high or another kind")
    cid = inputs["shift"]["node"]
    shift = nodes[cid]
    if shift.get("kind") != "curve" or shift.get("settings", {}).get("kind", "number") != "number":
        raise LeftAlone("shift not a number curve")
    if readers(nodes, cid) != [(sid, "shift", None)]:
        raise LeftAlone("shift curve read elsewhere")
    low, high = shift["inputs"].get("low", 0), shift["inputs"].get("high", 1)
    if not (number(low) and number(high)):
        raise LeftAlone("wired shift low or high")
    p = points(shift)
    if not p or not all(does_not_overshoot(q) for q in p):
        raise LeftAlone("shift curve may overshoot")
    scale = inputs["scale"]
    backs = [low + q[1] * (high - low) for q in p]
    if max(backs) > 0:
        raise LeftAlone("window's back edge enters the rig")
    for back in backs:
        front = back + scale
        if not (front > 1 or front < 0 or 0 < front <= SNAP):
            raise LeftAlone(f"front holds on the rig ({front:.6g})")
    new_space = copy.deepcopy(space)
    new_space["inputs"]["direction"] = [0 if v == 0 else -v for v in direction]
    del new_space["inputs"]["scale"]
    new_space["inputs"].pop("at", None)
    new_pill = copy.deepcopy(pill)
    new_pill["inputs"]["shape"] = {**pill["inputs"]["shape"], "points": copy.deepcopy(STEP_UP)}
    new_pill["inputs"].pop("low", None)
    new_pill["inputs"].pop("high", None)
    new_shift = copy.deepcopy(shift)
    new_shift["inputs"]["low"] = snapped(1 - scale - low)
    new_shift["inputs"]["high"] = snapped(1 - scale - high)
    return new_space, new_pill, cid, new_shift


def rewrite(graph):
    """(new graph, notes): every provable wide window as a front."""
    graph = copy.deepcopy(graph)
    nodes = graph.get("nodes", {})
    notes = collections.Counter()
    for sid, pid in wide_windows(nodes):
        try:
            new_space, new_pill, cid, new_shift = plan(nodes, sid, pid)
        except LeftAlone as why:
            notes[f"left alone: {why}"] += 1
            continue
        nodes[sid], nodes[pid], nodes[cid] = new_space, new_pill, new_shift
        notes["fronts rewritten"] += 1
        notes[f"front {round(1 - new_shift['inputs']['low'], 6):g} → "
              f"{round(1 - new_shift['inputs']['high'], 6):g}"] += 1
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


def run(database, out, new_bin=None, presets=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    rows = db.execute("SELECT id, name, start, duration, seed, blend_mode, graph_json "
                      "FROM clips ORDER BY id").fetchall()
    statements, notes, pairs, changed = [], collections.Counter(), [], set()
    examples = collections.defaultdict(list)
    for row in rows:
        old = json.loads(row["graph_json"])
        new, why = rewrite(old)
        notes.update(why)
        for k in why:
            if k.startswith("left alone"):
                examples[k].append(f"{row['id']} {row['name']!r}")
        if new != old:
            changed.add(row["id"])
            examples["changed"].append(f"{row['id']} {row['name']!r}")
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
              "forms": dict(notes), "draft_forms": dict(draft_notes)}
    if presets:
        document = json.loads(pathlib.Path(presets).read_text())
        preset_notes = collections.Counter()
        new = rewrite_document(document, preset_notes)
        report["presets_changed"] = sum(1 for a, b in zip(document["clips"], new["clips"]) if a != b)
        report["preset_forms"] = dict(preset_notes)
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
    lines = ["# A sweeping front as a front", ""]
    lines += [f"- {k}: {v}" for k, v in report.items() if not isinstance(v, dict)]
    for key, value in report.items():
        if isinstance(value, dict) and key != "examples":
            lines += ["", f"## {key}", ""] + ([f"- {k}: {v}" for k, v in value.items()] or ["- none"])
    lines += ["", "## examples", ""] + [f"- {k}: {', '.join(map(str, v))}"
                                         for k, v in report["examples"].items()]
    (out / "report.md").write_text("\n".join(lines) + "\n")
    return report


def parity(new_bin, pairs, changed, examples):
    """Plays every clip before and after on both stand-in rigs. A rewritten
    clip passes only when every number is the same (bit for bit counts as
    `identical`, within EXACT as `exact`). Audio nodes play as `time()`."""
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
                kind = "exact" if v3.difference(x["ok"], y["ok"])[0] <= EXACT else "failed"
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
                           f"1/8 beat steps; identical: every number equal; exact: within {EXACT}"}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--new", help="clip_graph_parity built from this checkout")
    parser.add_argument("--presets", help="presets.json to check for the pattern")
    args = parser.parse_args()
    report = run(args.database, args.out, args.new, args.presets)
    print(json.dumps({k: v for k, v in report.items() if k != "examples"}, indent=1))


if __name__ == "__main__":
    main()
