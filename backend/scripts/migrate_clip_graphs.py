#!/usr/bin/env python3
"""One-time conversion of form clips (color@1, aim@1, strobe.constant@1 with
source inputs) into clip graphs. See docs/specs/clip-graphs.md, section 8.

Never called while loading a score.

    migrate_clip_graphs.py --database <copy of luma.db> --out <dir>
        Reads clips and drafts from the copy (read only) and writes
        clips.sql, drafts.sql, report.md and names.csv.

    migrate_clip_graphs.py < score.json > converted.json
        JSON mode: converts every clip of a score document (or of any JSON
        value that contains clips). The parity tool uses this.

The run refuses (exit 1, no SQL written) when a clip hits a hard
unmappable case (section 8.4); report.md lists those clips.
"""
import argparse
import collections
import copy
import csv
import json
import math
import pathlib
import sqlite3
import sys

SCALARS = ("number", "proportion", "beats", "degrees", "position", "seconds")
KINDS = {"color@1": "color", "aim@1": "aim", "strobe.constant@1": "strobe"}
GRAIN = {"head": None, "fixture": 0, "clump2": 2, "clump4": 4, "clump8": 8}
PLANES = {"up_down": [0, 0, 1], "front_back": [0, -1, 0], "left_right": [1, 0, 0]}
LINES = {"u": [1, 0, 0], "v": [0, 1, 0], "z": [0, 0, 1]}
RAMP_UP = [[0, 0], [1, 1]]
ON = [[0, 1], [1, 1]]
DEFAULT_DIRECTION = [0, 0.766, -0.643]
EDGE = 4e-9  # see Clip.space: the old overrun stroke's open ends
# Approximate classes of section 8.4, as report keys.
FAN, BLOOM = "fan lean", "bloom lean"
NOISE_INDEPENDENT, NOISE_SPATIAL, NOISE = "independent noise", "spatial noise", "noise stream"
RELATIVE_VARYING = "relative width with a varying clock"
ASYMMETRIC = "non-monotone offset with an asymmetric shape"
OVERLAP = "overlap winners include color or alpha"
# Approximations the spec does not list; see the report's deviations.
VARYING_WIDTH = "varying width with an offset (offset taken at clip start)"
HOLD_FLIP = "reversed shape with a hold"
PHASE_WRAP = "phase wider than one turn (wrapped)"
COLOR_KEYS = "color keyframes blend in OKLab"
MAJOR_AXIS = "major axis becomes best-fit line"
FAN_GAIN_AT_START = "fan gain on an animated direction taken at clip start"
PATH = "direction path off a line (exact, nested vector curves)"
ORDER = "order axis reads (i + 0.5) / n (decision 6)"
WRAP_RING = "wrapped line or radial axis is a ring; its ends no longer meet (decision 41)"
FULL_TURN = "wrapped stroke that crosses once per event enters and leaves unwrapped (decision 42)"
RAMPS = ([[0, 0], [1, 1]], [[0, 1], [1, 0]])


class Unmappable(Exception):
    """A hard case of section 8.4: the converter refuses the run."""


def is_hard(why):
    return why.startswith("hard: ")


def hard(why):
    raise Unmappable(f"hard: {why}")


class Curve:
    """A curve node not yet written. `low` and `high` are numbers, vectors
    or curves, so gains and offsets can still fold into them."""

    def __init__(self, x, shape, low=0.0, high=1.0, kind="number", gradient=None):
        self.x, self.shape, self.low, self.high = x, shape, low, high
        self.kind, self.gradient = kind, gradient


def is_value(v):
    return isinstance(v, (int, float, list))


def affine(v, a, b=0.0):
    """v × a + b, folded into the curve's low and high."""
    if isinstance(v, (int, float)):
        return v * a + b
    if isinstance(v, list):
        return [x * a + b for x in v]
    if v.kind == "color":
        if b != 0:
            hard("an offset on a color")
        # OKLab lerp commutes with a uniform scale of linear light.
        g = copy.deepcopy(v.gradient)
        for stop in g["stops"]:
            stop["color"] = [c * a for c in stop["color"]]
        return Curve(v.x, v.shape, kind="color", gradient=g)
    return Curve(v.x, v.shape, affine(v.low, a, b), affine(v.high, a, b), v.kind)


def multiply(v, gain):
    if isinstance(gain, (int, float)):
        return affine(v, gain)
    if isinstance(v, (int, float)):
        return affine(gain, v)
    if isinstance(v, Curve) and v.kind == "color":
        hard("a gain source on a color gradient")
    if isinstance(v, list):
        hard("a gain source on a vector")
    if not (isinstance(v.low, (int, float)) and isinstance(v.high, (int, float))):
        hard("a gain source on a curve whose low or high is already a wire")
    if gain.kind != "number":
        hard("a gain source that is not a number")
    return Curve(v.x, v.shape, affine(gain, v.low), affine(gain, v.high), v.kind)


def at_start(v):
    """The value at the clip start: x is 0 for time, the first point."""
    if is_value(v):
        return v
    s = v.shape[0][1]
    low, high = at_start(v.low), at_start(v.high)
    return low + s * (high - low)


def normalize(values):
    lo, hi = min(values), max(values)
    return lo, hi, [(x - lo) / (hi - lo) if hi > lo else 0.0 for x in values]


MIRROR_EASE = {"sine-in": "sine-out", "sine-out": "sine-in", "ease-in": "ease-out",
               "ease-out": "ease-in", "sine-in-out": "sine-in-out",
               "ease-in-out": "ease-in-out", "linear": "linear"}


def flip(points):
    """The shape read backward: x → 1 − x, each ease time-reversed. A hold
    cannot be reversed exactly; the caller logs it."""
    out, holds = [], False
    n = len(points)
    for j in range(n):
        i = n - 1 - j  # new point j is old point i
        x, v = 1 - points[i][0], points[i][1]
        point = [0.0 if j == 0 else x, v]
        if j < n - 1:
            ease = points[i - 1][2] if len(points[i - 1]) > 2 else "linear"
            if isinstance(ease, list):
                x1, y1, x2, y2 = ease
                ease = [1 - x2, 1 - y2, 1 - x1, 1 - y1]
            elif ease == "hold":
                holds = True
            else:
                ease = MIRROR_EASE[ease]
            if ease != "linear":
                point.append(ease)
        out.append(point)
    out[-1][0] = 1.0
    return out, holds


NAMED_BEZIER = {"ease-in": [0.42, 0, 1, 1], "ease-out": [0, 0, 0.58, 1],
                "ease-in-out": [0.42, 0, 0.58, 1]}
SHIFTED_SINE = "shifted sine ease on a wide wrapped stroke"


def bezier_at(c, u):
    return [3 * (1 - u) ** 2 * u * c[0][k] + 3 * (1 - u) * u * u * c[1][k] + u ** 3
            for k in range(2)]


def solve_u(c, t):
    lo, hi = 0.0, 1.0
    for _ in range(80):
        u = (lo + hi) / 2
        lo, hi = (u, hi) if bezier_at(c, u)[0] < t else (lo, u)
    return (lo + hi) / 2


def sub_ease(ease, t0, t1, notes):
    """The part of an ease between segment shares t0 and t1, as its own
    ease, and the change done at t0 and t1. A cubic Bézier splits exactly
    (de Casteljau); a sine ease does not."""
    if ease in (None, "linear"):
        return "linear", t0, t1
    if ease == "hold":
        return "hold", 0.0, 0.0
    if isinstance(ease, str) and ease.startswith("sine"):
        if notes is not None:
            notes.add(SHIFTED_SINE)
        return ease, ease_share(ease, t0), ease_share(ease, t1)
    x1, y1, x2, y2 = NAMED_BEZIER.get(ease, ease) if isinstance(ease, str) else ease
    pts = [[0.0, 0.0], [x1, y1], [x2, y2], [1.0, 1.0]]

    def split(p, u):  # (left, right) control points at u
        a = [[p[i][k] + (p[i + 1][k] - p[i][k]) * u for k in range(2)] for i in range(3)]
        b = [[a[i][k] + (a[i + 1][k] - a[i][k]) * u for k in range(2)] for i in range(2)]
        m = [b[0][k] + (b[1][k] - b[0][k]) * u for k in range(2)]
        return [p[0], a[0], b[0], m], [m, b[1], a[2], p[3]]

    ctrl = [[x1, y1], [x2, y2]]
    u0, u1 = solve_u(ctrl, t0) if t0 > 0 else 0.0, solve_u(ctrl, t1) if t1 < 1 else 1.0
    part = split(pts, u0)[1] if u0 > 0 else pts
    if u1 < 1:
        part = split(part, (u1 - u0) / (1 - u0))[0]
    (ax, ay), (bx, by) = part[0], part[3]
    if by - ay <= 0 or bx - ax <= 0:
        return "linear", ay, by
    handles = [(part[1][0] - ax) / (bx - ax), (part[1][1] - ay) / (by - ay),
               (part[2][0] - ax) / (bx - ax), (part[2][1] - ay) / (by - ay)]
    return [min(1.0, max(0.0, h)) for h in handles], ay, by


def shift(points, start, length, notes=None):
    """The shape read from `start` on: new(x) = old(x + start) for x in
    0..length, then held. Each cut segment keeps its exact ease."""
    end = start + length
    out = []
    for a, b in zip(points, points[1:]):
        xa, xb = a[0], b[0]
        lo, hi = max(xa, start), min(xb, end)
        if hi <= lo and not (lo == hi == start and xb > start):
            continue
        ease = a[2] if len(a) > 2 else None
        t0, t1 = (lo - xa) / (xb - xa), (hi - xa) / (xb - xa)
        sub, s0, s1 = sub_ease(ease, t0, t1, notes)
        va = a[1] + (b[1] - a[1]) * s0
        point = [lo - start, va]
        if sub != "linear" and hi > lo:
            point.append(sub)
        if not out or point[0] > out[-1][0]:
            out.append(point)
        if hi == end:
            vb = b[1] if hi == xb else a[1] + (b[1] - a[1]) * s1
            if ease == "hold" and hi < xb:
                vb = a[1]
            out.append([length, vb])
            break
    if out[-1][0] < 1:
        out[-1] = out[-1][:2]
        out.append([1.0, out[-1][1]])
    out[0][0] = 0.0
    return out


def symmetric(points):
    xs = [p[0] for p in points]
    return all(abs(sample(points, x) - sample(points, 1 - x)) < 1e-9
               for x in xs + [1 - x for x in xs] + [i / 64 for i in range(65)])


def ease_share(ease, t):
    if ease in (None, "linear"):
        return t
    if ease == "hold":
        return 0.0
    if ease == "sine-in":
        return 1 - math.cos(math.pi / 2 * t)
    if ease == "sine-out":
        return math.sin(math.pi / 2 * t)
    if ease == "sine-in-out":
        return (1 - math.cos(math.pi * t)) / 2
    if isinstance(ease, str):
        ease = {"ease-in": [0.42, 0, 1, 1], "ease-out": [0, 0, 0.58, 1],
                "ease-in-out": [0.42, 0, 0.58, 1]}[ease]
    x1, y1, x2, y2 = ease
    lo, hi = 0.0, 1.0
    for _ in range(60):  # solve x(u) = t on the cubic
        u = (lo + hi) / 2
        x = 3 * (1 - u) ** 2 * u * x1 + 3 * (1 - u) * u * u * x2 + u ** 3
        lo, hi = (u, hi) if x < t else (lo, u)
    u = (lo + hi) / 2
    return 3 * (1 - u) ** 2 * u * y1 + 3 * (1 - u) * u * u * y2 + u ** 3


def sample(points, x):
    """A number curve at x (for symmetry and preview checks only)."""
    if x <= points[0][0]:
        return points[0][1]
    for a, b in zip(points, points[1:]):
        if x < b[0]:
            t = (x - a[0]) / (b[0] - a[0])
            return a[1] + (b[1] - a[1]) * ease_share(a[2] if len(a) > 2 else None, t)
    return points[-1][1]


def directions(points):
    """Old stroke direction per segment: a rising segment moves forward, a
    falling one backward, a flat one follows the whole curve's end − start."""
    overall = points[-1][1] < points[0][1]
    out = set()
    for a, b in zip(points, points[1:]):
        out.add(overall if b[1] == a[1] else b[1] < a[1])
    return out


class Graph:
    def __init__(self):
        self.nodes, self.count, self.memo = {}, collections.Counter(), {}
        self.clocks_of = {}  # node id → set of clock ids its output carries

    def add(self, kind, inputs=None, settings=None, share=True):
        inputs = {k: v for k, v in (inputs or {}).items() if v is not None}
        key = json.dumps([kind, inputs, settings], sort_keys=True)
        if share and key in self.memo:
            return self.memo[key]
        self.count[kind] += 1
        node_id = f"{kind}{self.count[kind]}"
        node = {"kind": kind}
        if settings:
            node["settings"] = settings
        node["inputs"] = inputs
        self.nodes[node_id] = node
        carried = set()
        for v in inputs.values():
            if isinstance(v, dict) and "node" in v:
                carried |= self.clocks_of.get(v["node"], set())
        if kind == "clock":
            carried = {node_id}
        self.clocks_of[node_id] = carried
        if share:
            self.memo[key] = node_id
        return node_id

    def wire(self, node_id):
        return {"node": node_id}

    def emit(self, v):
        """A value, or a wire to the curve node that gives it."""
        if v is None or is_value(v):
            return v
        inputs = {"x": self.wire(v.x), "shape": {"points": v.shape}}
        if v.kind == "color":
            inputs["gradient"] = v.gradient
        else:
            low, high = self.emit(v.low), self.emit(v.high)
            if low != 0 and low != [0, 0, 0] or v.kind == "vector":
                inputs["low"] = low
            if high != 1 or v.kind == "vector":
                inputs["high"] = high
        return self.wire(self.add("curve", inputs, {"kind": v.kind}, share=False))

    def clock_set(self, v):
        if v is None or is_value(v):
            return set()
        if isinstance(v, dict):
            return self.clocks_of.get(v["node"], set())
        return self.clocks_of.get(v.x, set()) | self.clock_set(v.low) | self.clock_set(v.high)


class Clip:
    """Converts one clip's inputs. `notes` collects approximate classes."""

    def __init__(self, form, inputs, duration):
        self.form, self.inputs, self.duration = form, inputs, duration
        self.g = Graph()
        self.notes = set()
        self.clocks = {}  # old source path → clock id (or None: once over the clip)

    # -- values ---------------------------------------------------------
    def number(self, value, path, clock):
        """A number input: a plain value or a source. `clock` is the
        enclosing clock that a nested source without events inherits."""
        kind, body = value["type"], value["value"]
        if kind in SCALARS:
            return float(body)
        if kind == "time":
            return self.time(body, path, clock, "number")
        if kind == "space":
            return self.space(body, path, clock, "number")
        if kind == "random":
            return self.random(body, path, clock)
        if kind == "noise":
            return self.noise(body, path, clock)
        if kind == "audio":
            return self.audio(body, path, clock)
        hard(f"{path}: a {kind} value where a number is expected")

    def vector(self, value, path):
        kind, body = value["type"], value["value"]
        if kind == "vector":
            return [float(x) for x in body]
        if kind == "time":
            return self.time(body, path, None, "vector")
        hard(f"{path}: a {kind} source on a vector")

    def color(self, value, path):
        kind, body = value["type"], value["value"]
        if kind == "color":
            return [float(x) for x in body]
        if kind == "time":
            return self.time(body, path, None, "color")
        if kind == "space":
            return self.space(body, path, None, "color")
        hard(f"{path}: a {kind} source on a color")

    # -- clocks (8.3) ---------------------------------------------------
    def events(self, events, path, inherited):
        if events is None:
            return inherited
        if "same_as" in events:
            leader = events["same_as"]
            self.leader(leader)
            return self.clocks[leader]
        if path in self.clocks:
            return self.clocks[path]
        every, life = events["every"], events.get("life")
        if life is not None and life["type"] in SCALARS and float(life["value"]) == 0:
            life = {"type": "beats", "value": self.duration}  # an old period 0 is the clip
        constant = every["type"] in SCALARS
        if constant and float(every["value"]) == 0:
            # Old "once with a life": one event. Every twice the clip keeps
            # float rounding on the last beat from starting a second one.
            clock = None if life is None else self.g.add("clock", {
                "every": 2 * self.duration,
                "duration": self.g.emit(self.number(life, f"{path}/life", None))}, share=False)
        else:
            if constant and float(every["value"]) < 0:
                hard(f"{path}: a negative every")
            clock = self.g.add("clock", {
                "every": self.g.emit(self.number(every, f"{path}/every", None)),
                "duration": None if life is None else
                self.g.emit(self.number(life, f"{path}/life", None))}, share=False)
        self.clocks[path] = clock
        return clock

    def leader(self, name):
        """Converts the clock of top-level input `name` (for same_as)."""
        if name in self.clocks:
            return
        value = self.inputs[name]
        body = value["value"]
        if value["type"] == "space" and "offset" in body:
            self.events(body["offset"]["value"].get("events"), f"{name}/offset", None)
            self.clocks[name] = self.clocks.get(f"{name}/offset")
        elif value["type"] in ("time", "random"):
            self.events(body.get("events"), name, None)
            self.clocks.setdefault(name, None)
        else:
            hard(f"{name} has no event clock for same_as")

    def spacing(self, events, path):
        """every / life at the clip start, for relative width."""
        if not events or "same_as" in events:
            return 1.0
        every = at_start(self.number(events["every"], f"{path}/every", None))
        life = events.get("life")
        life = every if life is None else at_start(self.number(life, f"{path}/life", None))
        varying = events["every"]["type"] not in SCALARS or (
            events.get("life") and events["life"]["type"] not in SCALARS)
        if varying:
            self.notes.add(RELATIVE_VARYING)
        if every == 0:
            return 1.0
        return every / life

    # -- sources --------------------------------------------------------
    def time_node(self, clock, phase):
        if phase is not None and phase != 0:
            phase = self.phase(phase)
        else:
            phase = None
        return self.g.add("time", {"clock": clock and self.g.wire(clock), "phase": phase})

    def phase(self, phase):
        """Old phase adds then wraps mod 1; the new input is 0–1 turns."""
        if isinstance(phase, float):
            return phase % 1.0 or (1.0 if phase else 0.0)
        low, high = phase.low, phase.high
        if not (isinstance(low, float) and isinstance(high, float)):
            hard("a phase whose range is a wire")
        a, b = min(low, high), max(low, high)
        k = -math.floor(a)
        if b + k <= 1:
            return self.g.emit(Curve(phase.x, phase.shape, low + k, high + k))
        # Wider than a turn: wrap a piecewise-linear shape into 0–1.
        pts = [(p[0], low + p[1] * (high - low)) for p in phase.shape]
        if any(len(p) > 2 and p[2] != "linear" for p in phase.shape[:-1]):
            hard("a phase wider than one turn on a curved shape")
        self.notes.add(PHASE_WRAP)
        eps = 1e-9
        out = []
        for (x0, v0), (x1, v1) in zip(pts, pts[1:]):
            # A falling run that starts on a whole turn starts from 1, not 0.
            start = 1.0 if v1 < v0 and v0 % 1.0 == 0 else v0 % 1.0
            if out and out[-1][0] == x0:
                out[-1][1] = start
            else:
                out.append([x0, start])
            lo, hi = sorted((v0, v1))
            for n in range(math.floor(lo) + 1, math.ceil(hi)):
                xc = x0 + (n - v0) / (v1 - v0) * (x1 - x0)
                before = xc - eps
                if before > out[-1][0]:
                    out.append([before, (v0 + (before - x0) / (x1 - x0) * (v1 - v0)) % 1.0])
                if xc > out[-1][0]:
                    out.append([xc, 0.0 if v1 > v0 else 1.0])
        (_, before), (x_end, end) = pts[-2], pts[-1]
        out.append([x_end, 1.0 if end > before and end % 1.0 == 0 else end % 1.0])
        return self.g.emit(Curve(phase.x, out, 0.0, 1.0))

    def time(self, body, path, inherited, kind):
        if "gradient" in body and kind != "color":
            hard(f"{path}: a gradient on a {kind}")
        clock = self.events(body.get("events"), path, inherited)
        phase = self.number(body["phase"], f"{path}/phase", clock) if "phase" in body else None
        gain = self.number(body["gain"], f"{path}/gain", clock) if "gain" in body else 1.0
        x = self.time_node(clock, phase)
        if "gradient" in body:
            if gain != 1.0:
                hard(f"{path}: a gain on a color gradient")
            return Curve(x, body["curve"]["points"], kind="color",
                         gradient=gradient(body["gradient"]))
        points = body["points"]
        values = [p[1] for p in points]
        numbers = [isinstance(v, (int, float)) for v in values]
        if any(numbers) and not all(numbers):
            hard(f"{path}: a time curve with both numbers and colors")
        eases = [p[2:] for p in points]
        if all(numbers):
            if kind != "number":
                hard(f"{path}: number keyframes on a {kind}")
            lo, hi, norm = normalize([float(v) for v in values])
            if lo == hi:
                if clock is None and phase is None:
                    return multiply(lo, gain)
                return multiply(Curve(x, RAMP_UP, lo, lo), gain)
            shape = [[p[0], n, *e] for p, n, e in zip(points, norm, eases)]
            return multiply(Curve(x, shape, lo, hi), gain)
        if kind == "number":
            hard(f"{path}: color keyframes on a number")
        if gain != 1.0:
            if kind == "color":
                hard(f"{path}: a gain on color keyframes")
            if not isinstance(gain, float):
                hard(f"{path}: a gain source on a vector")
        vectors = [[float(c) * (gain if kind == "vector" else 1.0) for c in v] for v in values]
        if kind == "vector":
            return self.vector_path(x, points, vectors, eases, path, clock is None and phase is None)
        if all(v == vectors[0] for v in vectors):
            if clock is None and phase is None:
                return vectors[0]
        else:
            self.notes.add(COLOR_KEYS)
        n = len(points)
        shape = [[p[0], i / (n - 1), *e] for i, (p, e) in enumerate(zip(points, eases))]
        stops = [{"t": i / (n - 1), "color": c} for i, c in enumerate(vectors)]
        return Curve(x, shape, kind="color", gradient={"stops": stops})

    def vector_path(self, x, points, vectors, eases, path, constant_ok):
        first = vectors[0]
        far = max(vectors, key=lambda v: dist(v, first))
        if dist(far, first) == 0:
            if constant_ok:
                return first
            return Curve(x, RAMP_UP, first, first, "vector")
        axis = [b - a for a, b in zip(first, far)]
        length2 = sum(c * c for c in axis)
        ts = [sum((v[i] - first[i]) * axis[i] for i in range(3)) / length2 for v in vectors]
        for v, t in zip(vectors, ts):
            on_line = [first[i] + t * axis[i] for i in range(3)]
            if dist(on_line, v) > 1e-9 * max(1.0, math.sqrt(length2)):
                self.notes.add(PATH)
                return self.vector_steps(x, [p[0] for p in points], vectors, eases)
        lo, hi, norm = normalize(ts)
        low = [first[i] + lo * axis[i] for i in range(3)]
        high = [first[i] + hi * axis[i] for i in range(3)]
        shape = [[p[0], n, *e] for p, n, e in zip(points, norm, eases)]
        return Curve(x, shape, low, high, "vector")

    def vector_steps(self, x, xs, vectors, eases):
        """A path off a line, exactly. Its moving parts are cut into runs
        whose points lie on one line; each run is one vector curve that
        holds its end values outside it. Runs join by curves that step
        from 0 to 1 where the next run starts: before the step the earlier
        runs show (holding their last point), after it the later ones
        (holding their first point, which is the same point)."""
        n = len(vectors)
        runs, i = [], 0
        while i < n - 1:
            if vectors[i + 1] == vectors[i]:
                i += 1
                continue
            j = i + 1
            while j < n - 1 and vectors[j + 1] != vectors[j] and \
                    collinear(vectors[i:j + 2]):
                j += 1
            runs.append((i, j))
            i = j

        def leaf(i, j):
            first, far = vectors[i], max(vectors[i:j + 1], key=lambda v: dist(v, vectors[i]))
            axis = [b - a for a, b in zip(first, far)]
            length2 = sum(c * c for c in axis)
            ts = [sum((v[c] - first[c]) * axis[c] for c in range(3)) / length2
                  for v in vectors[i:j + 1]]
            lo, hi, norm = normalize(ts)
            shape = [[0.0, norm[0]]] if xs[i] > 0 else []
            shape += [[xs[i + m], norm[m], *(eases[i + m] if m < j - i else [])]
                      for m in range(j - i + 1)]
            if xs[j] < 1:
                shape.append([1.0, norm[-1]])
            return Curve(x, shape, [first[c] + lo * axis[c] for c in range(3)],
                         [first[c] + hi * axis[c] for c in range(3)], "vector")

        def join(a, b):
            if b == a + 1:
                return leaf(*runs[a])
            k = (a + b) // 2
            return Curve(x, [[0.0, 0.0, "hold"], [xs[runs[k][0]], 1.0], [1.0, 1.0]],
                         join(a, k), join(k, b), "vector")

        return join(0, len(runs))

    def heads(self, axis, grain):
        """split → group → mirror, from a mapping and a grain (8.2)."""
        heads = None
        span = axis.get("span", "selection") if axis else "selection"
        if axis and axis.get("per_group"):
            span = "group"
        if span != "selection":
            heads = self.g.wire(self.g.add("split", {}, {"by": span}))
        size = GRAIN[grain or "head"]
        if size is not None:
            heads = self.g.wire(self.g.add("group", {
                "heads": heads, "size": float(size) if size else None}))
        mirror = axis.get("mirror") if axis else None
        if mirror:
            heads = self.g.wire(self.g.add("mirror", {
                "heads": heads, "normal": [float(c) for c in mirror["normal"]],
                "offset": float(mirror.get("offset", 0.0)) or None}))
        return heads

    def space_node(self, axis, grain, offset=None, width=None, wrap=False, heads=None):
        """A space node. Without a stroke, wrap stays off so x is the whole
        axis (a wrapped empty stroke would put half the axis outside)."""
        source = axis["source"]
        kind, direction = source["kind"], None
        if axis.get("reverse"):
            hard("a reversed axis")
        if heads is None:
            heads = self.heads(axis, grain)
        if kind in LINES:
            kind, direction = "line", LINES[kind]
        elif kind == "vector":
            kind, direction = "line", [float(c) for c in source["direction"]]
        elif kind == "major_axis":
            kind = "line"
            self.notes.add(MAJOR_AXIS)
        elif kind == "random":
            heads = self.g.wire(self.g.add("shuffle", {"heads": heads}))
            kind = "order"
        elif kind in ("radial", "angle"):
            plane = axis.get("plane") or {"kind": "auto"}
            direction = (PLANES.get(plane["kind"]) or
                         (plane.get("normal") if plane["kind"] == "custom" else None))
        elif kind == "order":
            self.notes.add(ORDER)
        else:
            hard(f"unknown axis {kind}")
        if wrap and kind in ("line", "radial"):
            self.notes.add(WRAP_RING)
        return self.g.add("space", {"heads": heads, "direction": direction,
                                    "offset": offset, "width": width},
                          {"kind": kind, "wrap": "yes" if wrap else "no"})

    def space(self, body, path, inherited, kind):
        if body.get("gradient") is not None and kind != "color":
            hard(f"{path}: a gradient on a {kind}")
        if kind == "color" and body.get("gradient") is None:
            hard(f"{path}: a space color without a gradient")
        axis, grain = body["axis"], body.get("grain", "head")
        offset_value = body.get("offset")
        clock, offset = inherited, None
        overrun = backward = False
        shape = body["curve"]["points"] if body.get("curve") else None
        if offset_value is not None:
            if offset_value["type"] != "time":
                hard(f"{path}: an offset that is not a time source")
            off_body = offset_value["value"]
            clock = self.events(off_body.get("events"), f"{path}/offset", inherited)
            offset = self.number(offset_value, f"{path}/offset", inherited)
            keys = off_body.get("points")
            if keys:
                glides = all(len(p) < 3 or p[2] != "hold" for p in keys)
                overrun = body.get("boundary", "clip") == "clip" and glides
                # A wrapped line stroke whose middle runs the whole axis once
                # per event starts and ends half on each end: a ghost half at
                # the far end at every event start. Enter and leave instead.
                line = body["axis"]["source"]["kind"] in (*LINES, "vector", "major_axis")
                if (line and body.get("boundary") == "wrap" and keys in RAMPS
                        and not body.get("width_relative")):
                    overrun = True
                    self.notes.add(FULL_TURN)
                ways = directions(keys)
                backward = ways == {True}
                if ways == {True, False}:
                    if shape is not None and not symmetric(shape):
                        self.notes.add(ASYMMETRIC)
                    elif shape is None:
                        self.notes.add(ASYMMETRIC)
        gain = self.number(body["gain"], f"{path}/gain", clock) if "gain" in body else 1.0
        wrap = body.get("boundary", "clip") == "wrap" and not overrun
        width = wide = None
        if offset_value is not None:
            w = self.number(body.get("width", {"type": "number", "value": 0.2}),
                            f"{path}/width", clock)
            if body.get("width_relative"):
                w0 = at_start(w)
                if not is_value(w):
                    self.notes.add(RELATIVE_VARYING)
                gap = min(w0 * self.spacing(off_body.get("events"), f"{path}/offset"),
                          0.8 if overrun else 4.0)
                w = min(gap / (1 - gap), 4.0) if overrun else gap  # 0.8 / 0.2 is 4 + 1 ulp
            w0 = at_start(w)
            if not is_value(w):
                self.notes.add(VARYING_WIDTH)
            ov = 1.0 if overrun else 0.0
            # Centre-anchored (with overrun) → start-anchored.
            offset = affine(offset, 1 + w0 * ov, -w0 / 2 * (1 + ov))
            width = w
            if overrun and is_value(w):
                # The old overrun stroke left out its two exact ends
                # (1e-9 < x < 1 − 1e-9); the new stroke keeps them. Pull both
                # ends in by a hair so heads that sit on an end stay dark.
                offset = affine(offset, 1.0, w * EDGE)
                width = w * (1 - 2 * EDGE)
            if wrap and w0 > 1:
                if not is_value(w):
                    hard(f"{path}: a varying wrapped width above 1")
                # Wider than the axis: the old window ran half a turn either
                # side of the stroke's middle; the new one runs one turn from
                # its offset. Start the window half a turn before the middle
                # and read the shape from 0.5 − 0.5/w on (shifted below).
                offset = affine(offset, 1.0, w0 / 2 - 0.5)
                wide = (0.5 - 0.5 / w0, 1 / w0)
        node = self.space_node(axis, grain, self.g.emit(offset), self.g.emit(width),
                               wrap and offset_value is not None)
        if kind == "color":
            g = gradient(body["gradient"])
            if backward:
                g = {"stops": [{"t": 1 - s["t"], "color": s["color"]}
                               for s in reversed(g["stops"])]}
            ramp = RAMP_UP if wide is None else shift(RAMP_UP, *wide)
            return multiply(Curve(node, ramp, kind="color", gradient=g), gain)
        if backward:
            shape, holds = flip(shape)
            if holds:
                self.notes.add(HOLD_FLIP)
        if wide is not None:
            shape = shift(shape, *wide, self.notes)
        # Outside the stroke the old space gave 0, and a number curve gives
        # its low: keep low 0 and the shape as it was.
        return multiply(Curve(node, shape, 0.0, 1.0), gain)

    def random(self, body, path, inherited):
        clock = self.events(body.get("events"), path, inherited)
        coverage = self.number(body["coverage"], f"{path}/coverage", clock)
        level = self.number(body.get("level", {"type": "number", "value": 1}),
                            f"{path}/level", clock)
        heads = self.heads(None, body.get("grain", "head"))
        heads = self.g.wire(self.g.add("shuffle", {
            "heads": heads, "clock": clock and self.g.wire(clock)}))
        node = self.space_node({"source": {"kind": "order"}}, None,
                               0.0, self.g.emit(coverage), heads=heads)
        return Curve(node, ON, 0.0, level)

    def noise(self, body, path, inherited):
        speed = self.number(body.get("speed", {"type": "beats", "value": 4}), f"{path}/speed", inherited)
        contrast = self.number(body.get("contrast", {"type": "number", "value": 0}),
                               f"{path}/contrast", inherited)
        low = self.number(body["range"][0], f"{path}/low", inherited)
        high = self.number(body["range"][1], f"{path}/high", inherited)
        scale = None
        if body.get("independent"):
            scale = 0.02
            self.notes.add(NOISE_INDEPENDENT)
        elif body.get("scale") is not None:
            scale = self.g.emit(self.number(body["scale"], f"{path}/scale", inherited))
            self.notes.add(NOISE_SPATIAL)
        self.notes.add(NOISE)
        node = self.g.add("noise", {
            "heads": self.heads(None, body.get("grain", "head")),
            "speed": self.g.emit(speed), "scale": scale,
            "contrast": self.g.emit(contrast) or None}, share=False)
        return Curve(node, RAMP_UP, low, high)

    def audio(self, body, path, inherited):
        f, th = float(body["floor"]), float(body.get("threshold", 0))
        gain = self.number(body["gain"], f"{path}/gain", inherited) if "gain" in body else 1.0
        node = self.g.add("audio", {"low_hz": float(body["from_hz"]),
                                    "high_hz": float(body["to_hz"])})
        shape = [[0, f], [1, 1]] if th == 0 else [[0, 0, "hold"], [th, f + (1 - f) * th], [1, 1]]
        return multiply(Curve(node, shape, 0.0, 1.0), gain)

    # -- forms (8.2) ----------------------------------------------------
    def alpha(self):
        fade = self.inputs.get("fade")
        if fade is None:
            return None
        v = self.number(fade, "fade", None)
        return None if v == 1.0 else v

    def convert(self):
        kind = KINDS[self.form]
        g = self.g
        if kind == "color":
            brightness = self.number(self.inputs["brightness"], "brightness", None)
            color = self.color(self.inputs["color"], "color")
            alpha = self.alpha()
            b_clocks = g.clock_set(brightness)
            if b_clocks & (g.clock_set(color) | g.clock_set(alpha)):
                self.notes.add(OVERLAP)
            out = g.add("color", {"color": None if color == [1.0, 1.0, 1.0] else g.emit(color),
                                  "brightness": None if brightness == 1.0 else g.emit(brightness),
                                  "alpha": g.emit(alpha)})
        elif kind == "strobe":
            rate = self.number(self.inputs["rate"], "rate", None)
            out = g.add("strobe", {"rate": None if rate == 0.5 else g.emit(rate),
                                   "alpha": g.emit(self.alpha())})
        else:
            out = self.aim()
        return tidy(g.nodes, out)

    def aim(self):
        g, inputs = self.g, self.inputs
        base = inputs["base"]["value"]
        # Same_as leaders first, so a follower finds its clock.
        yaw = self.number(inputs["horizontal"], "horizontal", None)
        pitch = self.number(inputs["vertical"], "vertical", None)
        axis = inputs["axis"]["value"]
        heads = self.heads({"mirror": axis.get("mirror")}, None) if axis.get("mirror") else None
        direction = point = None
        if base == "point":
            point = self.vector(inputs["point"], "point")
        else:
            direction = self.vector(inputs["direction"], "direction")
        lean = inputs.get("lean", {"type": "vector", "value": [0, 0, 0]})
        if lean["type"] == "vector" or (lean["type"] == "time" and _zero_keys(lean["value"])):
            v = [float(c) for c in lean["value"]] if lean["type"] == "vector" else [0, 0, 0]
            if any(v):
                if not isinstance(direction, list):
                    hard("a constant lean on a point base or an animated direction")
                direction = lean_direction(direction, v, math.sqrt(sum(c * c for c in v)))
        elif lean["type"] == "space":
            body = lean["value"]
            source = body["axis"]["source"]["kind"]
            if source in ("radial",) and base == "direction":
                self.notes.add(BLOOM)
                base, direction = "away", None
                point = Curve(g.add("time", {}), RAMP_UP, [0.0, 0.0, 40.0], [0.0, 0.0, 6.0], "vector")
            else:
                self.notes.add(FAN)
                fan = self.fan(body)
                if yaw == 0.0:
                    yaw = fan
                else:
                    if isinstance(direction, Curve) and not (is_value(fan.low) and is_value(fan.high)):
                        self.notes.add(FAN_GAIN_AT_START)
                    # Yaw is taken: lean the direction itself, per head.
                    toward = LINES.get(source, [1, 0, 0])
                    direction = Curve(fan.x, fan.shape, leaned(direction, toward, fan.low),
                                      leaned(direction, toward, fan.high), "vector")
        else:
            hard(f"lean: a {lean['type']} source")
        if direction == DEFAULT_DIRECTION:
            direction = None
        out = g.add("aim", {"heads": heads, "direction": g.emit(direction),
                            "point": g.emit(point),
                            "yaw": None if yaw == 0.0 else g.emit(yaw),
                            "pitch": None if pitch == 0.0 else g.emit(pitch),
                            "alpha": g.emit(self.alpha())},
                    {"base": base})
        return out

    def fan(self, body):
        """lean = space with a curve and gain g → yaw from −g/2 to g/2."""
        axis = body["axis"]
        node = self.space_node(axis, body.get("grain", "head"))
        # Old magnitude: curve(|c − 0.5|) × sign(c − 0.5) over the axis.
        points = body["curve"]["points"]
        if [p[:2] for p in points] == [[0, 0], [1, 1]] and all(len(p) < 3 for p in points[:-1]):
            shape = RAMP_UP
        else:
            xs = sorted({0.0, 1.0, *[0.5 + p[0] for p in points if p[0] <= 0.5],
                         *[0.5 - p[0] for p in points if p[0] <= 0.5]})
            shape = [[x, min(1.0, max(0.0, 0.5 + math.copysign(sample(points, abs(x - 0.5)), x - 0.5)))]
                     for x in xs]
        gain = self.number(body["gain"], "lean/gain", None) if "gain" in body else 1.0
        return multiply(Curve(node, shape, -0.5, 0.5), gain)


def tidy(nodes, out):
    """Drops nodes the output does not read and renumbers the rest, per
    kind in creation order, so ids stay `<kind>1`, `<kind>2`, ..."""
    live, stack = set(), [out]
    while stack:
        node_id = stack.pop()
        if node_id in live:
            continue
        live.add(node_id)
        stack += [v["node"] for v in nodes[node_id]["inputs"].values()
                  if isinstance(v, dict) and "node" in v]
    count, rename = collections.Counter(), {}
    for node_id, node in nodes.items():
        if node_id in live:
            count[node["kind"]] += 1
            rename[node_id] = f"{node['kind']}{count[node['kind']]}"
    result = {}
    for node_id, node in nodes.items():
        if node_id in live:
            node = copy.deepcopy(node)
            for k, v in node["inputs"].items():
                if isinstance(v, dict) and "node" in v:
                    node["inputs"][k] = {"node": rename[v["node"]]}
            result[rename[node_id]] = node
    return {"version": 1, "nodes": result}, rename[out]


def _zero_keys(body):
    return all(not any(p[1]) for p in body.get("points", []))


def collinear(vectors):
    first = vectors[0]
    far = max(vectors, key=lambda v: dist(v, first))
    axis = [b - a for a, b in zip(first, far)]
    length2 = sum(c * c for c in axis)
    if length2 == 0:
        return True
    for v in vectors:
        t = sum((v[i] - first[i]) * axis[i] for i in range(3)) / length2
        if dist([first[i] + t * axis[i] for i in range(3)], v) > 1e-9 * max(1.0, math.sqrt(length2)):
            return False
    return True


def dist(a, b):
    return math.sqrt(sum((x - y) ** 2 for x, y in zip(a, b)))


def lean_direction(d, toward, degrees):
    """aim::lean: rotate d by `degrees` toward `toward`."""
    length = math.sqrt(sum(c * c for c in d)) or 1.0
    d = [c / length for c in d]
    along = sum(t * c for t, c in zip(toward, d))
    across = [t - along * c for t, c in zip(toward, d)]
    size = math.sqrt(sum(c * c for c in across))
    if size < 1e-9 or degrees == 0:
        return d
    across = [c / size for c in across]
    s, c = math.sin(math.radians(degrees)), math.cos(math.radians(degrees))
    return [c * a + s * b for a, b in zip(d, across)]


def leaned(d, toward, degrees):
    """lean_direction for a number or a number curve with value ends; a
    curve becomes a vector curve between the two leaned directions. An
    animated direction leans every vector it holds."""
    if isinstance(d, Curve):
        if not isinstance(degrees, (int, float)):
            degrees = at_start(degrees)  # the caller notes FAN_GAIN_AT_START
        return Curve(d.x, d.shape, leaned(d.low, toward, degrees),
                     leaned(d.high, toward, degrees), "vector")
    if isinstance(degrees, (int, float)):
        return lean_direction(d, toward, degrees)
    if not (is_value(degrees.low) and is_value(degrees.high)):
        hard("a fan lean with a nested gain source, together with a yaw source")
    return Curve(degrees.x, degrees.shape, lean_direction(d, toward, degrees.low),
                 lean_direction(d, toward, degrees.high), "vector")


def gradient(g):
    return {"stops": [{"t": float(s["t"]), "color": [float(c) for c in s["color"]]}
                      for s in g["stops"]]}


# -- names (8.1, 7.6) ------------------------------------------------------
def fmt(x):
    """A number as Rust's `{}` writes an f64: 2 → "2", 0.5 → "0.5"."""
    text = repr(float(x))
    if "e" in text:
        text = f"{float(x):.20f}".rstrip("0")
    return text[:-2] if text.endswith(".0") else text.rstrip(".")


def summary(graph):
    nodes = graph["nodes"]
    parts = []
    for kind in ("space", "clock"):
        for node in nodes.values():
            if node["kind"] != kind:
                continue
            if kind == "space":
                parts.append(node["settings"]["kind"])
            else:
                every = node["inputs"].get("every")
                parts.append(f"every {fmt(every)}" if isinstance(every, (int, float)) else "every varies")
    if any(n["kind"] == "noise" for n in nodes.values()):
        parts.append("noise")
    for node in nodes.values():
        if node["kind"] == "audio":
            i = node["inputs"]
            parts.append(f"audio {fmt(i.get('low_hz', 40))}–{fmt(i.get('high_hz', 100))} Hz")
    return " · ".join(dict.fromkeys(parts)) or "still"


def structure(graph, out):
    """The graph from its output, ignoring ids, numbers, vectors, colors
    and gradients: kinds, settings, wires and shapes."""
    nodes = graph["nodes"]

    def walk(node_id):
        node = nodes[node_id]
        inputs = {}
        for k, v in sorted(node.get("inputs", {}).items()):
            if isinstance(v, dict) and "node" in v:
                inputs[k] = walk(v["node"])
            elif isinstance(v, dict) and "points" in v:
                inputs[k] = v["points"]
        return [node["kind"], node.get("settings", {}), inputs]

    return json.dumps(walk(out), sort_keys=True)


def output_of(graph):
    kinds = ("color", "aim", "strobe")
    return next(k for k, n in graph["nodes"].items() if n["kind"] in kinds)


def load_presets(path=None):
    """Shipped clip presets as {structure: name}, when presets.json already
    holds clip graphs (section 9). Empty otherwise."""
    path = pathlib.Path(path or pathlib.Path(__file__).resolve().parents[1]
                        / "crates/patterns/src/presets.json")
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        return {}
    clips = data.get("clips") if isinstance(data, dict) else None
    result = {}
    if isinstance(clips, dict):
        clips = [dict(v, name=k) for k, v in clips.items()]
    for preset in clips or []:
        graph = preset.get("graph", preset)
        if isinstance(graph, dict) and "nodes" in graph:
            result.setdefault(structure(graph, output_of(graph)), preset["name"])
    return result


PRESETS = None


def name_for(graph, out):
    global PRESETS
    if PRESETS is None:
        PRESETS = load_presets()
    match = PRESETS.get(structure(graph, out))
    if match:
        return match
    return f"{graph['nodes'][out]['kind'].capitalize()} · {summary(graph)}"


# -- clips and documents ---------------------------------------------------
def convert_inputs(form, inputs, duration):
    """(graph, notes) for one form clip. Raises Unmappable."""
    if form not in KINDS:
        hard(f"unknown form {form}")
    clip = Clip(form, inputs, float(duration))
    graph, out = clip.convert()
    return graph, out, clip.notes


def convert_clip(clip):
    """A score-document clip in the old shape → the new shape."""
    graph, out, _ = convert_inputs(clip["graph"], clip["inputs"], clip["duration"])
    new = {k: v for k, v in clip.items() if k not in ("graph", "inputs", "form")}
    new["name"] = clip.get("name") or name_for(graph, out)
    new["graph"] = graph
    return new


def is_old_clip(value):
    return isinstance(value, dict) and "inputs" in value and isinstance(value.get("graph"), str)


def convert_document(value):
    if isinstance(value, list):
        return [convert_document(v) for v in value]
    if not isinstance(value, dict):
        return value
    if is_old_clip(value):
        return convert_clip(value)
    return {k: convert_document(v) for k, v in value.items()}


def old_clips(value, path=""):
    """(path, clip) for every old clip inside a document."""
    if isinstance(value, list):
        for i, v in enumerate(value):
            yield from old_clips(v, f"{path}[{i}]")
    elif isinstance(value, dict):
        if is_old_clip(value):
            yield path, value
        else:
            for k, v in value.items():
                yield from old_clips(v, f"{path}.{k}" if path else k)


# -- database mode ---------------------------------------------------------
def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def compact(value):
    return json.dumps(value, separators=(",", ":"))


def run_database(database, out):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(f"file:{pathlib.Path(database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    columns = {r[1] for r in db.execute("PRAGMA table_info(clips)")}
    clip_sql, draft_sql, names = [], [], []
    notes, failures = collections.Counter(), []
    examples = collections.defaultdict(list)
    total = 0
    for row in db.execute("SELECT id, graph, duration, inputs_json FROM clips ORDER BY id"):
        total += 1
        try:
            graph, out_id, why = convert_inputs(row["graph"], json.loads(row["inputs_json"]),
                                                row["duration"])
        except Unmappable as e:
            failures.append((f"clip {row['id']}", str(e)))
            continue
        name = name_for(graph, out_id)
        names.append((row["id"], name))
        for n in why:
            notes[n] += 1
            if len(examples[n]) < 5:
                examples[n].append(row["id"])
        clip_sql.append(
            f"UPDATE clips SET name = {quote(name)}, graph_json = {quote(compact(graph))} "
            f"WHERE id = {quote(row['id'])} AND graph = {quote(row['graph'])} "
            f"AND inputs_json = {quote(row['inputs_json'])};")
    drafts = 0
    for row in db.execute("SELECT id, base_json, state_json FROM drafts ORDER BY id"):
        sets = []
        for column in ("base_json", "state_json"):
            document = json.loads(row[column])
            for path, clip in old_clips(document):
                try:
                    convert_inputs(clip["graph"], clip["inputs"], clip["duration"])
                except Unmappable as e:
                    failures.append((f"draft {row['id']} {column} {path}", str(e)))
            try:
                new = convert_document(document)
            except Unmappable:
                continue
            if new != document:
                sets.append(f"{column} = {quote(compact(new))}")
        if sets:
            drafts += 1
            draft_sql.append(f"UPDATE drafts SET {', '.join(sets)} WHERE id = {quote(row['id'])} "
                             f"AND base_json = {quote(row['base_json'])} "
                             f"AND state_json = {quote(row['state_json'])};")
    hard_failures = [(k, v) for k, v in failures if is_hard(v)]
    lines = ["# Clip graphs migration: conversion", "",
             f"- clips: {total}; converted {len(clip_sql)}; hard unmappable "
             f"{sum(1 for k, _ in hard_failures if k.startswith('clip '))}",
             f"- drafts rows changed: {drafts}",
             f"- columns name/graph_json present in the copy: "
             f"{'yes' if {'name', 'graph_json'} <= columns else 'no (run the SQLite migration first)'}",
             "", "## Approximations (converted, listed)", ""]
    lines += [f"- {k}: {n} (e.g. {', '.join(examples[k])})" for k, n in notes.most_common()] or ["- none"]
    lines += ["", "## Hard unmappables (the run refuses)", ""]
    reasons = collections.Counter(v for _, v in hard_failures)
    lines += [f"- {v}: {n}" for v, n in reasons.most_common()] or ["- none"]
    lines += [""] + [f"  - {k}: {v}" for k, v in hard_failures[:200]]
    (out / "report.md").write_text("\n".join(lines) + "\n")
    with open(out / "names.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["id", "name"])
        w.writerows(names)
    if hard_failures:
        for p in ("clips.sql", "drafts.sql"):
            (out / p).unlink(missing_ok=True)
        print(f"refused: {len(hard_failures)} hard unmappable clips; see {out / 'report.md'}",
              file=sys.stderr)
        return 1
    (out / "clips.sql").write_text(f"-- expected: {len(clip_sql)}\n" + "\n".join(clip_sql) + "\n")
    (out / "drafts.sql").write_text(f"-- expected: {len(draft_sql)}\n" + "\n".join(draft_sql) + "\n")
    print(json.dumps({"clips": len(clip_sql), "drafts": drafts, "approximate": dict(notes)}))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database")
    parser.add_argument("--out")
    args = parser.parse_args()
    if args.database:
        if not args.out:
            parser.error("--out is required with --database")
        sys.exit(run_database(args.database, args.out))
    try:
        json.dump(convert_document(json.load(sys.stdin)), sys.stdout, indent=2)
    except Unmappable as e:
        print(e, file=sys.stderr)
        sys.exit(1)
    print()


if __name__ == "__main__":
    main()
