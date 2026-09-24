"""Form clips over the same typed document GPUI edits.

    edit = luma.track.edit()
    form = edit.definition("color.chase@1")  # every input, with its default
    clip = edit.add_clip("color.chase@1", beats=(32, 48), selection="bars",
                         inputs={key: spec["default"] for key, spec in form["inputs"].items()})
    edit.check()
    edit.window(beats=(32, 36)).output.heatmap()
    edit.apply()

A clip plays one form: color.constant@1, color.time@1, color.space@1,
color.chase@1, color.sparkle@1, color.noise@1 or strobe.constant@1. It holds
a value for every input of its form. source() is the exact score document,
suitable for an agent workspace or a one-shot model.

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
            if value["type"] not in {"signal", "number", "beats", "proportion", "position", "degrees", "seconds", "color", "field", "mask", "color_field",
                                     "time", "hit", "noise", "audio", "events"}:
                raise TrackError("a signal socket needs a numerical value or a source")
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
        if value == "major_axis":
            source["toward"] = [0.0, 0.0, 1.0]
        if value == "vector":
            source["direction"] = [1.0, 0.0, 1.0]
        value = {"source": source, "reverse": False, "per_group": False}
        if value["source"]["kind"] in ("radial", "angle"):
            value["plane"] = {"kind": "auto"}
    if kind == "envelope" and isinstance(value, list):
        value = {"points": value}
    return {"type": kind, "value": value}


def _label(nodes, key):
    return nodes.get(key, {}).get("name") or key


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
    """The current score. An edit captures a copy; apply advances this object."""
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
        # An edit owns its candidate; only this live facade advances when the
        # worker installs a new manifest for the same score.
        for key, value in vars(snapshot).items():
            object.__setattr__(self, key, value)

    def _revoke(self):
        object.__setattr__(self, "_active", False)

    @property
    def document(self):
        """Read-only canonical score mapping; clips are keyed by ID.

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
        """Copy a built-in definition, such as a form, with typed inputs and body."""
        if id not in self._nodes:
            raise TrackError(f"unknown node definition {id!r}")
        return copy.deepcopy(self._nodes[id])

    def nodes(self, search=""):
        """Map matching node IDs to names. Read definition(id) for input schemas."""
        return {id: _label(self._nodes, id) for id in sorted(self._nodes)
                if search.casefold() in (id + " " + _label(self._nodes, id)).casefold()}

    def source(self):
        """The complete saved score as canonical JSON text."""
        return json.dumps(self._document, indent=2, sort_keys=True, allow_nan=False) + "\n"

    def edit(self):
        """Capture the complete saved score in a private Edit.

        Existing clips are included. Changes stay private until edit.apply().
        """
        self._require_active()
        if not self.editable:
            raise TrackReadOnlyError("this score is read-only")
        return Edit(self)

    def _advance(self, score):
        object.__setattr__(self, "_document", copy.deepcopy(score))
        object.__setattr__(self, "_values", dict(_items(self._values)) | {
            "document": _freeze(score),
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
        return Window(self, self._document, self._range(**range))

    def __repr__(self):
        return f"<luma.track {self.title!r} clips={len(self.clips)} editable={self.editable}>"


class Edit:
    """A complete private score candidate, including unchanged saved clips.

    Add and update clips, check(), preview through venue.render(edit=self),
    then apply(). Apply closes this edit; open a new one for further changes.
    """
    def __init__(self, track):
        self._track = track
        self._base = copy.deepcopy(track._document)
        self._candidate = copy.deepcopy(self._base)
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
        """Copy a built-in definition; a form's inputs are what its clips set."""
        return self._track.definition(id)

    def replace_source(self, source):
        """Stage an exact score source. check/apply run Rust's validator."""
        self._open()
        candidate = json.loads(source)
        if not isinstance(candidate, dict) or set(candidate) != {"clips"}:
            raise TrackError("expected a score document with only clips")
        self._candidate = candidate

    def source(self):
        """The complete candidate as JSON text, including unchanged saved clips."""
        return json.dumps(self._candidate, indent=2, sort_keys=True, allow_nan=False) + "\n"

    def _inputs(self, graph, inputs):
        schema = self.definition(graph)["inputs"]
        result = {}
        for key, value in (inputs or {}).items():
            if key not in schema:
                raise TrackError(f"{graph} has no input {key!r}")
            result[key] = _typed(schema[key]["value_type"], value)
        return result

    def add_clip(self, form, *, id=None, beats=None, bars=None, seconds=None,
                 selection="all", z=None, blend="replace", seed=None, inputs=None):
        """Stage a clip of a form ID, such as "color.chase@1", and return it.

        inputs must give a value for every input of the form; read
        definition(form)["inputs"] for their types and defaults. A missing or
        unknown input fails check(). Supply exactly one half-open range:
        beats=(0,32), bars=(1,9), or seconds=(0,16). Beats start at zero; bars
        at one. selection is a group expression; it always lights the whole
        group. Clips composite bottom-up by integer z.
        Omit z to place above clips overlapping this time range (or at zero
        when the range is empty). Supply z explicitly to choose layer order.
        replace covers the lower layer; add sums/clamps; screen brightens
        without replacing the base color. Inspect overlaps before applying.
        """
        self._open()
        graph = str(form)
        start, end = self._track._range(beats=beats, bars=bars, seconds=seconds)
        id = id or str(uuid.uuid4())
        if id in self._candidate["clips"]:
            raise TrackError(f"clip {id!r} already exists")
        if z is None:
            z = max((clip.get("z_index", 0) for clip in self._candidate["clips"].values()
                     if clip["start"] < end and start < clip["start"] + clip["duration"]),
                    default=-1) + 1
        value = {"graph": graph, "start": start, "duration": end-start,
                 "selection": _selection(selection), "z_index": _z(z), "blend_mode": _blend(blend),
                 "seed": seed if seed is not None else uuid.uuid4().int & ((1 << 64)-1),
                 "inputs": self._inputs(graph, inputs)}
        self._candidate["clips"][id] = value
        return Clip.read(id, value)

    def update_clip(self, clip, *, beats=None, bars=None, seconds=None,
                    selection=None, z=None, blend=None, seed=None, inputs=None):
        """Update a Clip or clip ID and return its new value; other fields stay.

        Ranges, selection, z and blend use add_clip's conventions. inputs
        merges values; inputs={key: None} restores that input's default.
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
            value["selection"] = _selection(selection)
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
                    spec = self.definition(value["graph"])["inputs"].get(key)
                    if spec is None:
                        raise TrackError(f"form has no input {key!r}")
                    overrides[key] = spec["default"]
        self._candidate["clips"][id] = value
        return Clip.read(id, value)

    def remove_clip(self, clip):
        """Remove a Clip or clip ID from the candidate; nothing is saved yet."""
        self._open()
        id = clip.id if isinstance(clip, Clip) else str(clip)
        if id not in self._candidate["clips"]:
            raise TrackError(f"unknown clip {id!r}")
        del self._candidate["clips"][id]

    def check(self):
        """Run the native score validator; return CheckResult without saving.

        Reports non-form clips and missing or invalid inputs.
        The validation verb is check(), not validate().
        """
        self._open()
        return _check_result(self._track._call("track.score_check", {"candidate": self.candidate}))

    def apply(self):
        """Validate and commit the complete candidate, then close this edit.

        Advances luma.track and returns the saved score. A subagent saves into
        its own draft, which its parent merges per clip when the child is done.
        """
        self._open()
        result = self._track._call("track.score_apply", {"candidate": self.candidate})
        self._track._advance(result)
        self._closed = True
        return result

    def diff(self):
        """IDs added, removed or updated versus this edit's captured base."""
        result = {}
        for kind in ("clips",):
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
        return Window(self._track, self._candidate, self._track._range(**range))

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
        return {"candidate": candidate}


# Discovery and validation use the same mode vocabulary.
Edit.add_clip.__doc__ += "\nAvailable blend modes: " + ", ".join(sorted(BLEND_MODES)) + "."


class Window(_ImmutableSnapshot):
    def __init__(self, track, document, span):
        self._track = track
        self._document = copy.deepcopy(document)
        self.start_s, self.end_s = (track.seconds_at(beat) for beat in span)
        self.bars = None
        self.clips = tuple(Clip.read(id, clip) for id, clip in document["clips"].items()
                           if clip["start"] < span[1] and clip["start"]+clip["duration"] > span[0])
        self.output = TrackOutput(self)
        self._seal()

    def _render(self):
        return self._track._call("track.score_render", {"candidate": copy.deepcopy(self._document),
                                                       "startTime": self.start_s, "endTime": self.end_s})

    def timeline(self):
        import matplotlib.pyplot as plt
        fig, ax = plt.subplots(figsize=(12, 3), dpi=100)
        for clip in self.clips:
            start = max(self.start_s, self._track.seconds_at(clip.start))
            end = min(self.end_s, self._track.seconds_at(clip.start+clip.duration))
            ax.barh(clip.z, end-start, left=start, color=_pattern_color(clip.graph), height=.7)
            ax.text((start+end)/2, clip.z, _label(self._track._nodes, clip.graph), ha="center", va="center", fontsize=8)
        ax.set(xlim=(self.start_s, self.end_s), xlabel="time (s)", ylabel="stack", title="Score clips")
        fig.tight_layout()
        return fig
