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
- `time.delay` turns with a clock become beats: × the clock's duration
  (duration, else every). With no clock the delay meant a share of the
  clip, so it must stretch with the clip: `curve(time(delay=D(s)), S)`
  with D a straight line over a space s becomes S over that space shifted
  by a curve over `time()` (`over_the_clip`); any other delay with no
  clock is refused. `time.length` other than 1 is refused.
- `space.length` becomes `scale`.
- `mirror.offset` other than 0 is refused.
- A list input becomes one `*` math node (a list of numbers, their product).
- Alpha stays alpha (a list becomes a math node like any other). Alpha is
  opacity in version 3: alone a clip looks the same; over another clip a
  fade now shows the clip below (with --new, report `fades_over_clips`
  counts those clips).
- A wrapped space now tiles (decision 50): one with a number scale other
  than 0 and 1 takes scale 1 and each curve over it reads its points at
  x × scale, so it plays the same.
- A `line` space after a mirror whose normal is parallel to the space's
  direction now measures from the mirror plane over the unfolded span: its
  shift and scale are halved (exact when a head sits on the plane). With an
  empty normal or direction the space is kept as it is (`--best-fit-mirror
  keep`): that plays exactly on both stand-in rigs, halving does not.

The 2026-09-30 audit (spec section 0, "Audit"):

- Blend `lighten` becomes `max` (the same math); `value` becomes `replace`
  with alpha × brightness, its closest standard form.
- A phase of 0 is dropped: a phase now always wraps.
- On the clip's own rig (`clip_cells`, from its score's venue): a radial or
  angle space takes `at`, the place of its heads' centroid in their box
  (version 2 measured from the centroid; version 3 from `at`, default the
  middle). A radial space's shift and scale take its nearest head's share
  m of the largest distance (version 2 read (d − min) / (max − min);
  version 3 reads d / max): shift' = m' + shift·(1 − m), scale' =
  scale·(1 − m), m' = m, or m·(n − ½)/n on a wrapped ring of n units.
- On the clip's own rig, an empty direction (a line space) or normal (a
  mirror) whose old best fit, the rig's principal axis, is not a stage axis
  takes that axis as written: the best fit is now the stage axis the heads
  spread along most.

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


class Refused(Exception):
    """A graph this migration does not convert, with the reason."""


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def wire(value):
    return isinstance(value, dict) and "node" in value


def w(node_id):
    return {"node": node_id}


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


def delays_to_beats(nodes, notes):
    for node_id, node in list(nodes.items()):
        if node["kind"] != "time" or "delay" not in node.get("inputs", {}):
            continue
        clock = clock_of(nodes, node)
        if not clock:
            over_the_clip(nodes, node_id, notes)
            continue
        inputs = clock.get("inputs", {})
        duration = inputs.get("duration", inputs.get("every"))
        node["inputs"]["delay"] = times(nodes, node["inputs"]["delay"], duration, notes,
                                        consumed=False)
        notes.add("delay: turns to beats")


# A hold reversed is a jump at the segment's start; the named eases pair up.
REVERSED = {"ease-in": "ease-out", "ease-out": "ease-in", "sine-in": "sine-out",
            "sine-out": "sine-in", "ease-in-out": "ease-in-out",
            "sine-in-out": "sine-in-out", "linear": "linear"}


def reversed_shape(shape):
    """The curve u ↦ shape(1 − u): points mirrored and in reverse order,
    each ease turned around (e'(s) = 1 − e(1 − s))."""
    points = [list(p) for p in shape["points"]]
    out = []
    for i in range(len(points) - 1, 0, -1):
        x, v = points[i][:2]
        ease = points[i - 1][2] if len(points[i - 1]) > 2 else "linear"
        if ease == "hold":
            out += [[clean(1 - x), v], [clean(1 - x), points[i - 1][1]]]
            continue
        if isinstance(ease, list):
            x1, y1, x2, y2 = ease
            ease = [clean(1 - x2), clean(1 - y2), clean(1 - x1), clean(1 - y1)]
        else:
            ease = REVERSED[ease]
        out.append([clean(1 - x), v] if ease == "linear" else [clean(1 - x), v, ease])
    out.append([clean(1 - points[0][0]), points[0][1]])
    xs = [p[0] for p in out]
    if any(xs[i] == xs[i + 2] for i in range(len(xs) - 2)) or len(out) > 256:
        raise Refused("a delay over the clip whose reversed curve has three points at one x")
    return {"points": out}


def straight_delay(nodes, delay):
    """(c0, c1, space id) when `delay` (turns) is c0 + c1 · a for the raw
    coordinate a of a plain space: a two-point linear curve over a space
    with no shift, length or wrap."""
    if not wire(delay):
        raise Refused("a delay with no clock that is one number")
    curve_node = nodes[delay["node"]]
    inputs = curve_node.get("inputs", {})
    points = inputs.get("shape", {"points": [[0, 0], [1, 1]]})["points"]
    low, high = inputs.get("low", 0), inputs.get("high", 1)
    x = inputs.get("x")
    space = nodes[x["node"]] if wire(x) else None
    settings = (space or {}).get("settings", {})
    wraps = settings.get("wrap", "yes" if settings.get("kind") == "angle" else "no") == "yes"
    if curve_node["kind"] != "curve" or len(points) != 2 or len(points[0]) > 2 \
            or not (number(low) and number(high)) or not space or space["kind"] != "space" \
            or wraps or {"shift", "length"} & set(space.get("inputs", {})):
        raise Refused("a delay with no clock that is not a straight line over a plain space")
    (_, y0), (_, y1) = points
    return low + (high - low) * y0, (high - low) * (y1 - y0), x["node"]


def over_the_clip(nodes, time_id, notes):
    """A delay with no clock is a share of the clip: each head's clock is
    τ = p − D(a), p the clip's progress. With D = c0 + c1·a this is the
    space a shifted by a curve over time() and scaled by 1/|c1|:
    x = (a − shift(p))·|c1| is τ when c1 < 0 and 1 − τ when c1 > 0 (then
    each curve over the time reads its shape reversed). Plays the same, and
    stretches with the clip."""
    time_node = nodes[time_id]
    if set(time_node["inputs"]) - {"delay"}:
        raise Refused("a delay with no clock and a phase")
    c0, c1, space_id = straight_delay(nodes, time_node["inputs"]["delay"])
    if c1 == 0:
        raise Refused("a delay with no clock that is the same for every head")
    users = [i for i, n in nodes.items() for k, v in n.get("inputs", {}).items()
             if time_id in sources(v)]
    if any(nodes[i]["kind"] != "curve" or nodes[i]["inputs"].get("x") != w(time_id)
           for i in users) or any(k != "x" and time_id in sources(v) for i in users
                                  for k, v in nodes[i]["inputs"].items()):
        raise Refused("a delayed time with no clock read by something other than a curve's x")
    clip = next((i for i, n in nodes.items() if n["kind"] == "time" and not n.get("inputs")),
                None)
    if clip is None:
        clip = free_id(nodes, "time")
        nodes[clip] = {"kind": "time"}
    k = abs(c1)
    low, high = (c0 / k, (c0 - 1) / k) if c1 < 0 else (-(c0 + 1) / c1, -c0 / c1)
    shift_id = free_id(nodes, "curve")
    nodes[shift_id] = {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": w(clip), "shape": {"points": [[0, 0], [1, 1]]},
                                  "low": clean(low), "high": clean(high)}}
    old_space = nodes[space_id]
    space = copy.deepcopy(old_space)
    space["inputs"] = {**space.get("inputs", {}), "shift": w(shift_id), "length": clean(1 / k)}
    new_id = free_id(nodes, "space")
    nodes[new_id] = space
    for i in users:
        nodes[i]["inputs"]["x"] = w(new_id)
        if c1 > 0:
            nodes[i]["inputs"]["shape"] = reversed_shape(
                nodes[i]["inputs"].get("shape", {"points": [[0, 0], [1, 1]]}))
    for orphan in (time_id, time_node["inputs"]["delay"]["node"], space_id):
        if orphan in nodes and not readers(nodes, orphan):
            del nodes[orphan]
    notes.add("delay over the clip: a space shifted by time()")


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


def scaled_points(points, scale):
    """Curve points that read at x what `points` read at x / scale, for x
    in 0..1: each x times `scale`; below 1 the last value is held to 1,
    above 1 the curve is cut at 1 (its value there joins as the last
    point). A cut inside an eased segment is refused: it would change the
    ease."""
    out = []
    for i, point in enumerate(points):
        x = point[0] * scale
        if x <= 1 + 1e-9:
            out.append([clean(min(x, 1.0)), *point[1:]])
            continue
        before = points[i - 1]
        x0 = before[0] * scale
        if x0 >= 1 - 1e-9:
            break
        ease = before[2] if len(before) > 2 else "linear"
        if ease == "hold":
            value = before[1]
        elif ease == "linear":
            u = (1 - x0) / (x - x0)
            value = clean(before[1] + (point[1] - before[1]) * u)
        else:
            raise Refused(f"wrapped scale {scale}: the cut at x 1 is inside an eased segment")
        out.append([1, value])
        break
    if out[-1][0] < 1:
        out.append([1, out[-1][1]])
    if len(out[-1]) > 2:
        out[-1] = out[-1][:2]
    # At most two points share an x: a jump at 1 cut from a longer run.
    while len(out) > 2 and out[-3][0] == out[-1][0]:
        del out[-2]
    return out


def wrapped_scales_into_curves(nodes, notes):
    """A wrapped space now tiles: x = fract((a − shift) / scale), where
    version 2 read ((a − shift) mod 1) / scale. A wrapped space with a
    number scale other than 0 and 1 takes scale 1, and each curve over it
    reads its points at x × scale: the same light (decision 50)."""
    for space_id, node in nodes.items():
        settings = node.get("settings", {})
        kind = settings.get("kind", "line")
        if node["kind"] != "space" or settings.get("wrap", "yes" if kind == "angle" else "no") != "yes":
            continue
        scale = node.get("inputs", {}).get("scale", 1)
        if number(scale) and scale in (0, 1):
            continue
        if not number(scale):
            raise Refused("a wrapped space with a wired scale")
        for reader in nodes.values():
            for name, value in reader.get("inputs", {}).items():
                if space_id not in sources(value):
                    continue
                shape = reader.get("inputs", {}).get("shape")
                if reader["kind"] != "curve" or name != "x" or not isinstance(shape, dict) \
                        or "points" not in shape:
                    raise Refused(f"a wrapped space with scale read by {reader['kind']}.{name}")
                if reader.get("settings", {}).get("kind", "number") != "number":
                    raise Refused("a color curve over a wrapped space with scale")
                shape["points"] = scaled_points(shape["points"], scale)
        del node["inputs"]["scale"]
        notes.add("wrapped scale: into the curve points")


def convert(graph, best_fit="keep", cells=None, coordinate=None):
    """(version 3 graph, notes). Raises Refused. With the clip's `cells` and
    a `coordinate` reader, the rig-dependent audit steps run too."""
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
    delays_to_beats(nodes, notes)
    clocks_into_times(nodes, notes)
    lengths_to_scales(nodes)
    halve_mirrored_lines(nodes, notes, best_fit)
    wrapped_scales_into_curves(nodes, notes)
    zero_phases(nodes, notes)
    if cells is not None:
        fit_to_rig(nodes, cells, coordinate, notes)
    return {"version": VERSION, "nodes": nodes}, notes


def convert_clip(graph, blend_mode, best_fit="keep", cells=None, coordinate=None):
    """(version 3 graph, blend mode, notes) for one clip."""
    version = graph.get("version")
    graph, notes = convert(graph, best_fit, cells, coordinate)
    nodes = copy.deepcopy(graph["nodes"])
    if version != VERSION:
        ties_read_as_before(nodes, notes)
    blend_mode = blend(blend_mode, nodes, notes)
    return {**graph, "nodes": nodes}, blend_mode, notes


def convert_document(value, best_fit="keep"):
    """A score document (or any JSON) with every version 1 or 2 graph
    converted."""
    if isinstance(value, dict):
        graph = value.get("graph")
        if isinstance(graph, dict) and graph.get("version") in (1, 2) and "nodes" in graph:
            rest = {k: convert_document(v, best_fit) for k, v in value.items() if k != "graph"}
            graph, blend_mode, _ = convert_clip(graph, value.get("blend_mode"), best_fit)
            if "blend_mode" in value:
                rest["blend_mode"] = blend_mode
            return {**rest, "graph": graph}
        return {k: convert_document(v, best_fit) for k, v in value.items()}
    if isinstance(value, list):
        return [convert_document(v, best_fit) for v in value]
    return value


# ---- the 2026-09-30 audit ----

BLENDS = {"lighten": "max", "value": "replace"}


def blend(blend_mode, nodes, notes):
    """The version 3 blend mode for `blend_mode`; `value` (top light as
    its own opacity) becomes replace with alpha × brightness."""
    if blend_mode not in BLENDS:
        return blend_mode
    notes.add(f"blend: {blend_mode} → {BLENDS[blend_mode]}")
    if blend_mode == "value":
        out = output_node(nodes)
        if out["kind"] == "color":
            inputs = out.setdefault("inputs", {})
            alpha = times(nodes, inputs.get("alpha"), inputs.get("brightness", 1), notes,
                          consumed=False)
            if alpha != 1:
                inputs["alpha"] = alpha
    return BLENDS[blend_mode]


def output_node(nodes):
    return next(n for n in nodes.values() if n["kind"] in ("color", "aim", "strobe"))


# How far a jump moves so a head that sat exactly on it reads as before.
TIE = 1e-9


def ties_read_as_before(nodes, notes):
    """At a jump x read the larger value; now it reads the value after
    (audit 1). A falling jump moves a hair right (TIE), so x itself still
    reads the higher value before it. A falling jump at x 1 over a space
    cannot move past 1: the space's scale grows by TIE instead (every x
    reads a hair lower), and each rising jump inside 0–1 over that space
    moves left by twice that, so it still reads its higher value."""
    nudged = set()
    for node in nodes.values():
        shape = node.get("inputs", {}).get("shape")
        if node["kind"] != "curve" or not isinstance(shape, dict):
            continue
        points = shape["points"]
        out, moved = [], False
        for i, point in enumerate(points):
            out.append(list(point))
            if i == 0 or points[i - 1][0] != point[0] or points[i - 1][1] <= point[1]:
                continue
            x = point[0]
            if x >= 1:
                source = node["inputs"].get("x")
                space = nodes.get(source["node"]) if wire(source) else None
                if space and space["kind"] == "space" and \
                        space.get("settings", {}).get("wrap", "no") != "yes":
                    nudged.add(source["node"])
                continue
            later = points[i + 1][0] if i + 1 < len(points) else 1
            if later <= x + TIE:
                continue
            if x == 0 and i == 1:
                out[0:2] = [list(points[0]), [TIE, *points[0][1:]], [TIE, *point[1:]]]
            else:
                out[-2][0] = out[-1][0] = x + TIE
            moved = True
        if moved:
            shape["points"] = out
            notes.add("tie: a falling jump moved a hair right")
    for space_id in nudged:
        space = nodes[space_id]
        inputs = space.setdefault("inputs", {})
        inputs["scale"] = times(nodes, inputs.get("scale", 1), 1 + TIE, notes)
        notes.add("tie: a space's scale grew a hair for a falling jump at x 1")
        for node in nodes.values():
            shape = node.get("inputs", {}).get("shape")
            if node["kind"] != "curve" or node["inputs"].get("x") != w(space_id) \
                    or not isinstance(shape, dict):
                continue
            points = shape["points"]
            for i in range(1, len(points)):
                x = points[i][0]
                if 0 < x < 1 and points[i - 1][0] == x and points[i - 1][1] < points[i][1] \
                        and (i < 2 or points[i - 2][0] < x - 2 * TIE * x):
                    points[i - 1][0] = points[i][0] = x * (1 - 2 * TIE)


def zero_phases(nodes, notes):
    """A phase now always wraps; version 2 wrapped only a phase other
    than 0, so a phase of 0 goes."""
    for node in nodes.values():
        if node["kind"] == "time" and node.get("inputs", {}).get("phase") == 0:
            del node["inputs"]["phase"]
            notes.add("phase 0 dropped")


def spread(points, center):
    """Eigenvalues and eigenvectors (columns) of the points' spread, by
    the cyclic Jacobi rotations of clip_graph::heads (version 2's best fit)."""
    m = [[sum((p[a] - center[a]) * (p[b] - center[b]) for p in points) for b in range(3)]
         for a in range(3)]
    v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    for _ in range(32):
        if m[0][1] ** 2 + m[0][2] ** 2 + m[1][2] ** 2 < 1e-30:
            break
        for p, q in ((0, 1), (0, 2), (1, 2)):
            if abs(m[p][q]) < 1e-300:
                continue
            theta = (m[q][q] - m[p][p]) / (2.0 * m[p][q])
            t = math.copysign(1.0, theta) / (abs(theta) + math.sqrt(theta * theta + 1.0))
            t = 1.0 if theta == 0.0 else t
            c = 1.0 / math.sqrt(t * t + 1.0)
            sn = t * c
            nxt = [row[:] for row in m]
            for k in range(3):
                nxt[k][p] = c * m[k][p] - sn * m[k][q]
                nxt[k][q] = sn * m[k][p] + c * m[k][q]
            rotated = [row[:] for row in nxt]
            for k in range(3):
                nxt[p][k] = c * rotated[p][k] - sn * rotated[q][k]
                nxt[q][k] = sn * rotated[p][k] + c * rotated[q][k]
            m = nxt
            for row in v:
                a, b = row[p], row[q]
                row[p] = c * a - sn * b
                row[q] = sn * a + c * b
    return [m[0][0], m[1][1], m[2][2]], v


def centroid(points):
    return [sum(p[a] for p in points) / max(len(points), 1) for a in range(3)]


def old_best_fit(points):
    """Version 2's empty direction: the principal axis, its largest
    component positive. None for one spot."""
    values, vectors = spread(points, centroid(points))
    most = max(range(3), key=lambda i: (values[i], i))
    if values[most] <= 1e-12:
        return None
    axis = [vectors[a][most] for a in range(3)]
    scale = max(abs(c) for c in axis)
    axis = [c / scale for c in axis]
    length = math.sqrt(sum(c * c for c in axis))
    axis = [c / length for c in axis]
    dominant = max(range(3), key=lambda i: (abs(axis[i]), -i))
    return [-c for c in axis] if axis[dominant] < 0 else axis


def new_best_fit(points):
    c = centroid(points)
    spreads = [sum((p[a] - c[a]) ** 2 for p in points) for a in range(3)]
    most = max(spreads)
    if most <= 1e-12:
        return None
    axis = next(a for a in range(3) if spreads[a] >= most * (1 - 1e-9))
    return [1.0 if a == axis else 0.0 for a in range(3)]


def fixture_of(cell_id):
    return cell_id.rsplit(":", 1)[0] if ":" in cell_id else cell_id


def head_of(cell_id):
    tail = cell_id.rsplit(":", 1)[-1]
    return int(tail) if tail.isdigit() else math.inf


class Unmapped(Exception):
    """A heads chain this step cannot follow on a rig (a mirror)."""


def units(nodes, heads, cells):
    """The spans of a heads wire on `cells`: each its unit positions (a
    group's centroid), as clip_graph::heads makes them, and its cell ids."""
    chain = []
    while heads:
        node = nodes[heads["node"]]
        chain.append(node)
        heads = node.get("inputs", {}).get("heads")
    unit = {c["id"]: c["id"] for c in cells}
    span = {c["id"]: "" for c in cells}
    position = {c["id"]: c["uvz"] for c in cells}
    for node in reversed(chain):
        kind = node["kind"]
        if kind == "mirror":
            raise Unmapped("a mirror in the heads")
        if kind == "split":
            by = node.get("settings", {}).get("by", "fixture")
            for c in cells:
                first = min((d for d in cells if unit[d["id"]] == unit[c["id"]]),
                            key=lambda d: d["id"])
                span[c["id"]] += "|" + (fixture_of(first["id"]) if by == "fixture"
                                        else first["group"])
        elif kind == "group":
            size = node.get("inputs", {}).get("size")
            if size is not None and not number(size):
                raise Unmapped("a wired group size")
            members = collections.defaultdict(list)
            for c in cells:
                members[(span[c["id"]], fixture_of(c["id"]))].append(c)
            for key, group in members.items():
                group.sort(key=lambda c: (head_of(c["id"]), c["id"]))
                chunk = max(1, round(size)) if size is not None else len(group)
                for i in range(0, len(group), chunk):
                    part = group[i:i + chunk]
                    for c in part:
                        unit[c["id"]] = f"{key}#{i}"
        elif kind not in ("shuffle",):
            raise Unmapped(f"a {kind} in the heads")
    spans = collections.defaultdict(dict)
    for c in cells:
        spans[span[c["id"]]].setdefault(unit[c["id"]], []).append(c)
    return [([centroid([c["uvz"] for c in members]) for members in by_unit.values()],
             [c["id"] for members in by_unit.values() for c in members])
            for by_unit in spans.values()]


def box_place(points, point):
    out = []
    for a in range(3):
        lo, hi = min(p[a] for p in points), max(p[a] for p in points)
        out.append((point[a] - lo) / (hi - lo) if hi - lo > 1e-9 else 0.5)
    return out


def agree(values, tolerance=1e-6):
    return all(max(abs(x - y) for x, y in zip(v, values[0])) <= tolerance for v in values)


def fit_to_rig(nodes, cells, coordinate, notes):
    """The rig-dependent steps of the audit on the clip's own cells.
    `coordinate(nodes, node_id)` gives a space's raw place per cell id."""
    if not cells:
        return
    for node_id, node in list(nodes.items()):
        if node["kind"] not in ("space", "mirror"):
            continue
        inputs = node.get("inputs", {})
        kind = node.get("settings", {}).get("kind", "line")
        if node["kind"] == "space" and kind == "line" \
                and mirror_for(nodes, node) == "parallel":
            fit_mirrored_line(nodes, node_id, cells, coordinate, notes)
            continue
        try:
            spans = units(nodes, inputs.get("heads"), cells)
        except Unmapped as e:
            notes.add(f"rig: {node['kind']} {kind} left as is ({e})")
            continue
        ids = [i for _, i in spans if i]
        spans = [s for s, i in spans if i]
        if node["kind"] == "mirror" or kind == "line":
            key = "normal" if node["kind"] == "mirror" else "direction"
            if key in inputs:
                continue
            olds = [old_best_fit(s) for s in spans]
            news = [new_best_fit(s) for s in spans]
            pairs = [(o, n) for o, n in zip(olds, news) if o is not None]
            if all(max(abs(a - b) for a, b in zip(o, n)) <= 1e-9 for o, n in pairs):
                continue
            if not agree([o for o, _ in pairs], 1e-9):
                notes.add("rig: best fit differs per span (approximate)")
                continue
            node["inputs"] = {**inputs, key: [clean(c) for c in pairs[0][0]]}
            notes.add(f"rig: old best fit written ({node['kind']})")
            continue
        if kind not in ("radial", "angle") or "at" in inputs:
            continue
        places = [box_place(s, centroid(s)) for s in spans]
        if not agree(places):
            notes.add(f"rig: {kind} centroid differs per span (approximate)")
        at = [clean(sum(p[a] for p in places) / len(places)) for a in range(3)]
        if any(abs(c - 0.5) > 1e-9 for c in at):
            inputs = node["inputs"] = {**inputs, "at": at}
            notes.add(f"rig: {kind} centre at the old centroid")
        if kind != "radial":
            continue
        probe = copy.deepcopy(nodes)
        probe[node_id]["inputs"] = {k: v for k, v in inputs.items()
                                    if k not in ("shift", "scale")}
        probe[node_id]["settings"] = {**node.get("settings", {}), "wrap": "no"}
        # Nodes that only fed the shift or scale go, so the probe checks.
        while True:
            orphans = [i for i, n in probe.items()
                       if n["kind"] not in ("color", "aim", "strobe") and not readers(probe, i)
                       and i != node_id]
            if not orphans:
                break
            for i in orphans:
                del probe[i]
        place = coordinate(probe, node_id)
        # The nearest unit's share of the largest distance, per span.
        nearest = [min(place[i] for i in span_ids) for span_ids in ids]
        wraps = node.get("settings", {}).get("wrap") == "yes"
        shifts = [m * (len(s) - 0.5) / len(s) if wraps else m for m, s in zip(nearest, spans)]
        if not agree([[m, x] for m, x in zip(nearest, shifts)]):
            notes.add("rig: radial nearest head differs per span (approximate)")
        m = sum(nearest) / len(nearest)
        shifted = sum(shifts) / len(shifts)
        if m <= 1e-9:
            continue
        inputs = node["inputs"] = dict(inputs)
        shift = inputs.get("shift", 0)
        if number(shift):
            inputs["shift"] = clean(shifted + shift * (1 - m))
        else:
            curve_node = nodes[shift["node"]]
            ci = curve_node.get("inputs", {})
            if readers(nodes, shift["node"]) != 1 or curve_node["kind"] != "curve" \
                    or not all(number(ci.get(k, d)) for k, d in (("low", 0), ("high", 1))):
                notes.add("radial: shared or wired shift (approximate)")
                continue
            for k, d in (("low", 0), ("high", 1)):
                ci[k] = clean(shifted + ci.get(k, d) * (1 - m))
        inputs["scale"] = times(nodes, inputs.get("scale", 1), clean(1 - m), notes)
        notes.add("rig: radial over d / max")


def heads_chain(nodes, heads):
    chain = []
    while heads:
        chain.append(nodes[heads["node"]])
        heads = chain[-1].get("inputs", {}).get("heads")
    return chain


def fit_mirrored_line(nodes, node_id, cells, coordinate, notes):
    """A line space after a parallel mirror, fitted to the clip's rig.

    Version 2 read the folded heads from the nearest (`a` 0) to the
    farthest (`a` 1). Version 3 reads `a = d / R` from the plane, so the
    folded heads span `m..M` of it. halve_mirrored_lines assumed `m` 0 and
    `M` 0.5; on the rig, `x = (a − shift) / scale` keeps every head's old
    place with shift `m + s (M − m)` and scale `c (M − m)` (`s`, `c` the
    version 2 values, twice the halved ones)."""
    node = nodes[node_id]
    inputs = node["inputs"]
    if node.get("settings", {}).get("wrap") == "yes" or any(
            n["kind"] == "split" for n in heads_chain(nodes, inputs.get("heads"))):
        notes.add("rig: mirrored line kept halved (wrapped or split)")
        return
    probe = copy.deepcopy(nodes)
    probe[node_id]["inputs"] = {k: v for k, v in inputs.items() if k not in ("shift", "scale")}
    while True:
        orphans = [i for i, n in probe.items()
                   if n["kind"] not in ("color", "aim", "strobe") and not readers(probe, i)
                   and i != node_id]
        if not orphans:
            break
        for i in orphans:
            del probe[i]
    place = coordinate(probe, node_id)
    values = [place[c["id"]] for c in cells if c["id"] in place]
    if not values:
        return
    m, k = min(values), 2 * (max(values) - min(values))
    if abs(m) <= 1e-9 and abs(k - 1) <= 1e-9:
        return
    shift = inputs.get("shift", 0)
    if number(shift):
        shift = clean(m + k * shift)
    else:
        curve_node = nodes[shift["node"]]
        ci = curve_node.get("inputs", {})
        if readers(nodes, shift["node"]) != 1 or curve_node["kind"] != "curve" \
                or not all(number(ci.get(key, d)) for key, d in (("low", 0), ("high", 1))):
            notes.add("rig: mirrored line kept halved (shared or wired shift)")
            return
        for key, d in (("low", 0), ("high", 1)):
            ci[key] = clean(m + k * ci.get(key, d))
    inputs["shift"] = shift
    inputs["scale"] = times(nodes, inputs.get("scale", 1), clean(k), notes)
    notes.add("rig: mirrored line fitted to the folded span")


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

def coordinate_reader(new_bin):
    """`coordinate(nodes, node_id, cells)`: a space's raw place per cell id,
    from the version 3 build."""
    def read(nodes, node_id, cells):
        graph = {"version": VERSION, "nodes": nodes}
        result = v2.evaluate(new_bin, [{"graph": graph, "node": node_id, "cells": cells}],
                             "--coordinate")[0]
        if "error" in result:
            raise Refused(f"radial centre: {result['error'][:80]}")
        return result["ok"]
    return read


def run(database, out, old_bin=None, new_bin=None, best_fit="keep", cells_path=None):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    statements, refusals = [], collections.Counter()
    notes, growth = collections.Counter(), collections.Counter()
    examples, mirrored = collections.defaultdict(list), collections.defaultdict(list)
    pairs = []
    rigs = json.loads(pathlib.Path(cells_path).read_text()) if cells_path else None
    if rigs and not new_bin:
        raise SystemExit("--cells needs --new: the radial centre reads the version 3 build")
    reader = coordinate_reader(new_bin) if rigs else None
    cells_of = {}
    blends = collections.Counter()
    rows = db.execute("SELECT id, score_id, name, start, duration, seed, blend_mode, z_index, "
                      "selection_json, graph_json FROM clips ORDER BY id").fetchall()
    for row in rows:
        old = json.loads(row["graph_json"])
        cells = None
        if rigs is not None:
            domain = rigs["clips"].get(row["id"])
            cells = rigs["domains"][domain] if isinstance(domain, int) else []
            cells_of[row["id"]] = cells
        try:
            coordinate = (lambda nodes, node_id, cells=cells: reader(nodes, node_id, cells)) \
                if reader else None
            new, new_blend, why = convert_clip(old, row["blend_mode"], best_fit, cells, coordinate)
        except Refused as e:
            refusals[str(e)] += 1
            examples[f"refused: {e}"].append(row["id"])
            continue
        for n in why:
            notes[n] += 1
            if n.startswith("mirrored line"):
                mirrored[n].append(row["id"])
        growth[len(new["nodes"]) - len(old["nodes"])] += 1
        if new_blend != row["blend_mode"]:
            blends[f"{row['blend_mode']} → {new_blend}"] += 1
            examples[f"blend {row['blend_mode']}"].append(f"{row['id']} {row['name']!r}")
        if new != old or new_blend != row["blend_mode"]:
            statements.append(f"UPDATE clips SET graph_json = {quote(compact(new))}, "
                              f"blend_mode = {quote(new_blend)} "
                              f"WHERE id = {quote(row['id'])} AND graph_json = {quote(row['graph_json'])} "
                              f"AND blend_mode = {quote(row['blend_mode'])};")
        pairs.append((row, v2.clip_json(row, old),
                      {**v2.clip_json(row, new), "blend_mode": new_blend}))
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
              "blend_modes": dict(blends),
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
        report["fades_over_clips"] = fades_over_clips(new_bin, good, examples)
        if old_bin and rigs is not None:
            report.update(rig_parity(old_bin, new_bin, good, cells_of, examples))
        elif old_bin:
            report.update(parity(old_bin, new_bin, good, examples, best_fit))
    report["examples"] = {k: v[:5] for k, v in examples.items()}
    (out / "report.json").write_text(json.dumps(report, indent=1) + "\n")
    write_markdown(out / "report.md", report)
    return report


def output(graph):
    return next(n for n in graph["nodes"].values() if n["kind"] in ("color", "aim", "strobe"))


# Frames per clip on its own rig: evenly over the clip, between beats.
RIG_FRAMES = 32


def rig_beats(clip):
    step = clip["duration"] / RIG_FRAMES
    return [clip["start"] + (k + 0.5) * step for k in range(RIG_FRAMES)]


def events_of(graph):
    """('overlap', 'gap') flags: a time node whose events overlap (duration
    above every, or either wired) or leave gaps (duration below every)."""
    overlap = gap = False
    for node in graph["nodes"].values():
        inputs = node.get("inputs", {})
        if node["kind"] != "time" or "every" not in inputs:
            continue
        every, duration = inputs["every"], inputs.get("duration", inputs["every"])
        if not (number(every) and number(duration)):
            overlap = gap = True
        elif duration > every + 1e-9:
            overlap = True
        elif duration < every - 1e-9:
            gap = True
    return overlap, gap


def rig_parity(old_bin, new_bin, triples, cells_of, examples):
    """Plays each clip before and after on its own rig (its score's venue,
    its selection), RIG_FRAMES frames over the clip. Sorts the clips into
    exact, close and failed, and counts the clips whose look changes on
    purpose (spec section 0, audit): replace is the top light (2), events
    combine per channel (4), lighten and value go (5), gaps are transparent
    (6), audio reads the whole track (8)."""
    by_score = collections.defaultdict(list)
    for row, _, clip in triples:
        by_score[row["score_id"]].append((row, clip))

    def under(row, clip):
        kind = output(clip["graph"])["kind"]
        end = row["start"] + row["duration"]
        return [o for o, c in by_score[row["score_id"]]
                if o["id"] != row["id"] and o["z_index"] < row["z_index"]
                and output(c["graph"])["kind"] == kind
                and o["start"] < end and row["start"] < o["start"] + o["duration"]]

    classes, by_class = collections.Counter(), collections.defaultdict(list)
    expected = collections.Counter()
    expected_ids = collections.defaultdict(set)
    chunk = 16
    for i in range(0, len(triples), chunk):
        part = triples[i:i + chunk]

        def requests(k):
            return [dict(clip=t[k], cells=cells_of.get(t[0]["id"]) or CELLS, beats=rig_beats(t[k]))
                    for t in part]
        olds = v2.evaluate(old_bin, requests(1))
        news = v2.evaluate(new_bin, requests(2))
        for (row, _, clip), x, y in zip(part, olds, news):
            label = f"{row['id']} {row['name']!r}"
            overlap, gap = events_of(clip["graph"])
            below = under(row, clip)
            kind = output(clip["graph"])["kind"]
            if gap and below:
                expected_ids["6 gaps are transparent (over a clip)"].add(row["id"])
            if row["blend_mode"] in BLENDS:
                expected_ids["5 lighten and value go"].add(row["id"])
            if "error" in x:
                why = x["error"]
                if "analyzed track data" in why:
                    expected_ids["8 audio over the whole track (not played)"].add(row["id"])
                    classes["not played: audio"] += 1
                else:
                    classes["not played: " + why[:50]] += 1
                continue
            if "error" in y:
                classes["failed: new errors"] += 1
                by_class["failed"].append((math.inf, f"{label}: {y['error'][:60]}"))
                continue
            if kind == "color" and clip["blend_mode"] == "replace" and below and any(
                    1e-6 < o.get("dimmer", 1.0) < 1 - 1e-6
                    for frame in y["ok"] for o in frame.get("lighting", {}).get("value", {}).values()):
                expected_ids["2 replace is the top light (dim, over a clip)"].add(row["id"])
            big, mean = difference(x["ok"], y["ok"])
            if big <= EXACT:
                cls = "exact"
            elif overlap:
                cls = "changed: events combine per channel"
                expected_ids["4 events combine per channel"].add(row["id"])
            elif big <= CLOSE_MAX and mean <= CLOSE_MEAN:
                cls = "close"
            else:
                cls = "failed"
            classes[cls] += 1
            by_class[cls].append((big, f"{label} (max {big:.3g}, mean {mean:.3g})"))
    for item, ids in sorted(expected_ids.items()):
        expected[item] = len(ids)
        examples[f"expected {item}"] = sorted(ids)[:5]
    listed = {kind: [label for _, label in sorted(items, reverse=True)]
              for kind, items in by_class.items() if kind != "exact"}
    for kind, labels in listed.items():
        examples[f"parity {kind}"] = labels
    return {"parity": dict(classes), "expected_changes": dict(expected),
            "parity_clips": listed,
            "parity_rule": f"each clip on its own rig (its venue and selection), {RIG_FRAMES} "
                           f"frames between beats; exact: every light, strobe and aim number "
                           f"within {EXACT}; close: largest difference ≤ {CLOSE_MAX} and "
                           f"mean ≤ {CLOSE_MEAN}"}


def fades_over_clips(new_bin, triples, examples):
    """Clips whose alpha is below 1 while another clip of the same score
    and output kind lies under them (lower z, active at that beat): the
    only clips whose look changes when alpha becomes opacity. `any
    selection` ignores which heads each clip lights; `same selection`
    counts only a clip under it with the same selection or `all`."""
    by_score = collections.defaultdict(list)
    for row, _, clip in triples:
        by_score[row["score_id"]].append((row, clip))
    fading = [(row, clip) for row, _, clip in triples
              if output(clip["graph"])["kind"] != "aim"
              and output(clip["graph"]).get("inputs", {}).get("alpha", 1) != 1]
    frames = play(new_bin, [clip for _, clip in fading], CELLS)
    counts = collections.Counter()
    for (row, clip), result in zip(fading, frames):
        if "error" in result:
            continue
        beats = [beat for beat, frame in zip(v2.beats(clip), result["ok"])
                 if any(o.get("alpha", 1.0) < 1 - 1e-6
                        for o in frame.get("lighting", {}).get("value", {}).values())]
        if not beats:
            continue
        counts["alpha below 1"] += 1
        kind = output(clip["graph"])["kind"]
        under = [other for other, c in by_score[row["score_id"]]
                 if other["id"] != row["id"] and other["z_index"] < row["z_index"]
                 and output(c["graph"])["kind"] == kind
                 and any(other["start"] <= b < other["start"] + other["duration"] for b in beats)]
        if under:
            counts["over a clip, any selection"] += 1
            examples["fade over a clip"].append(f"{row['id']} {row['name']!r}")
        expression = lambda r: json.loads(r["selection_json"]).get("expression")  # noqa: E731
        if any(expression(o) == expression(row) or "all" in (expression(o), expression(row))
               for o in under):
            counts["over a clip, same selection or all"] += 1
    return dict(counts)


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
        retry.append((row, a, {**a, "graph": convert(a["graph"], best_fit)[0]}))
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
    for key in ("forms", "node_count_change", "refusals", "blend_modes", "checker_errors",
                "fades_over_clips", "parity", "expected_changes"):
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
    parser.add_argument("--cells", help="clip_cells output: each clip's own rig; the radial "
                        "centre and best fit are fitted to it and parity plays on it")
    parser.add_argument("--best-fit-mirror", choices=["halve", "keep"], default="keep",
                        help="a line space after a mirror with an empty normal: keep plays "
                             "exactly on both stand-in rigs (2026-09-30 dry run)")
    args = parser.parse_args()
    report = run(args.database, args.out, args.old, args.new, args.best_fit_mirror, args.cells)
    print(json.dumps({k: v for k, v in report.items()
                      if k not in ("examples", "mirrored_lines", "parity_clips")}, indent=1))


if __name__ == "__main__":
    main()
