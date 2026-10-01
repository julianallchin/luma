"""Clip graphs as Python: one small graph per clip.

    t = time(every=2)
    place = space(shift=curve(t, "Ramp up", low=-0.2, high=1), scale=0.2)
    graph = color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))
    edit.add_clip(graph, name="Chase", beats=(32, 48), selection="bars")

Every builder is a bare function. It returns one node. A node goes into an
input of another node: that is a wire. The same Python object wired twice is
one node (a link). `color()`, `aim()` and `strobe()` make the output node and
return a Graph.

Values are plain: numbers, `(u, v, z)` tuples, `(r, g, b)` triples in linear
Rec. 2020, or "#RRGGBB" (sRGB, converted). A shape is a curve preset name
("Comet"), a list of `[x, v]` / `[x, v, ease]` points, or `{"points": ...}`;
two points at the same x are a jump. A gradient is a preset name ("Fire"), a
list of `(t, color)` pairs, or `{"stops": ...}`. `None` is the empty input.

Math is Python operators on values (curve results and math results):
`cut * bloom * fade`, `a + b`, `1 - a`, `0.5 * a`, `max(a, b)`, `min(a, b)`.
Each is a `math` node; a chain of one operator is one node with all its
items. Coordinates (time, space, noise, audio) are not values: wrap them in
curve(...) first.

A node's id is the variable it is assigned to (`place` above), so code,
graph and the card in the UI say the same name; `source()` writes the ids
back as variables. A node with no variable (or a name that cannot be an id:
a builder name or a Python keyword) takes `<kind><n>`, numbered per kind;
`source()` writes such a curve or math in place when one input reads it.
Rename a node by changing its variable in that code (a node loaded from a
stored graph keeps its stored id). Python does no type checking: the Rust
checker does, when you add or update a clip.
"""
from __future__ import annotations

import builtins
import copy
import itertools
import keyword
import math
import re
import sys
from dataclasses import dataclass
from types import MappingProxyType

from .color import from_srgb


BUILDERS = ("time", "space", "noise", "audio", "curve", "mirror", "shuffle", "group",
            "split", "color", "aim", "strobe", "preset", "max", "min")
# Names that cannot be node ids: the builders, and `math`, a kind with no
# builder (math is Python operators).
RESERVED = frozenset(BUILDERS) | {"math"}
OUTPUTS = ("color", "aim", "strobe")
VERSION = 3  # clip_graph::VERSION

# Input order per kind: the builder signature order, used by source().
_INPUTS = {
    "time": ("every", "duration", "delay", "phase"),
    "space": ("heads", "direction", "shift", "scale"),
    "noise": ("heads", "speed", "scale", "contrast"),
    "audio": ("low_hz", "high_hz"),
    "curve": ("x", "shape", "low", "high", "gradient"),
    "math": ("values",),
    "mirror": ("heads", "normal", "at"),
    "shuffle": ("heads", "time"),
    "group": ("heads", "size"),
    "split": ("heads",),
    "color": ("color", "brightness", "alpha"),
    "aim": ("heads", "direction", "point", "yaw", "pitch", "alpha"),
    "strobe": ("rate", "alpha"),
}
# Inputs that take one number 0-1. A list there was the version 2 product.
_SCALARS = {("color", "brightness"), ("color", "alpha"), ("aim", "alpha"),
            ("strobe", "rate"), ("strobe", "alpha"), ("noise", "contrast")}
# Math operators that chain: `a * b * c` is one node.
_CHAINS = ("*", "+", "max", "min")
_SEQUENCE = itertools.count(1)
_ID = re.compile(r"([a-z]+)([0-9]+)")
_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]{0,31}")  # clip_graph::check::is_name


class ClipError(ValueError):
    """A clip graph the checker refused, or a value Python cannot write."""


# ---------------------------------------------------------------------------
# presets
# ---------------------------------------------------------------------------

_PRESETS = {"clips": {}, "curves": {}, "gradients": {}, "bands": {}}


def _plain(value):
    if isinstance(value, (str, bytes)) or value is None:
        return value
    if hasattr(value, "items"):
        return {str(key): _plain(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(item) for item in value]
    return copy.deepcopy(value)


def _named(table):
    """A preset table as {name: record}; a list of {"name": ...} records works too."""
    table = _plain(table) or {}
    if isinstance(table, list):
        return {str(item["name"]): item for item in table}
    return dict(table)


def install_presets(presets):
    """Install the shipped presets from the `presets` binding."""
    presets = _plain(presets) or {}
    clips = {}
    for name, value in _named(presets.get("clips")).items():
        if "graph" not in value:
            value = {"graph": value}
        clips[name] = {"graph": value["graph"], "blend_mode": value.get("blend_mode")}
    curves = {}
    for name, value in _named(presets.get("curves")).items():
        if isinstance(value, dict):
            value = value.get("curve", value)
            value = value.get("points") if isinstance(value, dict) else value
        curves[name] = value
    gradients = {}
    for name, value in _named(presets.get("gradients")).items():
        if isinstance(value, dict):
            value = value.get("gradient", value)
            value = value.get("stops") if isinstance(value, dict) else value
        gradients[name] = value
    bands = {}
    for name, value in _named(presets.get("bands")).items():
        if isinstance(value, dict):
            value = (value["low_hz"], value["high_hz"])
        bands[name] = (float(value[0]), float(value[1]))
    _PRESETS.update(clips=clips, curves=curves, gradients=gradients, bands=bands)


def _lookup(table, name, what, example):
    entries = _PRESETS[table]
    if name in entries:
        return name, copy.deepcopy(entries[name])
    for key, value in entries.items():
        if key.casefold() == name.casefold():
            return key, copy.deepcopy(value)
    known = ", ".join(sorted(entries)) or "none installed"
    raise ClipError(f"unknown {what} {name!r}; known: {known}. Example: {example}")


class Presets:
    """`luma.presets`: shipped clips, curves, gradients and bands.

    clips      name -> Graph (a fresh copy each time)
    curves     name -> points
    gradients  name -> stops
    bands      name -> (low_hz, high_hz)

    A name works where a shape, a gradient or a band goes:
    curve(t, "Comet"), curve(t, gradient="Fire"), audio("Kick").
    """

    @property
    def clips(self):
        return MappingProxyType({name: preset(name) for name in _PRESETS["clips"]})

    @property
    def curves(self):
        return MappingProxyType(copy.deepcopy(_PRESETS["curves"]))

    @property
    def gradients(self):
        return MappingProxyType(copy.deepcopy(_PRESETS["gradients"]))

    @property
    def bands(self):
        return MappingProxyType(dict(_PRESETS["bands"]))

    def _luma_catalog_items(self):
        return [(name, tuple(sorted(_PRESETS[name]))) for name in ("clips", "curves", "gradients", "bands")]

    def __repr__(self):
        return "<luma.presets " + " ".join(
            f"{name}={len(_PRESETS[name])}" for name in ("clips", "curves", "gradients", "bands")) + ">"


# ---------------------------------------------------------------------------
# nodes
# ---------------------------------------------------------------------------


class Node:
    """One node: a kind, its settings and its inputs. Immutable once built.

    Values (curve and math results) take `*`, `+` and `-` with each other and
    with numbers; each makes a math node.
    """

    __slots__ = ("kind", "settings", "inputs", "_seq", "_id")

    def __init__(self, kind, settings=None, inputs=None, id=None):
        object.__setattr__(self, "kind", kind)
        object.__setattr__(self, "settings", MappingProxyType(dict(settings or {})))
        object.__setattr__(self, "inputs", MappingProxyType(
            {key: value for key, value in (inputs or {}).items() if value is not None}))
        object.__setattr__(self, "_seq", next(_SEQUENCE))
        object.__setattr__(self, "_id", id)

    def __setattr__(self, name, value):
        raise AttributeError("a node is immutable; build a new one")

    def __repr__(self):
        return f"<{type(self).__name__} {self.kind}>"

    def __mul__(self, other):
        return _operate("*", self, other)

    def __rmul__(self, other):
        return _operate("*", other, self)

    def __add__(self, other):
        return _operate("+", self, other)

    def __radd__(self, other):
        return _operate("+", other, self)

    def __sub__(self, other):
        return _operate("-", self, other)

    def __rsub__(self, other):
        return _operate("-", other, self)

    def _copy(self, inputs):
        """The same node (kind, settings, id, creation order) with new inputs."""
        node = type(self)(self.kind, self.settings, inputs, id=self._id)
        object.__setattr__(node, "_seq", self._seq)
        return node


class Coordinate(Node):
    """A raw 0-1 coordinate: goes into curve.x (time also into shuffle.time)."""


class Value(Node):
    """A curve's or a math node's output: a number, vector or color wire."""


class Heads(Node):
    """A heads wire from mirror, shuffle, group or split."""


class _Output(Node):
    """The output node of a graph."""


_CLASSES = {"time": Coordinate, "space": Coordinate, "noise": Coordinate, "audio": Coordinate,
            "curve": Value, "math": Value, "mirror": Heads, "shuffle": Heads, "group": Heads,
            "split": Heads, "color": _Output, "aim": _Output, "strobe": _Output}


def _number(value, where):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ClipError(f"{where}: expected a number; got {value!r}")
    value = float(value)
    if not math.isfinite(value):
        raise ClipError(f"{where}: expected a finite number; got {value!r}")
    return value


def _input(value, where):
    """A plain value or a wire, as the node stores it."""
    if value is None or isinstance(value, Node):
        if isinstance(value, _Output):
            raise ClipError(f"{where}: an output node cannot feed an input. Example: {where.split('.')[-1]}=curve(time())")
        return value
    if isinstance(value, Graph):
        raise ClipError(f"{where}: a graph is a whole clip, not an input. Example: {where.split('.')[-1]}=curve(time())")
    if isinstance(value, str):
        if value.startswith("#"):
            try:
                return from_srgb(value)
            except ValueError as error:
                raise ClipError(f"{where}: {error}") from None
        raise ClipError(f"{where}: expected a number, a (u, v, z) tuple, an (r, g, b) triple, "
                        f"\"#RRGGBB\" or a wire; got {value!r}")
    if isinstance(value, (list, tuple)):
        if tuple(where.split(".")) in _SCALARS or any(isinstance(item, Node) for item in value):
            raise ClipError(f"{where}: expected one number or wire; a list does not multiply. "
                            f"Write a * b. Example: {where.split('.')[-1]}=cut * fade")
        return [_number(item, where) for item in value]
    return _number(value, where)


def _shape(value, where):
    if value is None or isinstance(value, Node):
        return value
    if isinstance(value, str):
        return {"points": _lookup("curves", value, "curve preset", 'shape="Ramp up"')[1]}
    value = _plain(value)
    if isinstance(value, dict):
        return value
    if isinstance(value, list):
        return {"points": value}
    raise ClipError(f"{where}: expected a curve preset name or points; got {value!r}")


def _stop_color(color, where):
    if isinstance(color, str):
        return _input(color, where)
    return [_number(item, where) for item in color]


def _gradient(value, where):
    if value is None or isinstance(value, Node):
        return value
    if isinstance(value, str):
        return {"stops": _lookup("gradients", value, "gradient preset", 'gradient="Rainbow"')[1]}
    value = _plain(value)
    if isinstance(value, dict):
        stops = value.get("stops", [])
        return dict(value, stops=[dict(stop, color=_stop_color(stop["color"], where))
                                  if isinstance(stop, dict) and "color" in stop else stop
                                  for stop in stops])
    if isinstance(value, list):
        return {"stops": [{"t": _number(t, where), "color": _stop_color(color, where)}
                          for t, color in value]}
    raise ClipError(f"{where}: expected a gradient preset name, (t, color) pairs or stops; got {value!r}")


def _make(kind, settings=None, **inputs):
    return _CLASSES[kind](kind, settings, {key: _input(item, f"{kind}.{key}") for key, item in inputs.items()})


# ---------------------------------------------------------------------------
# math
# ---------------------------------------------------------------------------


def _not_a_value(node, op):
    if isinstance(node, Coordinate):
        return (f"math {op}: {node.kind}() is a coordinate (0-1), not a value; wrap it in curve(...). "
                f"Example: curve(t, low=0, high=2) {op} fade")
    return (f"math {op}: {node.kind}() is not a value; only curve(...) results and math on them are. "
            f"Example: curve(space(heads), 'Ramp up') {op} fade")


def _math(op, items):
    values = []
    for item in items:
        if isinstance(item, Value):
            values.append(item)
        elif isinstance(item, Node):
            raise ClipError(_not_a_value(item, op))
        else:
            values.append(_number(item, f"math {op}"))
    return Value("math", {"op": op}, {"values": values})


def _operate(op, left, right):
    for item in (left, right):
        if not isinstance(item, Node) and (isinstance(item, bool) or not isinstance(item, (int, float))):
            return NotImplemented
    return _math(op, [left, right])


def _extreme(op, builtin, args, kwargs):
    if len(args) == 1 and not isinstance(args[0], Node):
        try:
            items = list(args[0])
        except TypeError:
            return builtin(*args, **kwargs)
        if not any(isinstance(item, Node) for item in items):
            return builtin(items, **kwargs)
    else:
        items = list(args)
        if not any(isinstance(item, Node) for item in items):
            return builtin(*args, **kwargs)
    if kwargs:
        raise ClipError(f"{op}: on values {op}() takes no {', '.join(sorted(kwargs))}. Example: {op}(a, b)")
    if len(items) < 2:
        raise ClipError(f"{op}: expected 2 or more items; got {len(items)}. Example: {op}(a, b)")
    return _math(op, items)


def max(*args, **kwargs):
    """The larger value per light: max(a, b, ...) on curve or math results is
    one math node. On plain numbers it is Python's max."""
    return _extreme("max", builtins.max, args, kwargs)


def min(*args, **kwargs):
    """The smaller value per light: min(a, b, ...) on curve or math results is
    one math node. On plain numbers it is Python's min."""
    return _extreme("min", builtins.min, args, kwargs)


# ---------------------------------------------------------------------------
# builders
# ---------------------------------------------------------------------------


def time(every=None, duration=None, delay=None, phase=None) -> Coordinate:
    """Each head's own clock, 0-1 over the clip or over each event.

    `every` (beats): an event starts every `every` beats from the clip
    start; empty = once over the clip. `duration` (beats): each event's
    life; empty = every (the clip with no every). A duration above every
    makes events overlap on purpose (tails, many pills).
    `delay` (beats, any sign): the head starts this much later. Before its
    start the clock is below 0 and curves hold their first value: a curve
    over space on delay makes a one-shot wipe.
    `phase` (turns): added, then wrapped to 0-1: a curve over space on phase
    makes a loop such as a chase or a wave.
    Two time nodes with equal every and duration share one set of events.
    """
    return _make("time", every=every, duration=duration, delay=delay, phase=phase)


def _wrap(wrap, kind):
    if wrap is None:
        wrap = kind == "angle"
    if isinstance(wrap, str):
        return wrap
    return "yes" if wrap else "no"


def space(heads=None, direction=None, shift=None, scale=None, kind="line", wrap=None) -> Coordinate:
    """Place of each head: (a - shift) / scale, where a is 0-1 per head.

    kind: "line" (along direction; empty = best fit), "order" (rank),
    "radial" (distance from the centre), "angle" (turns around the centre).
    A region is a curve with jumps: the left half is
    curve(space(), [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]).
    `shift` (share) slides the coordinate, as a shader's p - offset: a curve
    over time on shift moves the curve along the heads (a chase, a sweep with
    its own ease), audio on shift makes a meter. `scale` (share, 0 or more)
    is how much of the axis reads as 0-1. With wrap, x tiles as a shader's
    fract((a - shift) / scale): the shape repeats every `scale` (0.25 = four
    copies); one pill per turn is a narrow curve with scale 1.
    A line space after a mirror with the same direction measures from the
    mirror's plane.
    """
    return _make("space", {"kind": kind, "wrap": _wrap(wrap, kind)},
                 heads=heads, direction=direction, shift=shift, scale=scale)


def noise(heads=None, speed=None, scale=None, contrast=None) -> Coordinate:
    """Coherent value noise 0-1. No scale = one value for all heads; 0.02 = each head its own."""
    return _make("noise", heads=heads, speed=speed, scale=scale, contrast=contrast)


def audio(low_hz=None, high_hz=None) -> Coordinate:
    """Energy of a band of the full mix, 0-1 over the clip. `audio("Kick")` takes a band preset."""
    if isinstance(low_hz, str) and high_hz is None:
        low_hz, high_hz = _lookup("bands", low_hz, "band preset", 'audio("Kick")')[1]
    return _make("audio", low_hz=low_hz, high_hz=high_hz)


def curve(x, shape=None, low=None, high=None, gradient=None) -> Value:
    """Turn a coordinate into a value: low + shape(x) * (high - low), or gradient(shape(x)).

    The kind follows the arguments: a gradient makes a color curve, tuple
    low/high a vector curve, anything else a number curve.
    """
    def vector(value):
        return (isinstance(value, (list, tuple))
                or (isinstance(value, Value) and value.settings.get("kind") == "vector"))
    kind = "color" if gradient is not None else "vector" if vector(low) or vector(high) else "number"
    return Value("curve", {"kind": kind}, {
        "x": _input(x, "curve.x"), "shape": _shape(shape, "curve.shape"),
        "low": _input(low, "curve.low"), "high": _input(high, "curve.high"),
        "gradient": _gradient(gradient, "curve.gradient")})


def mirror(heads=None, normal=None, at=None) -> Heads:
    """Fold heads across a plane (empty normal = best fit). `at` (share 0-1)
    places the plane along the normal within the selection; empty = 0.5, the
    centre. Heads on the low side reflect; aim yaw and pitch mirror too."""
    return _make("mirror", heads=heads, normal=normal, at=at)


def shuffle(heads=None, time=None) -> Heads:
    """A random order of the heads: one order, or a new order per event of
    `time` (a time node with every). Read it with space(kind="order")."""
    return _make("shuffle", heads=heads, time=time)


def group(heads=None, size=None) -> Heads:
    """Merge heads into units of `size` heads within a fixture (empty = one fixture)."""
    return _make("group", heads=heads, size=size)


def split(heads=None, by="fixture") -> Heads:
    """Make each fixture (by="fixture") or venue group (by="group") its own span."""
    return _make("split", {"by": by}, heads=heads)


def color(color=None, brightness=None, alpha=None) -> "Graph":
    """Color output: light = color * brightness. Empty color is white.

    `brightness` is the pattern across the lights (chase, pulse, cut).
    `alpha` (0-1) is the clip's opacity: it mixes the clip's light with the
    light below; 0 shows the light below in every blend mode.
    """
    return Graph(_make("color", color=color, brightness=brightness, alpha=alpha))


def aim(heads=None, base="direction", direction=None, point=None, yaw=None, pitch=None, alpha=None) -> "Graph":
    """Aim output. base: "direction" (along the vector), "point" (at the point),
    "away" (from the point through each head). Then yaw turns right and pitch
    up, in degrees. Heads from a mirror take the mirror image of yaw and pitch.
    `alpha` is the aim's weight.
    """
    return Graph(_make("aim", {"base": base}, heads=heads, direction=direction, point=point,
                       yaw=yaw, pitch=pitch, alpha=alpha))


def strobe(rate=None, alpha=None) -> "Graph":
    """Strobe output: shutter = rate. `alpha` (0-1) is the clip's opacity over
    the strobe below."""
    return Graph(_make("strobe", rate=rate, alpha=alpha))


def preset(name) -> "Graph":
    """A copy of a shipped clip preset, with its name and blend mode.
    luma.presets.clips lists them."""
    name, record = _lookup("clips", name, "clip preset", 'preset("Chase")')
    return Graph.from_json(record["graph"], name=name, blend=record["blend_mode"])


# ---------------------------------------------------------------------------
# graph
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class NodeRecord:
    """A stored node: kind, settings, inputs (a wire is {"node": id})."""
    kind: str
    settings: MappingProxyType
    inputs: MappingProxyType


def _items(value):
    return value if isinstance(value, list) else [value]


def _wires(node):
    """The nodes wired into `node`, once per wire (a list item is a wire)."""
    return [item for value in node.inputs.values() for item in _items(value) if isinstance(item, Node)]


def _reachable(output):
    seen, order, stack = set(), [], [output]
    while stack:
        node = stack.pop()
        if id(node) in seen:
            continue
        seen.add(id(node))
        order.append(node)
        stack.extend(_wires(node))
    return order


def _reads(nodes):
    """{id(node): how many inputs read it}, a list item counting once."""
    reads = {}
    for node in nodes:
        for item in _wires(node):
            reads[id(item)] = reads.get(id(item), 0) + 1
    return reads


def _usable(name):
    return bool(_NAME.fullmatch(name)) and name not in RESERVED and not keyword.iskeyword(name)


def _variables(nodes):
    """{id(node): variable name} for the nodes the calling code has bound to
    a variable: the innermost frame outside this module first."""
    wanted = {id(node) for node in nodes}
    names = {}
    frame = sys._getframe(1)
    while frame is not None and frame.f_globals.get("__name__") == __name__:
        frame = frame.f_back
    while frame is not None and len(names) < len(wanted):
        for name, value in list(frame.f_locals.items()):
            if isinstance(value, Node) and id(value) in wanted and id(value) not in names \
                    and _usable(name):
                names[id(value)] = name
        frame = frame.f_back
    return names


def _op(node):
    return node.settings.get("op", "*")


def _flatten(output, names):
    """Merge chains of one math operator into one node: `a * b * c` is built
    as (a * b) * c, and the inner node goes into the outer one when it has no
    name (no variable, no stored id) and only that node reads it.
    Returns the new output and `names` keyed by the new nodes."""
    reads = _reads(_reachable(output))

    def loose(item, op):
        return (isinstance(item, Node) and item.kind == "math" and op in _CHAINS and _op(item) == op
                and item._id is None and id(item) not in names and reads.get(id(item)) == 1)

    built, renamed = {}, {}

    def build(node):
        if id(node) in built:
            return built[id(node)]
        inputs, changed = {}, False
        for key, value in node.inputs.items():
            if isinstance(value, list):
                items = []
                for item in value:
                    if node.kind == "math" and key == "values" and loose(item, _op(node)):
                        items.extend(build(item).inputs["values"])
                    else:
                        items.append(build(item) if isinstance(item, Node) else item)
                changed |= len(items) != len(value) or any(a is not b for a, b in zip(items, value))
            else:
                items = build(value) if isinstance(value, Node) else value
                changed |= items is not value
            inputs[key] = items
        result = node._copy(inputs) if changed else node
        built[id(node)] = result
        if id(node) in names:
            renamed[id(result)] = names[id(node)]
        return result

    return build(output), renamed


def _assign_ids(nodes, names=None):
    """Ids: a stored id first, then the variable name, then `<kind><n>`."""
    ids, used = {}, set()
    for node in sorted(nodes, key=lambda node: node._seq):
        if node._id is not None and node._id not in used:
            ids[id(node)] = node._id
            used.add(node._id)
    for node in sorted(nodes, key=lambda node: node._seq):
        name = (names or {}).get(id(node))
        if id(node) not in ids and name is not None and name not in used:
            ids[id(node)] = name
            used.add(name)
    for node in sorted(nodes, key=lambda node: node._seq):
        if id(node) in ids:
            continue
        number = 1
        while f"{node.kind}{number}" in used:
            number += 1
        ids[id(node)] = f"{node.kind}{number}"
        used.add(ids[id(node)])
    return ids


def _stored(value, ids):
    if isinstance(value, Node):
        return {"node": ids[id(value)]}
    if isinstance(value, list):
        return [_stored(item, ids) for item in value]
    return copy.deepcopy(value)


class Graph:
    """One clip's graph: nodes wired into exactly one output node.

    graph.nodes    id -> NodeRecord(kind, settings, inputs)
    graph.output   the output NodeRecord (color, aim or strobe)
    graph.name     the preset name, when it came from a preset
    graph.blend    the preset blend mode, when it came from a preset
    graph.json()   the stored JSON
    graph.source() Python that rebuilds this graph
    """

    __slots__ = ("_output", "_nodes", "_ids", "name", "blend")

    def __init__(self, output, name=None, blend=None):
        names = {}
        if output is not None:
            names = _variables(_reachable(output))
            output, names = _flatten(output, names)
        nodes = _reachable(output) if output is not None else []
        object.__setattr__(self, "_output", output)
        object.__setattr__(self, "_nodes", nodes)
        object.__setattr__(self, "_ids", _assign_ids(nodes, names))
        object.__setattr__(self, "name", name)
        object.__setattr__(self, "blend", blend)

    def __setattr__(self, name, value):
        raise AttributeError("a graph is immutable; build a new one")

    def json(self):
        nodes = {}
        for node in sorted(self._nodes, key=lambda node: node._seq):
            record = {"kind": node.kind}
            if node.settings:
                record["settings"] = dict(node.settings)
            record["inputs"] = {key: _stored(value, self._ids) for key, value in node.inputs.items()}
            nodes[self._ids[id(node)]] = record
        return {"version": VERSION, "nodes": nodes}

    @property
    def nodes(self):
        return MappingProxyType({key: NodeRecord(value["kind"], MappingProxyType(value.get("settings", {})),
                                                 MappingProxyType(value["inputs"]))
                                 for key, value in self.json()["nodes"].items()})

    @property
    def output(self):
        if self._output is None:
            return None
        return self.nodes[self._ids[id(self._output)]]

    def node(self, node_id):
        """The builder node behind an id, to wire into a new graph."""
        for node in self._nodes:
            if self._ids[id(node)] == node_id:
                return node
        raise KeyError(node_id)

    def source(self):
        """Python that rebuilds this graph; the last line is the output call.

        A curve or math with a numbered id (`curve3`, `math1`) that one input
        reads is written in place; every other node gets its own line.
        """
        if self._output is None:
            return ""
        nodes = sorted(self._nodes, key=lambda node: node._seq)
        reads = _reads(nodes)
        chained = {id(item) for node in nodes if node.kind == "math" and _op(node) in _CHAINS
                   for item in _items(node.inputs.get("values")) if isinstance(item, Node)
                   and item.kind == "math" and _op(item) == _op(node)}
        inline = set()
        for node in nodes:
            match = _ID.fullmatch(self._ids[id(node)])
            if (node.kind in ("curve", "math") and node is not self._output and reads.get(id(node)) == 1
                    and match and match.group(1) == node.kind
                    # In place, a math in a chain of its own operator would merge into it.
                    and id(node) not in chained):
                inline.add(id(node))
        while True:
            lines, order = [], []
            for node in nodes:
                if id(node) in inline:
                    continue
                text, _ = _expression(node, self._ids, inline, order)
                lines.append(text if node is self._output else f"{self._ids[id(node)]} = {text}")
            moved = self._renumbered(order, inline)
            if not moved:
                return "\n".join(lines)
            # Running the code would number these differently: give them a line.
            inline -= moved

    def _renumbered(self, order, inline):
        """The in-place nodes that running the source would number differently
        from their ids (the stored numbering is not creation order)."""
        used = {self._ids[id(node)] for node in self._nodes
                if id(node) not in inline and node is not self._output}
        moved = set()
        for node in order:
            if id(node) not in inline and node is not self._output:
                continue
            number = 1
            while f"{node.kind}{number}" in used:
                number += 1
            used.add(f"{node.kind}{number}")
            if id(node) in inline and f"{node.kind}{number}" != self._ids[id(node)]:
                moved.add(id(node))
        return moved

    @classmethod
    def from_json(cls, data, name=None, blend=None):
        """Rebuild a graph from its stored JSON. Unknown kinds pass through for the checker."""
        data = _plain(data) or {}
        if not isinstance(data, dict):
            raise ClipError(f"graph: expected a graph object with nodes; got {data!r}")
        stored = data.get("nodes") or {}
        if not stored:
            return cls(None, name=name, blend=blend)

        def wire(value):
            return isinstance(value, dict) and set(value) == {"node"}

        def depends(record):
            return [item["node"] for value in (record.get("inputs") or {}).values()
                    for item in _items(value) if wire(item)]

        def key(node_id):
            match = _ID.fullmatch(node_id)
            return (int(match.group(2)), match.group(1)) if match else (0, node_id)

        waiting = {node_id: set(depends(record)) & set(stored) for node_id, record in stored.items()}
        ready = {node_id for node_id, deps in waiting.items() if not deps}
        built = {}

        def first(node_id):
            # The lowest id of its kind still to build: taking these first keeps
            # source() numbering the nodes as they are stored.
            kind = stored[node_id].get("kind")
            return all(key(node_id) <= key(other) for other in stored
                       if other not in built and stored[other].get("kind") == kind)

        while ready:
            node_id = builtins.min(ready, key=lambda node_id: (not first(node_id), key(node_id)))
            ready.discard(node_id)
            record = stored[node_id]
            inputs = {}
            for input_name, value in (record.get("inputs") or {}).items():
                for item in _items(value):
                    if wire(item) and item["node"] not in built:
                        raise ClipError(f"{node_id}.{input_name}: wire to unknown node {item['node']!r}")
                if wire(value):
                    value = built[value["node"]]
                elif isinstance(value, list) and any(wire(item) for item in value):
                    value = [built[item["node"]] if wire(item) else item for item in value]
                inputs[input_name] = value
            kind = record.get("kind")
            built[node_id] = _CLASSES.get(kind, Node)(kind, record.get("settings"), inputs, id=node_id)
            for other, deps in waiting.items():
                if node_id in deps:
                    deps.discard(node_id)
                    if not deps and other not in built:
                        ready.add(other)
        if len(built) != len(stored):
            raise ClipError("graph: expected no cycle; got a loop of wires. Example: rebuild it with the builders")
        outputs = [node for node in built.values() if isinstance(node, _Output)]
        if len(outputs) != 1:
            names = ", ".join(sorted(node._id for node in outputs)) or "none"
            raise ClipError(f"graph: expected one output node; got {names}. Example: one clip per output")
        graph = cls(outputs[0], name=name, blend=blend)
        missing = set(stored) - {graph._ids[id(node)] for node in graph._nodes}
        if missing:
            raise ClipError(f"graph: expected every node to reach the output; {', '.join(sorted(missing))} does not")
        return graph

    def __eq__(self, other):
        return isinstance(other, Graph) and self.json() == other.json()

    def __hash__(self):
        return hash(repr(self.json()))

    def __repr__(self):
        if self._output is None:
            return "<Graph empty>"
        title = f"Graph {self.name!r}" if self.name else "Graph"
        return f"<{title}\n{self.source()}\n>"


# ---------------------------------------------------------------------------
# source text
# ---------------------------------------------------------------------------


def _literal(value):
    if isinstance(value, float):
        return str(int(value)) if value.is_integer() and abs(value) < 1e15 else repr(value)
    if isinstance(value, (list, tuple)):
        items = ", ".join(_literal(item) for item in value)
        return f"({items},)" if len(value) == 1 else f"({items})"
    if isinstance(value, dict):
        return "{" + ", ".join(f"{key!r}: {_json_literal(item)}" for key, item in value.items()) + "}"
    return repr(value)


def _json_literal(value):
    if isinstance(value, dict):
        return "{" + ", ".join(f"{key!r}: {_json_literal(item)}" for key, item in value.items()) + "}"
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(_json_literal(item) for item in value) + "]"
    if isinstance(value, float):
        return _literal(value)
    return repr(value)


def _preset_name(table, value):
    for name, entry in _PRESETS[table].items():
        if _plain(entry) == value:
            return name
    return None


def _shape_literal(value):
    points = value.get("points") if isinstance(value, dict) and set(value) == {"points"} else None
    if points is not None:
        name = _preset_name("curves", points)
        return repr(name) if name else _json_literal(points)
    return _json_literal(value)


def _gradient_literal(value):
    stops = value.get("stops") if isinstance(value, dict) and set(value) == {"stops"} else None
    if stops is not None:
        name = _preset_name("gradients", stops)
        if name:
            return repr(name)
        if all(isinstance(stop, dict) and set(stop) == {"t", "color"} for stop in stops):
            return "[" + ", ".join(f"({_literal(float(stop['t']))}, {_literal(stop['color'])})"
                                   for stop in stops) + "]"
    return _json_literal(value)


_DEFAULT_SETTINGS = {"space": {"kind": "line"}, "aim": {"base": "direction"}, "split": {"by": "fixture"}}
# Python precedence: an atom (a name, a number, a call) binds tightest.
_ATOM = 3
_PRECEDENCE = {"*": 2, "+": 1, "-": 1}


def _expression(node, ids, inline, order):
    """(text, precedence) of the expression that builds `node`. Appends to
    `order` each node it builds, in the order Python would create them."""
    if node.kind == "math":
        text, level = _math_text(node, ids, inline, order)
    else:
        text, level = f"{node.kind}({', '.join(_arguments(node, ids, inline, order))})", _ATOM
    order.append(node)
    return text, level


def _reference(value, ids, inline, order):
    if id(value) in inline:
        return _expression(value, ids, inline, order)
    return ids[id(value)], _ATOM


def _math_text(node, ids, inline, order):
    op = _op(node)
    parts = [_reference(item, ids, inline, order) if isinstance(item, Node) else (_literal(item), _ATOM)
             for item in _items(node.inputs.get("values", []))]
    if op in ("max", "min"):
        return f"{op}({', '.join(text for text, _ in parts)})", _ATOM
    level = _PRECEDENCE.get(op, 2)
    # Python reads a - b - c as (a - b) - c: a right operand at the same level needs brackets.
    texts = [f"({text})" if part < level or (index and part == level) else text
             for index, (text, part) in enumerate(parts)]
    return f" {op} ".join(texts), level


def _value_text(name, value, ids, inline, order):
    if isinstance(value, Node):
        return _reference(value, ids, inline, order)[0]
    if isinstance(value, list) and any(isinstance(item, Node) for item in value):
        return "[" + ", ".join(_reference(item, ids, inline, order)[0] if isinstance(item, Node)
                               else _literal(item) for item in value) + "]"
    if name == "shape":
        return _shape_literal(value)
    if name == "gradient":
        return _gradient_literal(value)
    return _literal(value)


def _arguments(node, ids, inline, order):
    args = []
    names = list(_INPUTS.get(node.kind, ())) + sorted(set(node.inputs) - set(_INPUTS.get(node.kind, ())))
    names = [name for name in names if name in node.inputs]
    # curve(x, shape, ...): x and shape go by position, as people write them.
    positional = ("x", "shape") if node.kind == "curve" else ()
    for name in names:
        text = _value_text(name, node.inputs[name], ids, inline, order)
        if positional and name == positional[0]:
            positional = positional[1:]
            args.append(text)
        else:
            positional = ()
            args.append(f"{name}={text}")
    settings = dict(node.settings)
    if node.kind == "curve":
        settings.pop("kind", None)
    for name, value in settings.items():
        if node.kind == "space" and name == "wrap":
            if value != _wrap(None, settings.get("kind", "line")):
                args.append(f"wrap={value == 'yes'}" if value in ("yes", "no") else f"wrap={value!r}")
            continue
        if _DEFAULT_SETTINGS.get(node.kind, {}).get(name) == value:
            continue
        args.append(f"{name}={value!r}")
    return args
