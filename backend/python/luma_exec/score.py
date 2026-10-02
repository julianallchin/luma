"""Clips over the same typed score document GPUI edits.

    edit = luma.track.edit()
    t = time(every=2)
    place = space(shift=curve(t, "Ramp up", low=-0.2, high=1), scale=0.2)
    pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]])
    clip = edit.add_clip(color(brightness=pill), name="Chase", at=("9", "13"), selection="bars")
    edit.window(at=("9", "10")).output.heatmap()
    edit.apply()

A clip has a name, a time range, a selection, a blend mode, a seed and one
graph. Build the graph with the bare builders (time, space, noise, audio,
curve, mirror, shuffle, group, split, then color, aim or strobe) and math on
curves (cut * fade, max(a, b)), or start from preset("Chase"). add_clip and update_clip run the Rust checker on
that clip at once and raise ClipError with its text.

A position is "bar.beat.16th", all three from 1, as the editor shows it.
"17" is 17.1.1. A range includes its start and excludes its end.

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

from .clip import ClipError, Graph, _preset_name, preset
from .color import from_srgb
from .host_errors import LumaHostCallError
from .track import (TrackOutput, TrackError, TrackReadOnlyError,
                    TrackClosedError, TrackHostUnavailableError,
                    _ImmutableSnapshot, _field, _items,
                    _freeze, _range_pair, _selection, _blend, _z,
                    _feature_times, _check_result, _pattern_color, _format_time_axis, BLEND_MODES,
                    BarGrid)

# How far, in beats, one clip may run past the start of the next on its track
# and still count as ending where it starts.
_TOUCH = 1e-6


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
    """One clip: id, name, at, selection, seed, z, blend, graph.

    at is the clip's range as two positions, such as ("17.1.1", "21.1.1").
    The range includes its start and excludes its end; at=clip.at places
    another clip on the same range. This is a read-only value. Use
    edit.update_clip(clip, ...) to change it. clip.graph.source() is Python
    that rebuilds the graph. The stored JSON spells z and blend as z_index
    and blend_mode, and stores start and duration in beats, where 0 is 1.1.1.
    """
    id: str
    name: str
    at: tuple
    selection: object
    seed: int
    z: int
    blend: str
    graph: Graph

    @classmethod
    def read(cls, id, value, bars):
        name = value.get("name", "")
        start, end = value["start"], value["start"] + value["duration"]
        return cls(id, name, (bars.format(start), bars.format(end)),
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
        self._beats = _feature_times(features, "beats")
        self._downbeats = _feature_times(features, "downbeats")
        self._beats_per_bar = int(_field(features, "beats_per_bar", "beatsPerBar", default=4) or 4)
        self._seal()

    def _require_active(self):
        if not self._active:
            raise TrackClosedError("this score is no longer in scope; use the current luma.track")

    @staticmethod
    def from_srgb(srgb):
        """An sRGB color, "#RRGGBB" or three channels 0..1, as the linear
        Rec. 2020 triple a score stores. A plain tuple in color() is already
        linear Rec. 2020; use this to convert an sRGB tuple first."""
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
        """All saved Clip values, ordered by start, layer z, then ID."""
        return tuple(Clip.read(id, clip, self.bars) for id, clip in sorted(
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

    @property
    def bars(self):
        """The bar grid: bar n starts at the n-th detected downbeat, as on the ruler."""
        grid = vars(self).get("_bars")
        if grid is None:
            grid = BarGrid([self._beat_at(time) for time in self._downbeats], self._beats_per_bar)
            object.__setattr__(self, "_bars", grid)
        return grid

    def position_at(self, seconds):
        """The position at a time on the track, as "bar.beat.16th".

        Seconds count from the start of the audio. The beat grid gives the
        position, so a tempo change moves it. Example: position_at(63.4)
        gives "17.2.3".
        """
        return self.bars.format(self._beat_at(seconds))

    def seconds_at(self, position):
        """The time in seconds of a position "bar.beat.16th".

        "17" is 17.1.1. The beat grid gives the time. Example:
        seconds_at("17.2") gives the time of beat 2 of bar 17.
        """
        return self._seconds_at(self.bars.parse(position))

    def _beat_at(self, seconds):
        """Seconds -> beats from the first downbeat."""
        if not math.isfinite(seconds):
            raise TrackError("time must be finite")
        origin = _field(self._values, "beat_origin_s", default=None)
        if origin is None:
            raise TrackError("musical time requires a detected beat origin")
        return self._raw_beat(seconds) - self._raw_beat(origin)

    def _seconds_at(self, beat):
        """Beats from the first downbeat -> seconds."""
        self._clock()
        if not math.isfinite(beat):
            raise TrackError("beat position must be finite")
        origin = _field(self._values, "beat_origin_s", default=None)
        if origin is None:
            raise TrackError("musical time requires a detected beat origin")
        raw = beat + self._raw_beat(origin)
        left = max(0, min(len(self._beats)-2, math.floor(raw)))
        return self._beats[left] + (raw-left) * (self._beats[left+1]-self._beats[left])

    def _range(self, *, at=None, bars=None, seconds=None):
        """One of at=, bars= or seconds= as (start, end) in beats from the first downbeat."""
        if sum(value is not None for value in (at, bars, seconds)) != 1:
            raise TrackError('specify exactly one of at=("17", "21"), bars=(17, 21) or seconds=(0, 16)')
        if at is not None:
            if isinstance(at, (str, bytes)) or len(at) != 2:
                raise TrackError('at must be a (start, end) pair of positions, such as ("17.1", "21.1")')
            start, end = (self.bars.parse(position) for position in at)
            if end <= start:
                raise TrackError(f"at {tuple(at)!r}: the end must come after the start")
            return start, end
        if bars is not None:
            start, end = _range_pair(bars, "bars")
            if start != int(start) or end != int(end):
                raise TrackError("bars are whole bar numbers, such as bars=(17, 21); use at= inside a bar")
            return self.bars.start(int(start)), self.bars.start(int(end))
        start, end = _range_pair(seconds, "seconds")
        return self._beat_at(start), self._beat_at(end)

    def window(self, **range):
        """Inspect an at=, bars= or seconds= range of the saved score."""
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
        return tuple(self._read(id, clip) for id, clip in self._candidate["clips"].items())

    def _read(self, id, value):
        return Clip.read(id, value, self._track.bars)

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

    def add_clip(self, graph, *, name=None, at=None, bars=None, seconds=None,
                 selection="all", z=None, blend=None, seed=None, id=None):
        """Stage a clip and return it. The checker runs on it at once.

        graph is a Graph from color(), aim() or strobe(), preset("Chase"),
        or a preset name. name is required unless the graph is a preset;
        a preset also gives its blend mode (motion presets are offset).
        With no blend and no preset, the blend is replace.
        Supply exactly one range: at=("17.1", "21.1") with positions
        "bar.beat.16th" (all from 1; "17" is 17.1.1), bars=(17, 21), or
        seconds=(0, 16). A range includes its start and excludes its end.
        selection is a group expression. Clips composite bottom-up by integer z; omit z to
        place above clips overlapping this range. One z is one track, as in
        Ableton: clips at one z never overlap, whatever lights they select. A
        clip placed on or stretched into another clip at its z cuts that clip
        and replaces it there; each cut prints one line. replace covers the lower
        layer; add sums; screen brightens; an aim clip takes replace or
        offset (offset adds its yaw and pitch to the aim underneath).
        """
        self._open()
        graph = _graph(graph)
        name = name if name is not None else graph.name
        if not name:
            raise ClipError('clip: expected a name; got none. Example: name="Kick chase"')
        start, end = self._track._range(at=at, bars=bars, seconds=seconds)
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
        self._report(self._cut(id, value))
        self._candidate["clips"][id] = value
        return self._read(id, value)

    @staticmethod
    def _report(cuts):
        """Print each cut, so a caller sees what a new or moved clip took away."""
        for line in cuts:
            print(line)

    def _cut(self, id, value):
        """Cut away the part of every other clip at value's z that value covers.

        A z is one track and its clips never overlap. A clip the range covers
        whole is removed; one it covers in part is trimmed, or split in two
        with the far piece as a new clip. Returns one line per clip it changed.
        """
        z, start = value.get("z_index", 0), value["start"]
        end = start + value["duration"]
        clips = self._candidate["clips"]
        cuts = []
        for other_id, other in list(clips.items()):
            other_start = other["start"]
            other_end = other_start + other["duration"]
            if (other_id == id or other.get("z_index", 0) != z
                    or not (other_start < end - _TOUCH and start < other_end - _TOUCH)):
                continue
            del clips[other_id]
            head, tail = start - other_start, other_end - end
            if head > _TOUCH:
                clips[other_id] = dict(other, duration=head)
            if tail > _TOUCH:
                piece = dict(copy.deepcopy(other), start=end, duration=tail)
                clips[str(uuid.uuid4()) if head > _TOUCH else other_id] = piece
            label = f"clip {other.get('name') or other_id} ({other_id}) at z {z}"
            kept = [self._track.bars.format_range(a, a + d) for a, d in
                    ((other_start, head), (end, tail)) if d > _TOUCH]
            cuts.append(f"cut: {label} is now {' and '.join(kept)}" if kept
                        else f"cut: removed {label}; the new clip covers all of it")
        return cuts

    def update_clip(self, clip, *, graph=None, name=None, at=None, bars=None, seconds=None,
                    selection=None, z=None, blend=None, seed=None):
        """Update a Clip or clip ID and return its new value; other fields stay.

        graph, name, range, selection, z, blend and seed use add_clip's
        conventions, so the clip cuts what it covers at its z. The checker
        runs on the updated clip at once.
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
        if any(item is not None for item in (at, bars, seconds)):
            start, end = self._track._range(at=at, bars=bars, seconds=seconds)
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
        self._report(self._cut(id, value))
        self._candidate["clips"][id] = value
        return self._read(id, value)

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

    def diff(self, full=False):
        """What this edit changes against the score it captured.

        A short plan: a count line, then one line per clip, sorted by start.
        `+` is added, `-` removed, `~` changed. A changed clip shows only the
        fields that changed, as before → after, and its graph per node and
        input. A clip that a cut split shows as one line. full=True gives the
        raw stored clips: {"added": {id: clip}, "removed": {id: clip},
        "changed": {id: {"before": clip, "after": clip}}}.
        """
        before, after = self._base["clips"], self._candidate["clips"]
        added = {id: after[id] for id in after.keys() - before.keys()}
        removed = {id: before[id] for id in before.keys() - after.keys()}
        changed = {id: {"before": before[id], "after": after[id]}
                   for id in before.keys() & after.keys() if before[id] != after[id]}
        if full:
            return copy.deepcopy({"added": added, "removed": removed, "changed": changed})
        return _Plan(self._track.bars, after, added, removed, changed).text()

    def window(self, **range):
        """Inspect an at=, bars= or seconds= range of the complete candidate.

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


class Diff(str):
    """Plain text that shows as itself, not as a quoted string."""

    def __repr__(self):
        return str(self)


#: At most this many clip lines in edit.diff(); full=True has every clip.
_DIFF_LINES = 200


class _Plan:
    """edit.diff() as text: a count line and one line per clip."""

    def __init__(self, bars, candidate, added, removed, changed):
        self._bars, self._candidate = bars, candidate
        self._added, self._removed, self._changed = dict(added), removed, changed
        self._pieces = self._splits()

    def _splits(self):
        """Added clips that are the far piece of a changed clip a cut split."""
        pieces = {}
        for id, value in list(self._added.items()):
            for old_id, change in self._changed.items():
                old, new = change["before"], change["after"]
                if (_same_but_range(value, old) and _same_but_range(new, old)
                        and old["start"] - _TOUCH <= value["start"]
                        and value["start"] + value["duration"] <= old["start"] + old["duration"] + _TOUCH):
                    pieces.setdefault(old_id, []).append(id)
                    del self._added[id]
                    break
        return pieces

    def text(self):
        rows = [(value["start"], "+", self._line("+", id, value)) for id, value in self._added.items()]
        rows += [(value["start"], "-", self._line("-", id, value)) for id, value in self._removed.items()]
        rows += [(change["after"]["start"], "~", self._changed_line(id, change))
                 for id, change in self._changed.items()]
        rows.sort(key=lambda row: (row[0], row[1]))
        counts = (f"{len(self._added)} added, {len(self._removed)} removed, "
                  f"{len(self._changed)} changed")
        lines = [counts] + [line for _, _, line in rows[:_DIFF_LINES]]
        if len(rows) > _DIFF_LINES:
            lines.append(f"…and {len(rows) - _DIFF_LINES} more; diff(full=True) has every clip")
        return Diff("\n".join(lines))

    def _range(self, value):
        return self._bars.format_range(value["start"], value["start"] + value["duration"])

    def _line(self, sign, id, value):
        name = value.get("name") or id
        selection = (value.get("selection") or {}).get("expression", "all")
        return f"{sign} {name} {self._range(value)} z {value.get('z_index', 0)} {selection}"

    def _changed_line(self, id, change):
        old, new = change["before"], change["after"]
        pieces = self._pieces.get(id)
        if pieces:
            cutter = self._cutter(old, [id, *pieces])
            ranges = ", ".join(self._range(value) for value in
                               sorted([new, *(self._candidate[p] for p in pieces)], key=lambda v: v["start"]))
            return f"{self._line('~', id, old)}: split by {cutter} → {len(pieces) + 1} clips ({ranges})"
        notes = []
        if old.get("name") != new.get("name"):
            notes.append(f"name {old.get('name')!r} → {new.get('name')!r}")
        if (old["start"], old["duration"]) != (new["start"], new["duration"]):
            notes.append(f"at {self._range(old)} → {self._range(new)}")
        for key, label in (("z_index", "z"), ("blend_mode", "blend"), ("seed", "seed")):
            if old.get(key) != new.get(key):
                notes.append(f"{label} {old.get(key)} → {new.get(key)}")
        if old.get("selection") != new.get("selection"):
            notes.append(f"selection {(old.get('selection') or {}).get('expression')!r} → "
                         f"{(new.get('selection') or {}).get('expression')!r}")
        notes += _graph_changes(old.get("graph") or {}, new.get("graph") or {})
        return f"{self._line('~', id, new)}: " + "; ".join(notes)

    def _cutter(self, old, pieces):
        """The name of the candidate clip at old's z that sits inside old's range."""
        start, end = old["start"], old["start"] + old["duration"]
        for id, value in self._candidate.items():
            if (id not in pieces and value.get("z_index", 0) == old.get("z_index", 0)
                    and start < value["start"] + value["duration"] and value["start"] < end):
                return value.get("name") or id
        return "a cut"


def _same_but_range(a, b):
    """Two clips that differ at most in start and duration."""
    return {k: v for k, v in a.items() if k not in ("start", "duration")} == \
        {k: v for k, v in b.items() if k not in ("start", "duration")}


def _graph_changes(old, new):
    """Per node and input: what changed in a clip graph, as short notes."""
    before, after = old.get("nodes") or {}, new.get("nodes") or {}
    readers = {}
    for node_id, node in after.items():
        if node.get("kind") in ("color", "aim", "strobe"):
            for name, value in (node.get("inputs") or {}).items():
                for wire in _wires(value):
                    readers.setdefault(wire, name)
    notes = [f"-node {id}" for id in sorted(before.keys() - after.keys())]
    notes += [f"+node {id}" for id in sorted(after.keys() - before.keys())]
    for id in sorted(before.keys() & after.keys()):
        a, b = before[id], after[id]
        if a == b:
            continue
        prefix = f"{readers[id]}: " if id in readers else ""
        if a.get("kind") != b.get("kind"):
            notes.append(f"{prefix}{id} kind {a.get('kind')} → {b.get('kind')}")
            continue
        for field in ("settings", "inputs"):
            old_values, new_values = a.get(field) or {}, b.get(field) or {}
            for name in sorted(old_values.keys() | new_values.keys()):
                if old_values.get(name) != new_values.get(name):
                    notes.append(f"{prefix}{id} {name} {_brief(old_values.get(name))} → "
                                 f"{_brief(new_values.get(name))}")
    return notes


def _wires(value):
    if isinstance(value, dict) and set(value) == {"node"}:
        return [value["node"]]
    if isinstance(value, list):
        return [wire for item in value for wire in _wires(item)]
    return []


def _brief(value):
    """A stored input value in a few characters: a preset name, a wire, a number."""
    if value is None:
        return "empty"
    if isinstance(value, dict) and set(value) == {"node"}:
        return value["node"]
    if isinstance(value, dict) and set(value) == {"points"}:
        name = _preset_name("curves", value["points"])
        return f'"{name}"' if name else f"{len(value['points'])} points"
    if isinstance(value, dict) and set(value) == {"stops"}:
        name = _preset_name("gradients", value["stops"])
        return f'"{name}"' if name else f"{len(value['stops'])} stops"
    if isinstance(value, float):
        return f"{value:g}"
    if isinstance(value, list) and all(isinstance(item, (int, float)) for item in value):
        return "(" + ", ".join(f"{item:g}" for item in value) + ")"
    text = json.dumps(value, separators=(",", ":"))
    return text if len(text) <= 40 else text[:39] + "…"


class Window(_ImmutableSnapshot):
    """One range of a score: its clips, and its composited output.

    at is the range as two positions; bars is the same range in bars from 1,
    such as (17, 21). start_s and end_s are the range in seconds.
    """

    def __init__(self, track, document, span):
        self._track = track
        self._document = copy.deepcopy(document)
        start, end = span
        self.start_s, self.end_s = track._seconds_at(start), track._seconds_at(end)
        if track.duration_s > 0 and self.end_s > track.duration_s:
            if self.start_s >= track.duration_s:
                raise TrackError(f"window starts at {self.start_s:.3f} s, after the track ends at {track.duration_s:.3f} s")
            self.end_s = track.duration_s
            end = track._beat_at(self.end_s)
            print(f"window: clamped to the end of the track: {self.end_s:.3f} s")
        grid = track.bars
        self.at = (grid.format(start), grid.format(end))
        self.bars = (grid.bar_number(start), grid.bar_number(end))
        self.clips = tuple(Clip.read(id, clip, grid) for id, clip in document["clips"].items()
                           if clip["start"] < end and clip["start"]+clip["duration"] > start)
        self.output = TrackOutput(self)
        self._seal()

    def _render(self):
        return self._track._call("track.score_render", {"candidate": copy.deepcopy(self._document),
                                                       "startTime": self.start_s, "endTime": self.end_s})

    def timeline(self):
        import matplotlib.pyplot as plt
        fig, ax = plt.subplots(figsize=(12, 3), dpi=100)
        for clip in self.clips:
            stored = self._document["clips"][clip.id]
            start = max(self.start_s, self._track._seconds_at(stored["start"]))
            end = min(self.end_s, self._track._seconds_at(stored["start"] + stored["duration"]))
            kind = clip.graph.output.kind if clip.graph.output is not None else "empty"
            ax.barh(clip.z, end-start, left=start, color=_pattern_color(kind), height=.7)
            ax.text((start+end)/2, clip.z, clip.name or kind, ha="center", va="center", fontsize=8)
        ax.set(xlim=(self.start_s, self.end_s), ylabel="z", title=f"Score clips · {'-'.join(self.at)}")
        _format_time_axis(ax, self)
        fig.tight_layout()
        return fig
