"""Score-local graphs and clips over the same typed document GPUI edits.

    edit = luma.track.edit()
    graph = edit.graph(node="chase")  # one node, with its controls exposed
    clip = edit.add_clip(graph, beats=(32, 48), selection="bars",
                         inputs={"width": .3, "mapping": "z"})
    edit.check()
    edit.window(beats=(32, 36)).output.heatmap()
    edit.apply()

Compose with graph.node(), node.output(), graph.expose() and graph.output().
definition(id) follows both shipped and local references. source() is the exact
score.luma document, suitable for an agent workspace or a one-shot model.

Envelope values store anchors and optional Bézier segments, for example:
    {"points": [[0, 1], [1, 0]], "curves": [
        {"kind": "bezier", "control1": [.3, 1], "control2": [.7, 0]}]}
An envelope needs 2–256 anchors, starting at x=0 and ending at x=1 with strictly
increasing x; both coordinates must be finite and in 0..1.
Handles use the same normalized coordinates as anchors. Their x positions must
stay ordered between their segment endpoints; y stays in 0..1. An omitted
curves list means straight segments. Every editor and evaluator uses this value.
"""
from __future__ import annotations

import bisect
import copy
import json
import math
import re
import uuid
from dataclasses import dataclass

from .track import (Track, TrackOutput, TrackError, TrackReadOnlyError,
                    TrackClosedError, _ImmutableSnapshot, _field, _items,
                    _freeze, _range_pair, _selection, _blend, _z,
                    _downbeat_values, _check_result, _pattern_color, BLEND_MODES)


def _plain(value):
    if hasattr(value, "items"):
        return {key: _plain(item) for key, item in _items(value)}
    if isinstance(value, (list, tuple)):
        return [_plain(item) for item in value]
    return copy.deepcopy(value)


def _typed(kind, value):
    """Units come from the port schema; explicit typed values are also accepted."""
    value = _plain(value)
    kind = _plain(kind)
    if isinstance(kind, dict) and "signal" in kind:
        spec = kind["signal"]
        if isinstance(value, dict) and "type" in value:
            if value["type"] not in {"signal", "number", "beats", "proportion", "position", "degrees", "seconds", "color", "field", "mask", "color_field"}:
                raise TrackError("a signal socket needs a numerical value")
            return value  # The core validates units, channels and fixture domains.
        rgb = spec.get("channels") == "rgb" or isinstance(value, (list, tuple)) or (isinstance(value, str) and value.startswith("#"))
        literal = "color" if rgb else spec.get("unit") or "number"
        return _typed(literal, value)
    if isinstance(value, dict) and "type" in value:
        if value["type"] != kind:
            raise TrackError(f"expected {kind}, got {value['type']}")
        if kind == "seed":
            return _typed(kind, value.get("value"))
        return value
    if kind == "seed":
        if isinstance(value, str) and re.fullmatch(r"[0-9]+", value):
            value = int(value)
        if type(value) is not int or not 0 <= value < (1 << 64):
            raise TrackError("seed needs an integer from 0 through 18446744073709551615")
        value = str(value)
    if kind == "color" and isinstance(value, str):
        if not re.fullmatch(r"#[0-9a-fA-F]{6}", value):
            raise TrackError("color must be #RRGGBB or three normalized channels")
        value = [int(value[index:index+2], 16) / 255 for index in (1, 3, 5)]
    if kind == "gradient":
        if isinstance(value, list):
            value = {"stops": [{"t": stop[0], "color": stop[1]} for stop in value]}
        if not isinstance(value, dict) or "stops" not in value:
            raise TrackError("gradient needs stops with position and color")
        value = dict(value, stops=[dict(stop, color=_typed("color", stop["color"])["value"])
                                   for stop in value["stops"]])
    if kind == "mapping" and isinstance(value, str):
        source = {"kind": value}
        if value == "circle":
            source["origin"] = 0.0
        if value == "major_axis":
            source["toward"] = [0.0, 0.0, 1.0]
        if value == "vector":
            source["direction"] = [1.0, 0.0, 1.0]
        value = {"source": source, "reverse": False, "per_group": False}
    if kind == "envelope" and isinstance(value, list):
        value = {"points": value}
    return {"type": kind, "value": value}


def _label(definitions, key):
    seen = set()
    while key not in seen and key in definitions:
        seen.add(key)
        definition = definitions[key]
        if definition.get("name"):
            return definition["name"]
        body = definition["body"]
        nodes = body.get("body", {}).get("nodes", {}) if body["kind"] == "graph" else {}
        if len(nodes) != 1:
            break
        key = next(iter(nodes.values()))["definition"]
    return "Custom graph"


@dataclass(frozen=True)
class Clip:
    """One clip: id, graph, start/duration in beats, selection, z, blend, inputs.

    This is a read-only value. Use edit.update_clip(clip, ...) to change it.
    The canonical JSON spells z and blend as z_index and blend_mode.
    """
    id: str
    graph: str
    start: float
    duration: float
    selection: object
    seed: int
    z: int
    blend: str
    inputs: object

    @classmethod
    def read(cls, id, value):
        return cls(id, value["graph"], value["start"], value["duration"],
                   _freeze(value.get("selection", {"expression": "all"})), value["seed"],
                   value.get("z_index", 0), value.get("blend_mode", "replace"),
                   _freeze(value.get("inputs", {})))


class GraphTrack(_ImmutableSnapshot):
    """The current score. Edits capture a revision; apply advances this object."""
    __getattr__ = Track.__getattr__
    __dir__ = Track.__dir__
    _luma_catalog_items = Track._luma_catalog_items
    _bar_time = Track._bar_time

    def __init__(self, values, *, nodes, features=None, host_call=None, artifact_store=None):
        self._values, self._features = values, features
        self._host_call, self._artifact_store = host_call, artifact_store
        self._nodes = _plain(nodes)
        self._active = True
        self.id = str(_field(values, "id", default=""))
        self.title = str(_field(values, "title", default=""))
        self.duration_s = float(_field(values, "duration_s", default=0) or 0)
        self.editable = bool(_field(values, "editable", default=False))
        self.revision = str(_field(values, "revision"))
        self._document = _plain(_field(values, "document"))
        self._downbeats = _downbeat_values(features)
        self._beats = _downbeat_values({"downbeats": _field(features, "beats", default=None)})
        self._seal()

    def _require_active(self):
        if not self._active:
            raise TrackClosedError("this score is no longer in scope; use the current luma.track")

    def _call(self, method, payload):
        self._require_active()
        return Track._call(self, method, payload)

    def _refresh(self, snapshot):
        # Edits own their candidate and base revision; only this live facade
        # advances when the worker installs a new manifest for the same score.
        for key, value in vars(snapshot).items():
            object.__setattr__(self, key, value)

    def _revoke(self):
        object.__setattr__(self, "_active", False)

    @property
    def document(self):
        """Read-only canonical score mapping; clips/definitions are keyed by ID.

        Use track.clips to iterate Clip values. Raw clip keys include
        start/duration (beats), z_index and blend_mode; they are not z/blend.
        """
        return _freeze(self._document)

    @property
    def clips(self):
        """All saved Clip values, ordered by start beat, layer z, then ID."""
        return tuple(Clip.read(id, clip) for id, clip in sorted(
            self._document["clips"].items(), key=lambda item: (item[1]["start"], item[1].get("z_index", 0), item[0])))

    def definition(self, id):
        """Copy a built-in or score-local definition, with typed inputs and body."""
        definitions = self._nodes | self._document["definitions"]
        if id not in definitions:
            raise TrackError(f"unknown node definition {id!r}")
        return copy.deepcopy(definitions[id])

    def nodes(self, search=""):
        """Map matching node IDs to names. Read definition(id) for input schemas."""
        definitions = self._nodes | self._document["definitions"]
        return {id: _label(definitions, id) for id in sorted(definitions)
                if search.casefold() in (id + " " + _label(definitions, id)).casefold()}

    def source(self):
        """The complete saved score as canonical score.luma JSON text."""
        return json.dumps(self._document, indent=2, sort_keys=True, allow_nan=False) + "\n"

    def edit(self):
        """Capture the complete saved score in a private, revision-checked Edit.

        Existing clips are included. Changes stay private until edit.apply().
        """
        self._require_active()
        if not self.editable:
            raise TrackReadOnlyError("this score is read-only")
        return Edit(self)

    def _advance(self, document):
        object.__setattr__(self, "revision", document["revision"])
        object.__setattr__(self, "_document", copy.deepcopy(document["score"]))
        object.__setattr__(self, "_values", dict(_items(self._values)) | {
            "revision": document["revision"], "document": _freeze(document["score"]),
        })

    def _clock(self):
        if len(self._beats) < 2 or any(not math.isfinite(t) for t in self._beats) or any(
                right <= left for left, right in zip(self._beats, self._beats[1:])):
            raise TrackError("musical time requires at least two increasing detected beats")

    def _raw_beat(self, seconds):
        self._clock()
        left = max(0, min(len(self._beats)-2, bisect.bisect_right(self._beats, seconds)-1))
        return left + (seconds - self._beats[left]) / (self._beats[left+1] - self._beats[left])

    def beat_at(self, seconds):
        if not math.isfinite(seconds):
            raise TrackError("time must be finite")
        origin = _field(self._values, "beat_origin_s", default=None)
        if origin is None:
            raise TrackError("musical time requires a detected beat origin")
        return self._raw_beat(seconds) - self._raw_beat(origin)

    def seconds_at(self, beat):
        self._clock()
        if not math.isfinite(beat):
            raise TrackError("beat position must be finite")
        origin = _field(self._values, "beat_origin_s", default=None)
        if origin is None:
            raise TrackError("musical time requires a detected beat origin")
        raw = beat + self._raw_beat(origin)
        left = max(0, min(len(self._beats)-2, math.floor(raw)))
        return self._beats[left] + (raw-left) * (self._beats[left+1]-self._beats[left])

    def _range(self, *, beats=None, bars=None, seconds=None):
        if sum(value is not None for value in (beats, bars, seconds)) != 1:
            raise TrackError("specify exactly one of beats=, bars= or seconds=")
        if beats is not None:
            return _range_pair(beats, "beats")
        if bars is not None:
            if not self._downbeats:
                raise TrackError("bar ranges require detected downbeats")
            start, end = _range_pair(bars, "bars")
            return self.beat_at(self._bar_time(start)), self.beat_at(self._bar_time(end))
        start, end = _range_pair(seconds, "seconds")
        return self.beat_at(start), self.beat_at(end)

    def window(self, **range):
        return Window(self, self.revision, self._document, self._range(**range))

    def __repr__(self):
        return f"<luma.track {self.title!r} clips={len(self.clips)} graphs={len(self._document['definitions'])} editable={self.editable}>"


class Edit:
    """A complete private score candidate, including unchanged saved clips.

    Build graphs and clips, check(), preview through venue.render(edit=self),
    then apply(). Apply closes this edit; open a new one for further changes.
    """
    def __init__(self, track):
        self._track = track
        self.base_revision = track.revision
        self._base = copy.deepcopy(track._document)
        self._candidate = copy.deepcopy(self._base)
        if self._candidate.get("version") == 2:
            # This is a working copy. The original revision remains the CAS
            # base; migration is saved together with the eventual authored edit.
            self._candidate = track._host_call("track.score_upgrade", {"candidate": self._candidate})
        self._closed = False

    def _open(self):
        self._track._require_active()
        if self._closed:
            raise TrackClosedError("this edit is closed; start a new edit")

    @property
    def candidate(self):
        """A copy of the complete candidate mapping; changes to it are not edits."""
        return copy.deepcopy(self._candidate)

    @property
    def clips(self):
        """All candidate Clip values: unchanged saved clips plus staged edits."""
        return tuple(Clip.read(id, clip) for id, clip in self._candidate["clips"].items())

    def definition(self, id):
        definitions = self._track._nodes | self._candidate["definitions"]
        if id not in definitions:
            raise TrackError(f"unknown node definition {id!r}")
        return copy.deepcopy(definitions[id])

    def graph(self, id=None, *, node=None, name=None):
        """Create a local graph, or retrieve an existing graph by its ID.

        graph(node="wash") wraps one effect and exposes its inputs for clips.
        graph() starts an empty composition: add nodes, connect outputs, then
        set graph.output(...). name is an optional human-readable label.
        Built-in definitions can be read but not changed.
        """
        self._open()
        definitions = self._candidate["definitions"]
        if id is not None and (id in definitions or id in self._track._nodes):
            if node is not None or name is not None:
                raise TrackError("that graph already exists; edit it or choose a new id")
            return Graph(self, id)
        id = id or str(uuid.uuid4())
        definition = {"inputs": {}, "outputs": {}, "body": {"kind": "graph", "body": {"nodes": {}, "outputs": {}}}}
        if node is not None:
            self.definition(node)  # Report an unknown name before the host call.
            definition = self._track._host_call("track.graph_instance", {
                "candidate": copy.deepcopy(self._candidate), "definition": node,
            })
        if name is not None:
            definition["name"] = name
        definitions[id] = definition
        return Graph(self, id)

    def replace_source(self, source):
        """Stage an exact score.luma source. check/apply run Rust's validator."""
        self._open()
        candidate = json.loads(source)
        if not isinstance(candidate, dict) or candidate.get("version") not in (2, 3, 4):
            raise TrackError("expected a version-2, version-3 or version-4 score document")
        self._candidate = candidate

    def source(self):
        """The complete candidate as JSON text, including unchanged saved clips."""
        return json.dumps(self._candidate, indent=2, sort_keys=True, allow_nan=False) + "\n"

    def _graph_id(self, graph):
        if isinstance(graph, Graph):
            if graph._edit is not self:
                raise TrackError("graph belongs to another edit; use its id to reference a saved definition")
            return graph.id
        return str(graph)

    def _inputs(self, graph, inputs):
        schema = self.definition(graph)["inputs"]
        result = {}
        for key, value in (inputs or {}).items():
            if key not in schema:
                raise TrackError(f"{graph} has no input {key!r}")
            result[key] = _typed(schema[key]["value_type"], value)
        return result

    def add_clip(self, graph, *, id=None, beats=None, bars=None, seconds=None,
                 selection="all", subset=None, z=None, blend="replace", seed=None, inputs=None):
        """Stage a clip and return its Clip value; graph may be a Graph or ID.

        Supply exactly one half-open range: beats=(0,32), bars=(1,9), or
        seconds=(0,16). Beats start at zero; bars at one. selection is a group
        expression. subset accepts a fraction, head count, or "all".
        inputs overrides the graph's exposed controls; definition(graph.id)
        gives their types/defaults. Clips composite bottom-up by integer z.
        Omit z to place above clips overlapping this time range (or at zero
        when the range is empty). Supply z explicitly to choose layer order.
        replace covers the lower layer; add sums/clamps; screen brightens
        without replacing the base color. Inspect overlaps before applying.
        """
        self._open()
        graph = self._graph_id(graph)
        start, end = self._track._range(beats=beats, bars=bars, seconds=seconds)
        id = id or str(uuid.uuid4())
        if id in self._candidate["clips"]:
            raise TrackError(f"clip {id!r} already exists")
        if z is None:
            z = max((clip.get("z_index", 0) for clip in self._candidate["clips"].values()
                     if clip["start"] < end and start < clip["start"] + clip["duration"]),
                    default=-1) + 1
        value = {"graph": graph, "start": start, "duration": end-start,
                 "selection": _selection(selection, subset), "z_index": _z(z), "blend_mode": _blend(blend),
                 "seed": seed if seed is not None else uuid.uuid4().int & ((1 << 64)-1),
                 "inputs": self._inputs(graph, inputs)}
        self._candidate["clips"][id] = value
        return Clip.read(id, value)

    def update_clip(self, clip, *, beats=None, bars=None, seconds=None,
                    selection=None, subset=None, z=None, blend=None, seed=None, inputs=None):
        """Update a Clip or clip ID and return its new value; other fields stay.

        Ranges, selection, z and blend use add_clip's conventions. inputs
        merges overrides; inputs={key: None} restores that graph default.
        """
        self._open()
        id = clip.id if isinstance(clip, Clip) else str(clip)
        if id not in self._candidate["clips"]:
            raise TrackError(f"unknown clip {id!r}")
        value = copy.deepcopy(self._candidate["clips"][id])
        if any(value is not None for value in (beats, bars, seconds)):
            start, end = self._track._range(beats=beats, bars=bars, seconds=seconds)
            value.update(start=start, duration=end-start)
        if selection is not None:
            value.setdefault("selection", {"expression": "all"})["expression"] = _selection(selection)["expression"]
        if subset is not None:
            value["selection"] = _selection(value.get("selection", {}).get("expression", "all"), subset)
        if z is not None:
            value["z_index"] = _z(z)
        if blend is not None:
            value["blend_mode"] = _blend(blend)
        if seed is not None:
            value["seed"] = seed
        if inputs is not None:
            overrides = value.setdefault("inputs", {})
            overrides.update(self._inputs(value["graph"], {key: item for key, item in inputs.items() if item is not None}))
            for key, item in inputs.items():
                if item is None:
                    if key not in self.definition(value["graph"])["inputs"]:
                        raise TrackError(f"graph has no input {key!r}")
                    overrides.pop(key, None)
        self._candidate["clips"][id] = value
        return Clip.read(id, value)

    def remove_clip(self, clip):
        """Remove a Clip or clip ID from the candidate; nothing is saved yet."""
        self._open()
        id = clip.id if isinstance(clip, Clip) else str(clip)
        if id not in self._candidate["clips"]:
            raise TrackError(f"unknown clip {id!r}")
        del self._candidate["clips"][id]

    def make_independent(self, clip, *, id=None):
        """Copy this clip's graph dependencies so edits won't affect other clips."""
        self._open()
        id = id or str(uuid.uuid4())
        clip = clip.id if isinstance(clip, Clip) else str(clip)
        self._candidate = self._track._call("track.score_independent", {"candidate": self.candidate, "clip": clip, "id": id})
        return Graph(self, id)

    def _gesture(self, graph, edits):
        self._open()
        self._candidate = self._track._call("track.graph_edit", {"candidate": self.candidate, "graph": graph, "edits": edits})

    def check(self):
        """Run the native score validator; return CheckResult without saving.

        Reports incomplete graphs, invalid inputs and revision conflicts.
        The validation verb is check(), not validate().
        """
        self._open()
        return _check_result(self._track._call("track.score_check", {"baseRevision": self.base_revision, "candidate": self.candidate}))

    def apply(self):
        """Validate and commit the complete candidate, then close this edit.

        Returns the saved revision and advances luma.track. A child workspace
        saves privately for its parent's merge. On conflict, inspect current
        state and open a fresh edit; never silently overwrite other work.
        """
        self._open()
        result = self._track._call("track.score_apply", {"baseRevision": self.base_revision, "candidate": self.candidate})
        self._track._advance(result)
        self._closed = True
        return result["revision"]

    def diff(self):
        """IDs added, removed or updated versus this edit's captured base."""
        result = {}
        for kind in ("definitions", "clips"):
            before, after = self._base[kind], self._candidate[kind]
            result[kind] = {"added": sorted(after.keys()-before.keys()),
                            "removed": sorted(before.keys()-after.keys()),
                            "updated": sorted(key for key in before.keys() & after.keys() if before[key] != after[key])}
        return result

    def window(self, **range):
        """Inspect a beats=, bars= or seconds= window of the complete candidate.

        timeline() plots clips; output.heatmap() shows composited light output.
        For the 3D scene, use luma.venue.render(edit=self, t=seconds).
        """
        return Window(self._track, self.base_revision, self._candidate, self._track._range(**range))

    def _preview(self, only=None):
        """Snapshot for the venue camera; isolation never mutates the draft."""
        self._open()
        candidate = self.candidate
        if only is not None:
            clips = [only] if isinstance(only, (Clip, str)) else list(only)
            ids = [clip.id if isinstance(clip, Clip) else str(clip) for clip in clips]
            missing = [id for id in ids if id not in candidate["clips"]]
            if missing:
                raise TrackError(f"unknown preview clip(s): {', '.join(missing)}")
            candidate["clips"] = {id: candidate["clips"][id] for id in ids}
        return {"baseRevision": self.base_revision, "candidate": candidate}


# Discovery and validation use the same mode vocabulary.
Edit.add_clip.__doc__ += "\nAvailable blend modes: " + ", ".join(sorted(BLEND_MODES)) + "."


@dataclass(frozen=True)
class Output:
    node: "Node"
    port: str


@dataclass(frozen=True)
class Input:
    graph: "Graph"
    key: str

    def rename(self, name):
        """Rename the visible control; its stable key and clip overrides stay."""
        self.graph._edit._gesture(self.graph.id, [{"op": "rename_input", "key": self.key, "name": name}])
        return self

    def move(self, x, y):
        self.graph._edit._gesture(self.graph.id, [{"op": "move_inputs", "positions": {self.key: [x, y]}}])
        return self


@dataclass(frozen=True)
class Node:
    graph: "Graph"
    id: str

    def output(self, port=None):
        """Reference an output for wiring; omit port when this node has only one."""
        spec = self.graph._node_spec(self.id)
        if port is None and len(spec["outputs"]) == 1:
            port = next(iter(spec["outputs"]))
        if port not in spec["outputs"]:
            raise TrackError(f"choose an output from {list(spec['outputs'])}")
        return Output(self, port)

    def bind(self, **inputs):
        """Change this node's input values/connections in place; None unbinds."""
        self.graph._bind(self.id, inputs)
        return self

    def customize(self, *, id=None):
        """Copy this node's graph into the score and edit that call site."""
        edit = self.graph._edit
        edit._open()
        id = id or str(uuid.uuid4())
        edit._candidate = edit._track._call("track.graph_customize", {
            "candidate": edit.candidate, "graph": self.graph.id, "node": self.id, "id": id,
        })
        return Graph(edit, id)


class Graph:
    """A score graph. Wire node outputs to inputs, then declare its output."""
    def __init__(self, edit, id):
        self._edit, self.id = edit, id

    def definition(self):
        """Copy this graph's full definition, including ports and node body."""
        return self._edit.definition(self.id)

    def source(self):
        """This graph's definition as JSON text; edit.source() exports the score."""
        return json.dumps(self.definition(), indent=2, sort_keys=True, allow_nan=False) + "\n"

    def _same(self, graph):
        if graph._edit is not self._edit or graph.id != self.id:
            raise TrackError("connections must stay within one graph; expose inputs on a subgraph")

    def _node_spec(self, id):
        definition = self.definition()
        nodes = definition["body"]["body"].get("nodes", {}) if definition["body"]["kind"] == "graph" else {}
        if id not in nodes:
            raise TrackError(f"unknown node {id!r}")
        return self._edit.definition(nodes[id]["definition"])

    def _binding(self, value, kind):
        if isinstance(value, Output):
            self._same(value.node.graph)
            return {"source": "connection", "node": value.node.id, "output": value.port}
        if isinstance(value, Input):
            self._same(value.graph)
            return {"source": "input", "input": value.key}
        return {"source": "value", "value": _typed(kind, value)}

    def node(self, definition, *, id=None, **inputs):
        """Add a node by definition ID; inputs are constants or wired outputs.

        Inspect edit.definition(definition) for its typed input/output ports.
        Returns a Node; use node.output(port) to connect it to the next node.
        """
        self._edit._open()
        spec = self._edit.definition(definition)
        id = id or str(uuid.uuid4())
        edits = [{"op": "add", "id": id, "definition": definition}]
        for key, value in inputs.items():
            if key not in spec["inputs"]:
                raise TrackError(f"{definition} has no input {key!r}")
            edits.append({"op": "bind", "node": id, "input": key,
                          "binding": self._binding(value, spec["inputs"][key]["value_type"])})
        self._edit._gesture(self.id, edits)
        return Node(self, id)

    def get(self, id):
        """Retrieve a node in this graph by its node ID, for example to bind()."""
        self._node_spec(id)
        return Node(self, id)

    def _bind(self, id, inputs):
        spec = self._node_spec(id)
        edits = []
        for key, value in inputs.items():
            if key not in spec["inputs"]:
                raise TrackError(f"node {id} has no input {key!r}")
            edits.append({"op": "bind", "node": id, "input": key,
                          "binding": None if value is None else self._binding(value, spec["inputs"][key]["value_type"])})
        self._edit._gesture(self.id, edits)

    def add_input(self, name, *, key=None, position=(0, 0)):
        """Add a named Input; its first destination infers type and default."""
        key = key or str(uuid.uuid4())
        self._edit._gesture(self.id, [{"op": "add_input", "key": key, "name": name, "position": list(position)}])
        return Input(self, key)

    def expose(self, node, input, *, key=None, name=None):
        """Create an Input and wire it to a parameter in one edit."""
        self._same(node.graph)
        spec = self._node_spec(node.id)["inputs"].get(input)
        if spec is None:
            raise TrackError(f"node {node.id} has no input {input!r}")
        key = key or input
        self._edit._gesture(self.id, [
            {"op": "add_input", "key": key, "name": name or spec["name"], "position": [0, 0]},
            {"op": "bind", "node": node.id, "input": input, "binding": {"source": "input", "input": key}},
        ])
        return Input(self, key)

    def input(self, key):
        """Reference a named Input, including one awaiting its first wire."""
        definition = self.definition()
        nodes = definition["body"]["body"].get("input_nodes", {}) if definition["body"]["kind"] == "graph" else {}
        if key not in definition["inputs"] and key not in nodes:
            raise TrackError(f"graph has no input {key!r}")
        return Input(self, key)

    def default(self, key, value):
        """Change an exposed input's default; clip overrides remain explicit."""
        spec = self.definition()["inputs"].get(key)
        if spec is None:
            raise TrackError(f"graph has no input {key!r}")
        self._edit._gesture(self.id, [{"op": "default", "key": key, "value": _typed(spec["value_type"], value)}])

    def output(self, output, *, key="lighting"):
        """Declare a Node.output(...) as this graph's output.

        Connect numerical signals to an Output node for a playable clip.
        Reusable graphs may expose numerical or structured outputs directly.
        """
        if not isinstance(output, Output):
            raise TrackError("a graph output must reference a node output")
        self._edit._gesture(self.id, [{"op": "output", "key": key, "binding": self._binding(output, None)}])

    def remove(self, node):
        self._same(node.graph)
        edit = {"op": "remove_input", "key": node.key} if isinstance(node, Input) else {"op": "remove", "id": node.id}
        self._edit._gesture(self.id, [edit])

    def rename(self, name):
        self._edit._gesture(self.id, [{"op": "name", "name": name}])

    def __repr__(self):
        return f"<Graph {self.id!r} {'local' if self.id in self._edit._candidate['definitions'] else 'built-in, read-only'}>"


class Window(_ImmutableSnapshot):
    def __init__(self, track, revision, document, span):
        self._track, self._revision = track, revision
        self._document = copy.deepcopy(document)
        self.start_s, self.end_s = (track.seconds_at(beat) for beat in span)
        self.bars = None
        self.clips = tuple(Clip.read(id, clip) for id, clip in document["clips"].items()
                           if clip["start"] < span[1] and clip["start"]+clip["duration"] > span[0])
        self.output = TrackOutput(self)
        self._seal()

    def _render(self):
        return self._track._call("track.score_render", {"baseRevision": self._revision, "candidate": copy.deepcopy(self._document),
                                                       "startTime": self.start_s, "endTime": self.end_s})

    def timeline(self):
        import matplotlib.pyplot as plt
        fig, ax = plt.subplots(figsize=(12, 3), dpi=100)
        definitions = self._track._nodes | self._document["definitions"]
        for clip in self.clips:
            start = max(self.start_s, self._track.seconds_at(clip.start))
            end = min(self.end_s, self._track.seconds_at(clip.start+clip.duration))
            ax.barh(clip.z, end-start, left=start, color=_pattern_color(clip.graph), height=.7)
            ax.text((start+end)/2, clip.z, _label(definitions, clip.graph), ha="center", va="center", fontsize=8)
        ax.set(xlim=(self.start_s, self.end_s), xlabel="time (s)", ylabel="stack", title="Score graphs")
        fig.tight_layout()
        return fig
