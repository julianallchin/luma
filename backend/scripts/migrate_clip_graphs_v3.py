#!/usr/bin/env python3
"""Graph version 3 (docs/specs/clip-graphs.md section 0).

    migrate_clip_graphs_v3.py --database <copy of luma.db> --out <dir>
        [--old <v1 clip_graph_parity>] [--new <v3 clip_graph_parity>]
        [--best-fit-mirror halve|keep]

Converts version 1 graphs (through migrate_clip_clocks.convert, spec 8.7)
and version 2 graphs to version 3:

- `clock` nodes go. Each time node that read clock k takes k's `every`
  (and `duration` when set) as its own inputs: the same numbers or the same
  wires. A shuffle's clock becomes `shuffle.time`: a time node of that clock
  with no delay or phase when there is one, else a new one. Time nodes with
  equal every/duration share their events, so shared clocks stay shared.
- `time.delay` turns become beats: × the clock's duration (duration, else
  every), or × the clip's duration with no clock. `time.length` other than
  1 is refused.
- `space.length` becomes `scale`.
- `mirror.offset` other than 0 is refused.
- A list input becomes one `*` math node (a list of numbers, their product).
- `color.alpha` folds into brightness and `strobe.alpha` into rate, so a
  saved clip looks the same with alpha as opacity. Alpha is left empty.
- A `line` space after a mirror whose normal is parallel to the space's
  direction now measures from the mirror plane over the unfolded span: its
  shift and scale are halved (exact when a head sits on the plane). With an
  empty normal or direction the space is kept as it is (`--best-fit-mirror
  keep`): that plays exactly on both stand-in rigs, halving does not.

A number times a wire scales the wired curve's low and high when that curve
has no other reader, or joins a `*` math node with no other reader; else it
adds one math node. Writes <dir>/clips.sql and <dir>/drafts.sql (`--
expected: N`, guarded UPDATEs for `clip_graphs_apply`), <dir>/report.md and
<dir>/report.json. With --new it runs the version 3 checker; with --old as
well it plays every clip before and after on stand-in rigs. Opens the
database read-only; never writes it or the network.
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
import migrate_clip_clocks as v2  # noqa: E402
from migrate_clip_graphs import compact, quote  # noqa: E402

VERSION = 3
# Inputs that took a list in version 1 and 2: number inputs with range 0–1.
LISTS = {("color", "brightness"), ("color", "alpha"), ("aim", "alpha"),
         ("strobe", "rate"), ("strobe", "alpha"), ("noise", "contrast")}
EMPTY = {("color", "brightness"): 1, ("strobe", "rate"): 0.5}


class Refused(Exception):
    """A graph this migration does not convert, with the reason."""


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def wire(value):
    return isinstance(value, dict) and "node" in value


def clean(x):
    return v2.clean(x)


# ---- graph helpers ----

def sources(value):
    """The node ids an input value reads."""
    if wire(value):
        yield value["node"]
    elif isinstance(value, list):
        for item in value:
            yield from sources(item)


def readers(nodes, node_id):
    """How many input values read `node_id` (a list item counts once)."""
    return sum(1 for node in nodes.values() for value in node.get("inputs", {}).values()
               for source in sources(value) if source == node_id)


def free_id(nodes, kind):
    n = 1
    while f"{kind}{n}" in nodes:
        n += 1
    return f"{kind}{n}"


def add_math(nodes, values, notes):
    node_id = free_id(nodes, "math")
    nodes[node_id] = {"kind": "math", "settings": {"op": "*"}, "inputs": {"values": values}}
    notes.add("math node added")
    return {"node": node_id}


def times(nodes, value, factor, notes, consumed=True):
    """`value × factor` as an input value, adding at most one node.

    `value` and `factor` are numbers or wires (`None` is empty and means 1).
    A number times a curve with no other reader scales that curve's low and
    high; a factor joins a `*` math node with no other reader. `consumed`
    false: a wired factor keeps another reader (a clock's duration), so its
    node is never changed."""
    if value is None:
        return factor
    if factor is None:
        return value
    if number(value) and number(factor):
        return clean(value * factor)
    if number(value):
        if not consumed:
            return add_math(nodes, [value, factor], notes)
        value, factor = factor, value
    if number(factor) and factor == 1:
        return value
    node = nodes[value["node"]]
    if readers(nodes, value["node"]) == 1:
        inputs = node.setdefault("inputs", {})
        if number(factor) and node["kind"] == "curve" and \
                node.get("settings", {}).get("kind", "number") == "number" and \
                all(number(inputs.get(k, d)) for k, d in (("low", 0), ("high", 1))):
            for k, default in (("low", 0), ("high", 1)):
                if k in inputs or default != 0:
                    inputs[k] = clean(inputs.get(k, default) * factor)
            notes.add("curve low/high scaled")
            return value
        if node["kind"] == "math" and node.get("settings", {}).get("op", "*") == "*":
            inputs["values"].append(factor)
            notes.add("joined a math node")
            return value
    return add_math(nodes, [value, factor], notes)


# ---- clocks (version 3 terms) ----

def clock_key(node):
    inputs = node.get("inputs", {})
    if "every" not in inputs:
        return None
    return json.dumps([inputs["every"], inputs.get("duration")], sort_keys=True)


def clocks(nodes, node_id, memo=None):
    """The version 3 clocks whose events `node_id`'s wire carries: one per
    distinct every/duration pair of the time nodes upstream."""
    memo = {} if memo is None else memo
    if node_id in memo:
        return memo[node_id]
    node = nodes[node_id]
    out = set()
    if node["kind"] == "time" and clock_key(node):
        out.add(clock_key(node))
    for value in node.get("inputs", {}).values():
        for source in sources(value):
            out |= clocks(nodes, source, memo)
    memo[node_id] = out
    return out


def carried(nodes, value):
    return set().union(*[clocks(nodes, s) for s in sources(value)]) if value is not None else set()


# ---- the steps ----

def refuse_unmapped(nodes):
    for node_id, node in nodes.items():
        inputs = node.get("inputs", {})
        if node["kind"] == "mirror" and "offset" in inputs:
            if inputs["offset"] != 0:
                raise Refused("a mirror offset other than 0")
            del inputs["offset"]
        if node["kind"] == "time" and "length" in inputs:
            if inputs["length"] != 1:
                raise Refused("a time length other than 1")
            del inputs["length"]
        if node["kind"] == "clock" and "every" not in inputs:
            raise Refused("a clock with no every")


def lists_to_math(nodes, notes):
    for node in list(nodes.values()):
        for name, value in list(node.get("inputs", {}).items()):
            if (node["kind"], name) not in LISTS or not isinstance(value, list):
                continue
            if all(number(item) for item in value):
                node["inputs"][name] = clean(math.prod(value))
                notes.add("list of numbers: product")
            else:
                node["inputs"][name] = add_math(nodes, list(value), notes)
                notes.add("list: math node")


def clock_of(nodes, node):
    clock = node.get("inputs", {}).get("clock")
    return nodes[clock["node"]] if clock else None


def delays_to_beats(nodes, clip_duration, notes):
    for node in list(nodes.values()):
        if node["kind"] != "time" or "delay" not in node.get("inputs", {}):
            continue
        clock = clock_of(nodes, node)
        if clock:
            inputs = clock.get("inputs", {})
            duration = inputs.get("duration", inputs.get("every"))
        elif clip_duration is None:
            raise Refused("a delay with no clock and no clip duration")
        else:
            duration = clip_duration
        node["inputs"]["delay"] = times(nodes, node["inputs"]["delay"], duration, notes,
                                        consumed=False)
        notes.add("delay: turns to beats")


def clocks_into_times(nodes, notes):
    clock_ids = [i for i, n in nodes.items() if n["kind"] == "clock"]
    for clock_id in clock_ids:
        given = {k: v for k, v in nodes[clock_id].get("inputs", {}).items()
                 if k in ("every", "duration")}
        for node in nodes.values():
            inputs = node.get("inputs", {})
            if node["kind"] == "time" and inputs.get("clock") == {"node": clock_id}:
                del inputs["clock"]
                node["inputs"] = {**copy.deepcopy(given), **inputs}
                notes.add("clock: into time inputs")
    for clock_id in clock_ids:
        given = {k: v for k, v in nodes[clock_id].get("inputs", {}).items()
                 if k in ("every", "duration")}
        for node in list(nodes.values()):
            inputs = node.get("inputs", {})
            if node["kind"] != "shuffle" or inputs.get("clock") != {"node": clock_id}:
                continue
            del inputs["clock"]
            reuse = [i for i, n in nodes.items() if n["kind"] == "time"
                     and n.get("inputs", {}) == given]
            if reuse:
                time_id = reuse[0]
                notes.add("shuffle: an existing time")
            else:
                time_id = free_id(nodes, "time")
                nodes[time_id] = {"kind": "time", "inputs": copy.deepcopy(given)}
                notes.add("shuffle: a new time")
            inputs["time"] = {"node": time_id}
    for clock_id in clock_ids:
        if readers(nodes, clock_id):
            raise Refused(f"{clock_id} is read by something other than time or shuffle")
        del nodes[clock_id]


def lengths_to_scales(nodes):
    for node in nodes.values():
        if node["kind"] == "space" and "length" in node.get("inputs", {}):
            node["inputs"]["scale"] = node["inputs"].pop("length")


def unit(v):
    norm = math.sqrt(sum(c * c for c in v))
    return [c / norm for c in v]


def parallel(a, b):
    a, b = unit(a), unit(b)
    return abs(abs(sum(x * y for x, y in zip(a, b))) - 1) < 1e-9


def mirror_for(nodes, space):
    """'parallel', 'best fit' or None: whether `space` measures from a
    mirror plane in version 3."""
    direction = space.get("inputs", {}).get("direction")
    heads = space.get("inputs", {}).get("heads")
    while heads:
        node = nodes[heads["node"]]
        if node["kind"] == "mirror":
            normal = node.get("inputs", {}).get("normal")
            if isinstance(normal, list) and isinstance(direction, list):
                if parallel(normal, direction):
                    return "parallel"
            else:
                return "best fit"
        heads = node.get("inputs", {}).get("heads")
    return None


def halve_mirrored_lines(nodes, notes, best_fit):
    for node in list(nodes.values()):
        if node["kind"] != "space" or node.get("settings", {}).get("kind", "line") != "line":
            continue
        why = mirror_for(nodes, node)
        if why == "best fit" and best_fit == "keep":
            notes.add("mirrored line: kept (best fit)")
            continue
        if not why:
            continue
        inputs = node.setdefault("inputs", {})
        if "shift" in inputs:
            inputs["shift"] = times(nodes, inputs["shift"], 0.5, notes)
        inputs["scale"] = times(nodes, inputs.get("scale", 1), 0.5, notes)
        notes.add(f"mirrored line: halved ({why})")
        if node.get("settings", {}).get("wrap") == "yes":
            notes.add("mirrored line: wrapped")


def fold_alpha(nodes, notes):
    for node_id, node in list(nodes.items()):
        name = {"color": "brightness", "strobe": "rate"}.get(node["kind"])
        inputs = node.get("inputs", {})
        if not name or "alpha" not in inputs:
            continue
        alpha, value = inputs["alpha"], inputs.get(name)
        if wire(alpha) and wire(value):
            a, b = carried(nodes, alpha), carried(nodes, value)
            if a and b and a != b:
                raise Refused(f"{node_id}.{name} and alpha follow different clocks")
        # Alpha stays an input until its value has moved, so a wired alpha
        # still counts its one reader.
        folded = times(nodes, EMPTY[(node["kind"], name)] if value is None else value,
                       alpha, notes)
        del inputs["alpha"]
        if alpha != 1:
            inputs[name] = folded
            notes.add("alpha: folded")
    for node in nodes.values():
        if not node.get("inputs", True):
            del node["inputs"]


def convert(graph, clip_duration=None, best_fit="keep"):
    """(version 3 graph, notes). Raises Refused."""
    if graph.get("version") == VERSION:
        return graph, set()
    notes = set()
    if graph.get("version") == 1:
        try:
            graph, why = v2.convert(graph)
        except v2.Refused as e:
            raise Refused(str(e)) from e
        notes |= {f"v1→v2: {n}" for n in why}
    if graph.get("version") != 2:
        raise Refused(f"graph version {graph.get('version')}")
    nodes = copy.deepcopy(graph["nodes"])
    refuse_unmapped(nodes)
    lists_to_math(nodes, notes)
    delays_to_beats(nodes, clip_duration, notes)
    clocks_into_times(nodes, notes)
    lengths_to_scales(nodes)
    halve_mirrored_lines(nodes, notes, best_fit)
    fold_alpha(nodes, notes)
    return {"version": VERSION, "nodes": nodes}, notes


def convert_document(value, best_fit="keep"):
    """A score document (or any JSON) with every version 1 or 2 graph
    converted; a clip's duration comes from the clip holding the graph."""
    if isinstance(value, dict):
        graph = value.get("graph")
        if isinstance(graph, dict) and graph.get("version") in (1, 2) and "nodes" in graph:
            return {**{k: convert_document(v, best_fit) for k, v in value.items() if k != "graph"},
                    "graph": convert(graph, value.get("duration"), best_fit)[0]}
        return {k: convert_document(v, best_fit) for k, v in value.items()}
    if isinstance(value, list):
        return [convert_document(v, best_fit) for v in value]
    return value


# ---- parity ----

CELLS = v2.CELLS
# Five vertical bars of five heads, on the U–Z plane.
BARS = [dict(id=f"bar{c}:{r}", group="all", world=[c, 0, r], uvz=[c, 0, r])
        for c in range(5) for r in range(5)]
RIGS = {"cells": CELLS, "bars": BARS}
# A clip is close when its largest light difference is at most CLOSE_MAX and
# its mean difference at most CLOSE_MEAN (over every compared number).
EXACT, CLOSE_MAX, CLOSE_MEAN = 1e-6, 0.05, 0.01


def lights(frames):
    """Per frame and fixture: light (color × dimmer × alpha), strobe ×
    alpha, aim direction and weight, as a flat list of numbers."""
    out = []
    for frame in frames:
        heads = frame.get("lighting", {}).get("value", {})
        for head in sorted(heads):
            o = heads[head]
            alpha = o.get("alpha", 1.0)
            if "color" in o:
                out += [c * o.get("dimmer", 1.0) * alpha for c in o["color"]]
            elif "dimmer" in o:
                out += [o["dimmer"] * alpha] * 3
            if "strobe" in o:
                out.append(o["strobe"] * alpha)
            if "aim" in o:
                out += [*o["aim"]["direction"], o["aim"]["weight"]]
    return out


def difference(old, new):
    """(largest, mean) difference between two frame lists; inf when their
    shapes differ."""
    a, b = lights(old), lights(new)
    if len(a) != len(b):
        return math.inf, math.inf
    if not a:
        return 0.0, 0.0
    diffs = [abs(x - y) for x, y in zip(a, b)]
    return max(diffs), sum(diffs) / len(diffs)


def play(binary, clips, cells, chunk=200):
    out = []
    for i in range(0, len(clips), chunk):
        out += v2.evaluate(binary, [dict(clip=c, cells=cells, beats=v2.beats(c))
                                    for c in clips[i:i + chunk]])
    return out


# ---- the run ----

def run(database, out, old_bin=None, new_bin=None, best_fit="keep"):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    statements, refusals = [], collections.Counter()
    notes, growth = collections.Counter(), collections.Counter()
    examples, mirrored = collections.defaultdict(list), collections.defaultdict(list)
    pairs = []
    rows = db.execute("SELECT id, name, start, duration, seed, blend_mode, graph_json "
                      "FROM clips ORDER BY id").fetchall()
    for row in rows:
        old = json.loads(row["graph_json"])
        try:
            new, why = convert(old, row["duration"], best_fit)
        except Refused as e:
            refusals[str(e)] += 1
            examples[f"refused: {e}"].append(row["id"])
            continue
        for n in why:
            notes[n] += 1
            if n.startswith("mirrored line"):
                mirrored[n].append(row["id"])
        growth[len(new["nodes"]) - len(old["nodes"])] += 1
        if new != old:
            statements.append(f"UPDATE clips SET graph_json = {quote(compact(new))} "
                              f"WHERE id = {quote(row['id'])} AND graph_json = {quote(row['graph_json'])};")
        pairs.append((row, v2.clip_json(row, old), v2.clip_json(row, new)))
    (out / "clips.sql").write_text("\n".join([f"-- expected: {len(statements)}", *statements]) + "\n")
    drafts = []
    for row in db.execute("SELECT id, base_json, state_json FROM drafts ORDER BY id"):
        sets = []
        for column in ("base_json", "state_json"):
            document = json.loads(row[column])
            try:
                new = convert_document(document, best_fit)
            except Refused as e:
                refusals[f"draft: {e}"] += 1
                examples[f"refused draft: {e}"].append(row["id"])
                continue
            if new != document:
                sets.append(f"{column} = {quote(compact(new))}")
        if sets:
            drafts.append(f"UPDATE drafts SET {', '.join(sets)} WHERE id = {quote(row['id'])} "
                          f"AND base_json = {quote(row['base_json'])} "
                          f"AND state_json = {quote(row['state_json'])};")
    (out / "drafts.sql").write_text("\n".join([f"-- expected: {len(drafts)}", *drafts]) + "\n")
    report = {"clips": len(rows), "converted": len(pairs), "refused": sum(refusals.values()),
              "changed": len(statements), "drafts_changed": len(drafts),
              "best_fit_mirror": best_fit,
              "forms": dict(notes),
              "node_count_change": {str(k): v for k, v in sorted(growth.items())},
              "refusals": dict(refusals),
              "mirrored_lines": {k: v for k, v in mirrored.items()}}
    if new_bin:
        errors, good = collections.Counter(), []
        results = v2.evaluate(new_bin, [c for _, _, c in pairs], "--check")
        for (row, a, b), result in zip(pairs, results):
            if "error" in result:
                errors[result["error"].split(";")[0][:90]] += 1
                examples["check: " + result["error"][:90]].append(row["id"])
            else:
                good.append((row, a, b))
        report["checker_errors"] = dict(errors)
        if old_bin:
            report.update(parity(old_bin, new_bin, good, examples, best_fit))
    report["examples"] = {k: v[:5] for k, v in examples.items()}
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    write_markdown(out / "report.md", report)
    return report


def nudged(clip):
    """`clip` (version 1) with every number band width a hair narrower and
    its beats a hair later: a head that sat exactly on a band's end, or a
    beat exactly on a moving band's end, no longer does."""
    clip = copy.deepcopy(clip)
    for node in clip["graph"]["nodes"].values():
        inputs = node.get("inputs", {})
        if node["kind"] == "space" and number(inputs.get("width")):
            inputs["width"] *= 1 - 1e-9
    return clip


def worst_over_rigs(old_bin, new_bin, triples, unplayed, shift=0.0):
    """{id: (largest, mean)} over every rig; records clips that do not play."""
    worst = {row["id"]: (0.0, 0.0) for row, _, _ in triples}
    for rig, cells in RIGS.items():
        def requests(clips):
            return [dict(clip=c, cells=cells, beats=[t + shift for t in v2.beats(c)])
                    for c in clips]
        olds, news = [], []
        for i in range(0, len(triples), 200):
            chunk = triples[i:i + 200]
            olds += v2.evaluate(old_bin, requests([a for _, a, _ in chunk]))
            news += v2.evaluate(new_bin, requests([b for _, _, b in chunk]))
        for (row, _, _), x, y in zip(triples, olds, news):
            if "error" in x:
                unplayed.setdefault(row["id"], "not played: " + x["error"][:60])
            elif "error" in y:
                unplayed.setdefault(row["id"], "failed: new errors: " + y["error"][:60])
            else:
                big, mean = difference(x["ok"], y["ok"])
                old_big, old_mean = worst[row["id"]]
                worst[row["id"]] = (max(big, old_big), max(mean, old_mean))
    return worst


def parity(old_bin, new_bin, good, examples, best_fit="keep"):
    """Plays each clip before and after on every rig; a clip's class is its
    worst over the rigs. A version 1 clip that differs but plays exactly
    once nudged off band ends is "edge"."""
    unplayed = {}
    worst = worst_over_rigs(old_bin, new_bin, good, unplayed)
    again = [(row, a, b) for row, a, b in good
             if row["id"] not in unplayed and worst[row["id"]][0] > EXACT
             and a["graph"].get("version") == 1]
    retry = []
    for row, a, _ in again:
        a = nudged(a)
        retry.append((row, a, {**a, "graph": convert(a["graph"], a["duration"], best_fit)[0]}))
    edge = worst_over_rigs(old_bin, new_bin, retry, {}, shift=v2.NUDGE)
    classes, by_class = collections.Counter(), collections.defaultdict(list)
    for row, _, _ in good:
        big, mean = worst[row["id"]]
        if row["id"] in unplayed:
            kind = unplayed[row["id"]]
        elif big <= EXACT:
            kind = "exact"
        elif edge.get(row["id"], (math.inf,))[0] <= EXACT:
            kind = "edge"
        elif big <= CLOSE_MAX and mean <= CLOSE_MEAN:
            kind = "close"
        else:
            kind = "failed"
        classes[kind] += 1
        by_class[kind].append((big, f"{row['id']} {row['name']!r} (max {big:.3g}, mean {mean:.3g})"))
    listed = {kind: [label for _, label in sorted(items, reverse=True)]
              for kind, items in by_class.items() if kind != "exact"}
    for kind, labels in listed.items():
        examples[f"parity {kind}"] = labels
    return {"parity": dict(classes), "parity_clips": listed,
            "parity_rule": f"exact: every light, strobe and aim number within {EXACT}; "
                           "edge: differs, but exact once every band width is 1e-9 narrower "
                           "and every beat 1.4e-4 later (a head exactly on a band end); "
                           f"close: largest difference ≤ {CLOSE_MAX} and mean ≤ {CLOSE_MEAN}; "
                           f"rigs: {', '.join(f'{k} ({len(v)} heads)' for k, v in RIGS.items())}; "
                           "1/8 beat steps"}


def write_markdown(path, report):
    lines = ["# Graph version 3", ""]
    lines += [f"- {k}: {v}" for k, v in report.items() if not isinstance(v, dict)]
    for key in ("forms", "node_count_change", "refusals", "checker_errors", "parity"):
        if key not in report:
            continue
        lines += ["", f"## {key}", ""]
        items = sorted(report[key].items(), key=lambda kv: -kv[1]) if key != "node_count_change" \
            else report[key].items()
        lines += [f"- {k}: {v}" for k, v in items] or ["- none"]
    lines += ["", "## mirrored lines", ""]
    lines += [f"- {k} ({len(v)}): {', '.join(v)}" for k, v in report["mirrored_lines"].items()] \
        or ["- none"]
    lines += ["", "## examples", ""] + [f"- {k}: {', '.join(v)}" for k, v in report["examples"].items()]
    path.write_text("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--old", help="clip_graph_parity built from a version 1 checkout")
    parser.add_argument("--new", help="clip_graph_parity built from this checkout")
    parser.add_argument("--best-fit-mirror", choices=["halve", "keep"], default="keep",
                        help="a line space after a mirror with an empty normal: keep plays "
                             "exactly on both stand-in rigs (2026-09-30 dry run)")
    args = parser.parse_args()
    report = run(args.database, args.out, args.old, args.new, args.best_fit_mirror)
    print(json.dumps({k: v for k, v in report.items()
                      if k not in ("examples", "mirrored_lines", "parity_clips")}, indent=1))


if __name__ == "__main__":
    main()
