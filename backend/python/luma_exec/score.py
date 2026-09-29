"""Clips over the same typed score document GPUI edits.

    edit = luma.track.edit()
    k = clock(every=2)
    pos = curve(time(k), "Ramp up", low=-0.2, high=1)
    clip = edit.add_clip(color(brightness=curve(space(offset=pos, width=0.2), "On")),
                         name="Chase", beats=(32, 48), selection="bars")
    edit.window(beats=(32, 36)).output.heatmap()
    edit.apply()

A clip has a name, a time range, a selection, a blend mode, a seed and one
graph. Build the graph with the bare builders (clock, time, space, noise,
audio, curve, mirror, shuffle, group, split, then color, aim or strobe), or
start from preset("Chase"). add_clip and update_clip run the Rust checker on
that clip at once and raise ClipError with its text.

A color is light in linear Rec. 2020, three channels 0..1. "#RRGGBB" is sRGB
and is converted. clip.graph.source() gives Python that rebuilds a clip's
graph; edit.source() is the exact score document.
"""
from __future__ import annotations

import bisect
import copy
import json
import math
import uuid
from dataclasses import dataclass

from .clip import ClipError, Graph, preset
from .color import from_srgb
from .host_errors import LumaHostCallError
from .track import (TrackOutput, TrackError, TrackReadOnlyError,
                    TrackClosedError, TrackHostUnavailableError,
                    _ImmutableSnapshot, _field, _items,
                    _freeze, _range_pair, _selection, _blend, _z,
                    _downbeat_values, _check_result, _pattern_color, BLEND_MODES)


def _plain(value):
    if hasattr(value, "items"):
        return {key: _plain(item) for key, item in _items(value)}
    if isinstance(value, (list, tuple)):
        return [_plain(item) for item in value]
    return copy.deepcopy(value)


def _graph(value):
    """A Graph from a Graph, or a preset name."""
    if isinstance(value, Graph):
        return value
    if isinstance(value, str):
        return preset(value)
    raise TrackError("graph must be a Graph from color(), aim() or strobe(), preset(\"Chase\"), "
                     "or a preset name")


def _seed(value):
    if value is None:
        return uuid.uuid4().int & ((1 << 64) - 1)
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value < (1 << 64):
        raise TrackError("seed needs an integer from 0 through 18446744073709551615")
    return value


@dataclass(frozen=True)
class Clip:
    """One clip: id, name, start/duration in beats, selection, seed, z, blend, graph.

    This is a read-only value. Use edit.update_clip(clip, ...) to change it.
    clip.graph.source() is Python that rebuilds the graph. The stored JSON
    spells z and blend as z_index and blend_mode.
    """
    id: str
    name: str
    start: float
    duration: float
    selection: object
    seed: int
    z: int
    blend: str
    graph: Graph

    @classmethod
    def read(cls, id, value):
        name = value.get("name", "")
        return cls(id, name, value["start"], value["duration"],
                   _freeze(value.get("selection", {"expression": "all"})), value["seed"],
                   value.get("z_index", 0), value.get("blend_mode", "replace"),
                   Graph.from_json(value.get("graph") or {}, name=name or None))


def _definitions(nodes):
    """Definitions keyed by kind; the binding may be a map or a list of records."""
    nodes = _plain(nodes) or {}
    if isinstance(nodes, list):
        return {record["kind"]: record for record in nodes}
    return nodes


class GraphTrack(_ImmutableSnapshot):
    """The current score. An edit captures a copy; apply advances this object."""

    def __init__(self, values, *, nodes, features=None, host_call=None, artifact_store=None):
        self._values, self._features = values, features
        self._host_call, self._artifact_store = host_call, artifact_store
        self._nodes = _definitions(nodes)
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

    @staticmethod
    def color(srgb):
        """An sRGB color, "#RRGGBB" or three channels 0..1, as the linear
        Rec. 2020 triple a score stores."""
        try:
            return from_srgb(srgb)
        except ValueError as error:
            raise TrackError(str(error)) from None

    def _call(self, method, payload):
        self._require_active()
        if self._host_call is None:
            raise TrackHostUnavailableError(
                f"{method} requires Luma's host; this track has no host_call capability"
            )
        return self._host_call(method, payload)

    def __getattr__(self, name):
        """Preserve ordinary scalar track bindings (album, bpm, key, ...)."""
        if name.startswith("_"):
            raise AttributeError(name)
        missing = object()
        value = _field(self._values, name, default=missing)
        if value is missing:
            raise AttributeError(f"luma.track has no binding {name!r}")
        return value

    def __dir__(self):
        names = set(object.__dir__(self))
        try:
            names.update(str(key) for key, _ in _items(self._values))
        except TrackError:
            pass
        return sorted(names)

    def _luma_catalog_items(self):
        """Binding inventory hook; keeps ``luma.catalog()`` domain-neutral."""
        return _items(self._values)

    def _bar_time(self, bar):
        """1-indexed fractional bar boundary -> seconds, with edge extrapolation."""
        downbeats = self._downbeats
        index = math.floor(bar - 1.0)
        fraction = bar - 1.0 - index
        if len(downbeats) == 1:
            bpm = float(_field(self._features, "bpm", default=120.0) or 120.0)
            beats_per_bar = float(
                _field(self._features, "beats_per_bar", "beatsPerBar", default=4.0)
                or 4.0
            )
            span = beats_per_bar * 60.0 / bpm
            return downbeats[0] + (index + fraction) * span
        if index < 0:
            return downbeats[0] + (index + fraction) * (downbeats[1] - downbeats[0])
        if index + 1 < len(downbeats):
            return downbeats[index] + fraction * (downbeats[index + 1] - downbeats[index])
        span = downbeats[-1] - downbeats[-2]
        return downbeats[-1] + (index - (len(downbeats) - 1) + fraction) * span

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

    def definition(self, kind):
        """The definition record of a node kind: its inputs (type, unit,
        range, default) and settings (options, default). A default of None
        means the input is empty."""
        if kind not in self._nodes:
            raise TrackError(f"unknown node kind {kind!r}; luma.track.nodes() lists them")
        return copy.deepcopy(self._nodes[kind])

    def nodes(self):
        """The node kinds a clip graph is built from."""
        return list(self._nodes)

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

    def definition(self, kind):
        """The definition record of a node kind (inputs, settings, defaults)."""
        return self._track.definition(kind)

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

    def _check_clip(self, id, value):
        """Run the Rust checker on one clip; raise ClipError with its text."""
        label = value.get("name") or id
        try:
            result = self._track._call("track.clip_check", {"id": id, "clip": copy.deepcopy(value)})
        except LumaHostCallError as error:
            if error.code in ("invalid_clip", "invalid_score", "invalid_request"):
                raise ClipError(_prefixed(label, id, str(error))) from None
            raise
        if isinstance(result, dict) and result.get("error"):
            raise ClipError(_prefixed(label, id, str(result["error"])))
        result = _check_result(result)
        if not result.ok:
            raise ClipError("\n".join(_prefixed(label, id, message) for message in result.errors))

    def add_clip(self, graph, *, name=None, beats=None, bars=None, seconds=None,
                 selection="all", z=None, blend=None, seed=None, id=None):
        """Stage a clip and return it. The checker runs on it at once.

        graph is a Graph from color(), aim() or strobe(), preset("Chase"),
        or a preset name. name is required unless the graph is a preset;
        a preset also gives its blend mode (motion presets are offset).
        With no blend and no preset, the blend is replace.
        Supply exactly one half-open range: beats=(0,32), bars=(1,9), or
        seconds=(0,16). Beats start at zero; bars at one. selection is a
        group expression. Clips composite bottom-up by integer z; omit z to
        place above clips overlapping this range. replace covers the lower
        layer; add sums; screen brightens; an aim clip takes replace or
        offset (offset adds its yaw and pitch to the aim underneath).
        """
        self._open()
        graph = _graph(graph)
        name = name if name is not None else graph.name
        if not name:
            raise ClipError('clip: expected a name; got none. Example: name="Kick chase"')
        start, end = self._track._range(beats=beats, bars=bars, seconds=seconds)
        id = id or str(uuid.uuid4())
        if id in self._candidate["clips"]:
            raise TrackError(f"clip {id!r} already exists")
        if z is None:
            z = max((clip.get("z_index", 0) for clip in self._candidate["clips"].values()
                     if clip["start"] < end and start < clip["start"] + clip["duration"]),
                    default=-1) + 1
        value = {"name": str(name), "start": start, "duration": end - start, "seed": _seed(seed),
                 "selection": _selection(selection), "z_index": _z(z), "blend_mode": _blend(blend or graph.blend or "replace"),
                 "graph": graph.json()}
        self._check_clip(id, value)
        self._candidate["clips"][id] = value
        return Clip.read(id, value)

    def update_clip(self, clip, *, graph=None, name=None, beats=None, bars=None, seconds=None,
                    selection=None, z=None, blend=None, seed=None):
        """Update a Clip or clip ID and return its new value; other fields stay.

        graph, name, range, selection, z, blend and seed use add_clip's
        conventions. The checker runs on the updated clip at once.
        """
        self._open()
        id = clip.id if isinstance(clip, Clip) else str(clip)
        if id not in self._candidate["clips"]:
            raise TrackError(f"unknown clip {id!r}")
        value = copy.deepcopy(self._candidate["clips"][id])
        if graph is not None:
            graph = _graph(graph)
            value["graph"] = graph.json()
            if name is None and not value.get("name") and graph.name:
                value["name"] = graph.name
        if name is not None:
            if not str(name):
                raise ClipError('clip: expected a name; got none. Example: name="Kick chase"')
            value["name"] = str(name)
        if any(item is not None for item in (beats, bars, seconds)):
            start, end = self._track._range(beats=beats, bars=bars, seconds=seconds)
            value.update(start=start, duration=end - start)
        if selection is not None:
            value["selection"] = _selection(selection)
        if z is not None:
            value["z_index"] = _z(z)
        if blend is not None:
            value["blend_mode"] = _blend(blend)
        if seed is not None:
            value["seed"] = _seed(seed)
        self._check_clip(id, value)
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

        Reports every clip the checker refuses. The verb is check(), not validate().
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
        """Snapshot for the venue camera; isolation never mutates the draft.

        A bare score document (`{"clips": ...}`), matching what
        `RenderRequest.edit` deserializes on the Rust side — unlike
        `score_check`/`score_apply`, which want `{"candidate": ...}`.
        """
        self._open()
        candidate = self.candidate
        if only is not None:
            clips = [only] if isinstance(only, (Clip, str)) else list(only)
            ids = [clip.id if isinstance(clip, Clip) else str(clip) for clip in clips]
            missing = [id for id in ids if id not in candidate["clips"]]
            if missing:
                raise TrackError(f"unknown preview clip(s): {', '.join(missing)}")
            candidate["clips"] = {id: candidate["clips"][id] for id in ids}
        return candidate


# Discovery and validation use the same mode vocabulary.
Edit.add_clip.__doc__ += ("\nAvailable blend modes: " + ", ".join(sorted(BLEND_MODES)) + "."
                          " An aim clip takes replace or offset; offset is for aim only.")


def _prefixed(label, id, message):
    return message if message.startswith("clip ") else f"clip {label} ({id}): {message}"


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
            kind = clip.graph.output.kind if clip.graph.output is not None else "empty"
            ax.barh(clip.z, end-start, left=start, color=_pattern_color(kind), height=.7)
            ax.text((start+end)/2, clip.z, clip.name or kind, ha="center", va="center", fontsize=8)
        ax.set(xlim=(self.start_s, self.end_s), xlabel="time (s)", ylabel="stack", title="Score clips")
        fig.tight_layout()
        return fig
