"""The agent-facing lighting-track editor.

This module is deliberately independent of the worker protocol and of Luma's
database.  ``Track`` is built from ordinary binding values and a synchronous
``host_call(method, payload)`` capability.  The Python object owns the cheap,
local work (candidate editing, windows, diffs, and figures); the host owns the
authoritative work (compilation, compositing, validation, and atomic apply).

The public shape is intentionally small::

    edit = luma.track.edit()
    edit.add_clip("Blue wash", bars=(17, 25), z=0,   # id or unique name
                  selection="front_wash")
    edit.update_clip("clip-id", args={"Intensity": .7})
    edit.remove_clip("another-id")

    view = edit.window(bars=(17, 25))       # half-open: bars 17 through 24
    view.timeline()                         # authored clips, time x z
    view.output.heatmap()                   # real candidate composite

    edit.diff()
    edit.check()
    edit.apply()

An ``Edit`` always contains the *whole* candidate track.  A window therefore
includes unchanged clips which intersect it, not only clips touched by the
edit.  ``Clip`` is an immutable value and ``Track`` changes only when an
``Edit`` is applied, which advances it to the revision it committed; authoring
is otherwise confined to the explicit ``Edit`` object.

``pattern_id`` accepts a pattern id or an unambiguous pattern name, and is the
same identity a clip carries as ``Clip.pattern_id``; ``clip_id`` likewise
accepts a ``Clip``.

Host calls
----------

``track.check`` and ``track.apply`` receive exactly::

    {"baseRevision": str, "candidate": [TrackClip, ...]}

``track.render`` receives the same fields plus half-open ``startTime`` and
``endTime`` in absolute seconds.  ``TrackClip`` is the transaction wire shape::

    {"id", "patternId", "startTime", "endTime", "zIndex",
     "blendMode", "args"}

The canonical render response reuses the normal Luma artifact system::

    {
      "tensor": {"$kind": "tensor", "artifact_id": ..., "dtype": "f32",
                 "shape": [light, time, channel], "axes": [...]},
      "artifact": {"id": ..., "kind": "tensor", "encoding": "raw_le",
                   "rel_path": ..., "byte_len": ...}
    }

The tensor is registered into the ``ArtifactStore`` passed to ``Track`` and
materialized as the ordinary lazy, read-only ``LumaTensor``.  Tests and other
in-process callers may return an ndarray, a LumaTensor, or
``{"values": array, "lightIds": [...], "timesS": [...]}`` instead.
"""

from __future__ import annotations

import copy
import hashlib
import math
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any


HostCall = Callable[[str, Any], Any]

#: Clips staged by an edit carry a provisional id until `apply` mints the real
#: one. The prefix is what makes a staged id recognisable in a diff or error.
TEMP_ID_PREFIX = "new:"

BLEND_MODES = frozenset(
    {
        "replace",
        "add",
        "multiply",
        "screen",
        "max",
        "min",
        "lighten",
        "value",
        "subtract",
    }
)


class TrackError(ValueError):
    """An invalid local track operation."""


class TrackReadOnlyError(TrackError):
    """The bound track cannot be changed by this agent thread."""


class TrackClosedError(TrackError):
    """An edit was used after it had been applied."""


class TrackHostUnavailableError(RuntimeError):
    """An operation needs Luma's host but no host capability was installed."""


class _ImmutableSnapshot:
    """A public value object whose fields are fixed after construction.

    Python's ``object.__setattr__`` remains available to deliberately hostile
    code, as it does for frozen dataclasses. This guard owns the ordinary API
    invariant: an agent cannot accidentally turn a snapshot assignment into a
    later full-candidate mutation.
    """

    __slots__ = ()

    def _seal(self) -> None:
        object.__setattr__(self, "_sealed", True)

    def __setattr__(self, name: str, value: Any) -> None:
        try:
            sealed = object.__getattribute__(self, "_sealed")
        except AttributeError:
            sealed = False
        if sealed:
            raise AttributeError(f"{type(self).__name__} is immutable")
        object.__setattr__(self, name, value)

    def __delattr__(self, name: str) -> None:
        try:
            sealed = object.__getattribute__(self, "_sealed")
        except AttributeError:
            sealed = False
        if sealed:
            raise AttributeError(f"{type(self).__name__} is immutable")
        object.__delattr__(self, name)


@dataclass(frozen=True, slots=True)
class Clip:
    """One immutable authored clip in absolute track time."""

    id: str
    pattern_id: str
    pattern_name: str | None
    start_s: float
    end_s: float
    z: int
    blend: str
    # Persisted legacy rows may contain any JSON value. New/edited argument
    # sets are objects, but merely viewing or moving a legacy clip must remain
    # lossless.
    args: Any

    @classmethod
    def from_value(cls, value: Any) -> "Clip":
        return cls(
            id=str(_required(value, "id")),
            pattern_id=str(_required(value, "pattern_id", "patternId")),
            pattern_name=_optional_string(value, "pattern_name", "patternName"),
            start_s=float(
                _required(value, "start_s", "startTime", "start_time_s", "start_time")
            ),
            end_s=float(
                _required(value, "end_s", "endTime", "end_time_s", "end_time")
            ),
            z=int(_required(value, "z", "zIndex", "z_index")),
            blend=str(_field(value, "blend", "blendMode", "blend_mode", default="replace")),
            args=_freeze(_field(value, "args", default={})),
        )

    def to_wire(self) -> dict[str, Any]:
        """The sole candidate representation accepted by the Rust transaction."""
        return {
            "id": self.id,
            "patternId": self.pattern_id,
            "startTime": self.start_s,
            "endTime": self.end_s,
            "zIndex": self.z,
            "blendMode": self.blend,
            "args": _thaw(self.args),
        }

    def __repr__(self) -> str:
        name = self.pattern_name or self.pattern_id
        return (
            f"<Clip {self.id!r} {name!r} {self.start_s:g}..{self.end_s:g}s "
            f"z={self.z} {self.blend}>"
        )


@dataclass(frozen=True, slots=True)
class ClipChange:
    before: Clip
    after: Clip


@dataclass(frozen=True, slots=True)
class TrackDiff:
    added: tuple[Clip, ...]
    updated: tuple[ClipChange, ...]
    removed: tuple[Clip, ...]

    @property
    def changed(self) -> bool:
        return bool(self.added or self.updated or self.removed)

    def __bool__(self) -> bool:
        return self.changed

    def __repr__(self) -> str:
        header = (
            f"<TrackDiff +{len(self.added)} ~{len(self.updated)} "
            f"-{len(self.removed)}>"
        )
        lines = [header]
        lines.extend(f"  + {_clip_line(c)}" for c in self.added)
        lines.extend(
            f"  ~ {_clip_line(change.after)}" for change in self.updated
        )
        lines.extend(f"  - {_clip_line(c)}" for c in self.removed)
        return "\n".join(lines)


@dataclass(frozen=True, slots=True)
class CheckResult:
    ok: bool
    errors: tuple[str, ...] = ()
    warnings: tuple[str, ...] = ()

    def __bool__(self) -> bool:
        return self.ok

    def __repr__(self) -> str:
        if self.ok and not self.warnings:
            return "<CheckResult ok>"
        state = "ok" if self.ok else "failed"
        lines = [f"<CheckResult {state}>"]
        lines.extend(f"  error: {message}" for message in self.errors)
        lines.extend(f"  warning: {message}" for message in self.warnings)
        return "\n".join(lines)


@dataclass(frozen=True, slots=True)
class ApplyResult:
    revision: str
    clips: tuple[Clip, ...]
    id_map: Mapping[str, str]
    added: int
    updated: int
    removed: int
    applied: bool = True

    def __repr__(self) -> str:
        if not self.applied:
            return f"<ApplyResult unchanged revision={self.revision!r}>"
        return (
            f"<ApplyResult revision={self.revision!r} +{self.added} "
            f"~{self.updated} -{self.removed}>"
        )


class _PatternCatalog:
    def __init__(self, patterns: Any) -> None:
        summaries = _field(patterns, "summaries", default=[]) if patterns is not None else []
        schemas = (
            _field(patterns, "argument_schemas", "argumentSchemas", default={})
            if patterns is not None
            else {}
        )

        self._by_id: dict[str, Any] = {}
        self._ids_by_name: dict[str, list[str]] = {}
        for summary in _sequence(summaries):
            pattern_id = str(_required(summary, "id"))
            self._by_id[pattern_id] = summary
            name = str(_field(summary, "name", default=pattern_id))
            self._ids_by_name.setdefault(name.casefold(), []).append(pattern_id)

        self._schemas = dict(_items(schemas))

    def resolve(self, reference: str) -> tuple[str, str | None]:
        reference = str(reference)
        if reference in self._by_id:
            return reference, self.name(reference)
        matches = self._ids_by_name.get(reference.casefold(), [])
        if len(matches) == 1:
            pattern_id = matches[0]
            return pattern_id, self.name(pattern_id)
        if len(matches) > 1:
            raise TrackError(
                f"pattern name {reference!r} is ambiguous; use one of these ids: "
                + ", ".join(matches)
            )
        raise TrackError(f"unknown pattern {reference!r}; use a pattern id or unique name")

    def name(self, pattern_id: str) -> str | None:
        summary = self._by_id.get(pattern_id)
        if summary is None:
            return None
        return str(_field(summary, "name", default=pattern_id))

    def normalize_args(
        self,
        pattern_id: str,
        args: Mapping[str, Any] | None,
        selection: str | None,
    ) -> dict[str, Any]:
        definitions = list(_sequence(self._schemas.get(pattern_id, [])))
        by_id = {str(_required(d, "id")): d for d in definitions}
        ids_by_name: dict[str, list[str]] = {}
        for definition in definitions:
            arg_id = str(_required(definition, "id"))
            name = str(_field(definition, "name", default=arg_id))
            ids_by_name.setdefault(name.casefold(), []).append(arg_id)

        normalized: dict[str, Any] = {}
        for raw_key, value in (args or {}).items():
            key = str(raw_key)
            if key not in by_id:
                matches = ids_by_name.get(key.casefold(), [])
                if len(matches) == 1:
                    key = matches[0]
                elif len(matches) > 1:
                    raise TrackError(
                        f"argument name {raw_key!r} is ambiguous for pattern "
                        f"{pattern_id!r}; use an argument id"
                    )
                else:
                    raise TrackError(
                        f"unknown argument {raw_key!r} for pattern {pattern_id!r}; "
                        "use an argument id or unique display name"
                    )
            normalized[key] = self._normalize_arg_value(by_id.get(key), value)

        if selection is not None:
            selection_ids = [
                str(_required(d, "id"))
                for d in definitions
                if str(_field(d, "arg_type", "argType", default="")).casefold()
                == "selection"
            ]
            if len(selection_ids) != 1:
                detail = "none" if not selection_ids else "more than one"
                raise TrackError(
                    f"pattern {pattern_id!r} has {detail} Selection argument; "
                    "put the value in args by argument id"
                )
            normalized[selection_ids[0]] = _selection(selection)
        return normalized

    @staticmethod
    def _normalize_arg_value(definition: Any, value: Any) -> Any:
        if definition is None:
            return _thaw(value)
        arg_type = str(_field(definition, "arg_type", "argType", default=""))
        if arg_type.casefold() == "selection" and isinstance(value, str):
            return _selection(value)
        return _thaw(value)


class Track(_ImmutableSnapshot):
    """The current authored lighting track.

    Immutable except through `Edit.apply`, which advances it to the revision
    it just committed: `luma.track` always names the live score, whether the
    host installed it at the start of the cell or an apply moved it forward
    mid-cell.
    """

    __slots__ = (
        "_values",
        "_patterns",
        "_features",
        "_host_call",
        "_artifact_store",
        "id",
        "title",
        "artist",
        "duration_s",
        "revision",
        "editable",
        "clips",
        "_downbeats",
        "_sealed",
    )

    def __init__(
        self,
        values: Any,
        *,
        patterns: Any = None,
        features: Any = None,
        host_call: HostCall | None = None,
        artifact_store: Any = None,
    ) -> None:
        self._values = values
        self._patterns = _PatternCatalog(patterns)
        self._features = features
        self._host_call = host_call
        self._artifact_store = artifact_store

        self.id = str(_field(values, "id", default=""))
        self.title = str(_field(values, "title", default=""))
        self.artist = str(_field(values, "artist", default=""))
        self.duration_s = float(_field(values, "duration_s", default=0.0) or 0.0)
        self.revision = str(_required(values, "revision"))
        self.editable = bool(_field(values, "editable", default=False))
        self.clips = _canonical_clips(
            self._clip(value)
            for value in _sequence(_field(values, "clips", default=[]))
        )
        self._downbeats = _downbeat_values(features)
        self._seal()

    def edit(self) -> "Edit":
        if not self.editable:
            raise TrackReadOnlyError("this track is read-only")
        return Edit(self)

    def _advance(self, revision: str, clips: tuple[Clip, ...]) -> None:
        """Adopt the authoritative document a successful apply just produced.

        `luma.track` names the *current* authored score, not a photograph of it
        taken when the cell began. The host reinstalls it between cells, so
        without this an apply would leave the rest of its own cell holding a
        superseded revision: every later `edit()`, `window()` or `heatmap()`
        would be built on it and rejected by the host as a conflict.

        Everything else in the snapshot — title, duration, patterns, features —
        is unchanged by an apply, so only identity moves forward.
        """
        object.__setattr__(self, "revision", revision)
        object.__setattr__(self, "clips", clips)

    def window(
        self,
        *,
        bars: tuple[float, float] | None = None,
        seconds: tuple[float, float] | None = None,
    ) -> "TrackWindow":
        start_s, end_s = self._resolve_range(bars=bars, seconds=seconds)
        return TrackWindow(
            track=self,
            candidate=self.clips,
            start_s=start_s,
            end_s=end_s,
            bars=bars,
        )

    def _resolve_range(
        self,
        *,
        bars: tuple[float, float] | None,
        seconds: tuple[float, float] | None,
    ) -> tuple[float, float]:
        if (bars is None) == (seconds is None):
            raise TrackError("specify exactly one of bars=(start, end) or seconds=(start, end)")
        if bars is not None:
            start, end = _range_pair(bars, "bars")
            if not self._downbeats:
                raise TrackError("bar ranges require luma.features.downbeats")
            return self._bar_time(start), self._bar_time(end)
        assert seconds is not None
        return _range_pair(seconds, "seconds")

    def _bar_time(self, bar: float) -> float:
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

    def _pattern(self, reference: str) -> tuple[str, str | None]:
        return self._patterns.resolve(reference)

    def _clip(self, value: Any) -> Clip:
        clip = Clip.from_value(value)
        pattern_name = clip.pattern_name or self._patterns.name(clip.pattern_id)
        if pattern_name == clip.pattern_name:
            return clip
        return Clip(
            id=clip.id,
            pattern_id=clip.pattern_id,
            pattern_name=pattern_name,
            start_s=clip.start_s,
            end_s=clip.end_s,
            z=clip.z,
            blend=clip.blend,
            args=clip.args,
        )

    def _call(self, method: str, payload: Any) -> Any:
        if self._host_call is None:
            raise TrackHostUnavailableError(
                f"{method} requires Luma's host; this Track has no host_call capability"
            )
        return self._host_call(method, payload)

    def __getattr__(self, name: str) -> Any:
        """Preserve ordinary scalar track bindings (album, bpm, key, ...)."""
        if name.startswith("_"):
            raise AttributeError(name)
        missing = object()
        value = _field(self._values, name, default=missing)
        if value is missing:
            raise AttributeError(f"luma.track has no binding {name!r}")
        return value

    def __dir__(self) -> list[str]:
        names = set(object.__dir__(self))
        try:
            names.update(str(key) for key, _ in _items(self._values))
        except TrackError:
            pass
        return sorted(names)

    def _luma_catalog_items(self) -> list[tuple[Any, Any]]:
        """Binding inventory hook; keeps ``luma.catalog()`` domain-neutral."""
        return _items(self._values)

    def __repr__(self) -> str:
        return (
            f"<luma.track {self.title!r} revision={self.revision!r} "
            f"clips={len(self.clips)} editable={self.editable}>"
        )


class Edit:
    """The sole mutable object: a staged, complete candidate track."""

    def __init__(self, track: Track) -> None:
        self._track = track
        self._base = track.clips
        self._clips: dict[str, Clip] = {clip.id: clip for clip in track.clips}
        self._closed = False
        self._next_temp = 1

    @property
    def base_revision(self) -> str:
        return self._track.revision

    @property
    def clips(self) -> tuple[Clip, ...]:
        return _canonical_clips(self._clips.values())

    @property
    def candidate(self) -> tuple[Clip, ...]:
        return self.clips

    def add_clip(
        self,
        pattern_id: str,
        *,
        bars: tuple[float, float] | None = None,
        seconds: tuple[float, float] | None = None,
        z: int,
        blend: str = "replace",
        args: Mapping[str, Any] | None = None,
        selection: str | None = None,
    ) -> Clip:
        self._ensure_open()
        pattern_id, pattern_name = self._track._pattern(pattern_id)
        start_s, end_s = self._track._resolve_range(bars=bars, seconds=seconds)
        blend = _blend(blend)
        z = _z(z)
        normalized = self._track._patterns.normalize_args(pattern_id, args, selection)
        clip_id = self._temp_id()
        clip = Clip(
            id=clip_id,
            pattern_id=pattern_id,
            pattern_name=pattern_name,
            start_s=start_s,
            end_s=end_s,
            z=z,
            blend=blend,
            args=_freeze_mapping(normalized),
        )
        self._clips[clip.id] = clip
        return clip

    def update_clip(
        self,
        clip_id: str | Clip,
        *,
        pattern_id: str | None = None,
        bars: tuple[float, float] | None = None,
        seconds: tuple[float, float] | None = None,
        z: int | None = None,
        blend: str | None = None,
        args: Mapping[str, Any] | None = None,
        unset_args: Iterable[str] = (),
        selection: str | None = None,
    ) -> Clip:
        self._ensure_open()
        before = self._find(clip_id)
        if bars is not None or seconds is not None:
            start_s, end_s = self._track._resolve_range(bars=bars, seconds=seconds)
        else:
            start_s, end_s = before.start_s, before.end_s

        if pattern_id is None:
            pattern_id, pattern_name = before.pattern_id, before.pattern_name
            prior_args = _thaw(before.args)
        else:
            pattern_id, pattern_name = self._track._pattern(pattern_id)
            # Argument ids belong to a pattern. Carrying them across a pattern
            # replacement is almost always an accidental, invalid mutation.
            prior_args = {} if pattern_id != before.pattern_id else _thaw(before.args)

        unset_args = tuple(unset_args)
        touches_args = args is not None or selection is not None or bool(unset_args)
        if not touches_args:
            next_args = prior_args
        else:
            if not isinstance(prior_args, Mapping):
                raise TrackError(
                    f"clip {clip_id!r} has legacy non-object args; change its pattern "
                    "before setting arguments"
                )
            merged_args = dict(prior_args)
            normalized = self._track._patterns.normalize_args(pattern_id, args, selection)
            merged_args.update(normalized)
            next_args = merged_args
        for key in unset_args:
            resolved = self._track._patterns.normalize_args(pattern_id, {str(key): None}, None)
            for arg_id in resolved:
                next_args.pop(arg_id, None)

        after = Clip(
            id=before.id,
            pattern_id=pattern_id,
            pattern_name=pattern_name,
            start_s=start_s,
            end_s=end_s,
            z=before.z if z is None else _z(z),
            blend=before.blend if blend is None else _blend(blend),
            args=_freeze(next_args),
        )
        self._clips[before.id] = after
        return after

    def remove_clip(self, clip_id: str | Clip) -> Clip:
        self._ensure_open()
        clip = self._find(clip_id)
        del self._clips[clip.id]
        return clip

    def window(
        self,
        *,
        bars: tuple[float, float] | None = None,
        seconds: tuple[float, float] | None = None,
    ) -> "TrackWindow":
        self._ensure_open()
        start_s, end_s = self._track._resolve_range(bars=bars, seconds=seconds)
        # Snapshot the complete candidate now. Subsequent edits require a new
        # window, keeping timeline and output figures internally consistent.
        return TrackWindow(
            track=self._track,
            candidate=self.clips,
            start_s=start_s,
            end_s=end_s,
            bars=bars,
        )

    def diff(self) -> TrackDiff:
        base = {clip.id: clip for clip in self._base}
        current = self._clips
        added = _canonical_clips(
            clip for clip_id, clip in current.items() if clip_id not in base
        )
        removed = _canonical_clips(
            clip for clip_id, clip in base.items() if clip_id not in current
        )
        updated = tuple(
            ClipChange(base[clip_id], current[clip_id])
            for clip_id in sorted(base.keys() & current.keys())
            if _semantic_key(base[clip_id]) != _semantic_key(current[clip_id])
        )
        return TrackDiff(added=added, updated=updated, removed=removed)

    def check(self) -> CheckResult:
        self._ensure_open()
        local = self._local_check()
        if not local.ok:
            return local
        try:
            response = self._track._call("track.check", self._plan())
        except Exception as error:
            from .host_errors import LumaHostCallError

            if isinstance(error, LumaHostCallError) and error.code in {
                "invalid_edit",
                "compile_error",
            }:
                return CheckResult(False, (str(error),))
            raise
        return _check_result(response)

    def apply(self) -> ApplyResult:
        self._ensure_open()
        local = self._local_check()
        if not local.ok:
            raise TrackError("candidate is invalid:\n" + "\n".join(local.errors))
        difference = self.diff()
        response = self._track._call("track.apply", self._plan())
        if not isinstance(response, Mapping):
            raise RuntimeError("track.apply returned an invalid response")
        revision = str(_required(response, "revision"))
        clips = _canonical_clips(
            self._track._clip(value)
            for value in _sequence(_field(response, "clips", default=[]))
        )
        id_map = MappingProxyType(
            {
                str(key): str(value)
                for key, value in _items(_field(response, "idMap", "id_map", default={}))
            }
        )
        added = int(_field(response, "added", default=len(difference.added)))
        updated = int(_field(response, "updated", default=len(difference.updated)))
        removed = int(_field(response, "removed", default=len(difference.removed)))
        self._closed = True
        self._track._advance(revision, clips)
        return ApplyResult(
            revision=revision,
            clips=clips,
            id_map=id_map,
            added=added,
            updated=updated,
            removed=removed,
            applied=bool(added or updated or removed),
        )

    def _plan(self) -> dict[str, Any]:
        return {
            "baseRevision": self.base_revision,
            "candidate": [clip.to_wire() for clip in self.clips],
        }

    def _local_check(self) -> CheckResult:
        errors: list[str] = []
        clips = self.clips
        base_by_id = {clip.id: clip for clip in self._base}
        for clip in clips:
            if not clip.id:
                errors.append("clip id cannot be empty")
            if not clip.pattern_id:
                errors.append(f"clip {clip.id!r} has no pattern")
            if not math.isfinite(clip.start_s) or not math.isfinite(clip.end_s):
                errors.append(f"clip {clip.id!r} has non-finite time")
            elif clip.end_s <= clip.start_s:
                errors.append(f"clip {clip.id!r} must end after it starts")
            before = base_by_id.get(clip.id)
            preserves_legacy_range = before is not None and (
                clip.start_s == before.start_s and clip.end_s == before.end_s
            )
            if not preserves_legacy_range:
                if clip.start_s < 0.0:
                    errors.append(f"clip {clip.id!r} starts before the track")
                if clip.end_s > self._track.duration_s:
                    errors.append(
                        f"clip {clip.id!r} ends after the track "
                        f"({clip.end_s:g}s > {self._track.duration_s:g}s)"
                    )
            if clip.blend not in BLEND_MODES:
                errors.append(f"clip {clip.id!r} has unknown blend {clip.blend!r}")
            try:
                import json

                json.dumps(_thaw(clip.args), allow_nan=False)
            except (TypeError, ValueError) as exc:
                errors.append(f"clip {clip.id!r} args are not JSON: {exc}")

        # Legacy tracks may already contain same-layer overlaps. Preserve them,
        # matching the Rust transaction, but do not allow an edit to introduce
        # a new overlapping pair.
        new_overlaps = _overlap_pairs(clips) - _overlap_pairs(self._base)
        for left, right in sorted(new_overlaps):
            z = self._clips[left].z
            errors.append(f"clips {left!r} and {right!r} overlap on z={z}")
        return CheckResult(ok=not errors, errors=tuple(errors))

    def _find(self, clip_id: str | Clip) -> Clip:
        """Resolve a clip reference against the candidate.

        Accepts the `Clip` itself as well as its id: a clip taken from
        `edit.clips` or returned by `add_clip` is always a usable reference,
        so the common way of naming a staged clip cannot be mistyped.
        """
        if isinstance(clip_id, Clip):
            clip_id = clip_id.id
        key = str(clip_id)
        try:
            return self._clips[key]
        except KeyError:
            staged = f"{TEMP_ID_PREFIX}{key}"
            if staged in self._clips:
                raise TrackError(
                    f"unknown clip id {key!r}; the staged clip is {staged!r} "
                    "— pass clip.id or the Clip itself"
                ) from None
            raise TrackError(f"unknown clip id {key!r}") from None

    def _temp_id(self) -> str:
        while True:
            candidate = f"{TEMP_ID_PREFIX}{self._next_temp}"
            self._next_temp += 1
            if candidate not in self._clips:
                return candidate

    def _ensure_open(self) -> None:
        if self._closed:
            raise TrackClosedError("this edit is closed; create a new edit from luma.track")

    def __repr__(self) -> str:
        state = "closed" if self._closed else "open"
        diff = self.diff()
        return (
            f"<TrackEdit {state} base={self.base_revision!r} clips={len(self._clips)} "
            f"+{len(diff.added)} ~{len(diff.updated)} -{len(diff.removed)}>"
        )


class TrackWindow(_ImmutableSnapshot):
    """An immutable candidate snapshot over one explicit half-open interval."""

    __slots__ = (
        "_track",
        "_candidate",
        "start_s",
        "end_s",
        "bars",
        "clips",
        "output",
        "_sealed",
    )

    def __init__(
        self,
        *,
        track: Track,
        candidate: Sequence[Clip],
        start_s: float,
        end_s: float,
        bars: tuple[float, float] | None,
    ) -> None:
        self._track = track
        self._candidate = tuple(candidate)
        self.start_s = start_s
        self.end_s = end_s
        self.bars = tuple(bars) if bars is not None else None
        self.clips = tuple(
            clip
            for clip in self._candidate
            if clip.end_s > self.start_s and clip.start_s < self.end_s
        )
        self.output = TrackOutput(self)
        self._seal()

    def timeline(self) -> Any:
        """Draw authored clips with x=time and y=explicit stack order."""
        import matplotlib.patches as patches
        import matplotlib.pyplot as plt

        z_values = sorted({clip.z for clip in self.clips})
        row_of = {z: row for row, z in enumerate(z_values)}
        height = min(10.0, max(2.8, 1.2 + 0.65 * max(1, len(z_values))))
        fig, ax = plt.subplots(figsize=(12, height), dpi=100)

        for clip in self.clips:
            left = max(self.start_s, clip.start_s)
            right = min(self.end_s, clip.end_s)
            row = row_of[clip.z]
            rectangle = patches.Rectangle(
                (left, row - 0.34),
                right - left,
                0.68,
                facecolor=_pattern_color(clip.pattern_id),
                edgecolor=(1.0, 1.0, 1.0, 0.7),
                linewidth=0.7,
            )
            ax.add_patch(rectangle)
            label = f"{clip.pattern_name or clip.pattern_id} · {clip.id}"
            ax.text(
                (left + right) / 2.0,
                row,
                label,
                ha="center",
                va="center",
                fontsize=7,
                color="white",
                clip_on=True,
            )
            if clip.start_s < self.start_s:
                ax.text(left, row, "←", ha="left", va="center", color="white", fontsize=9)
            if clip.end_s > self.end_s:
                ax.text(right, row, "→", ha="right", va="center", color="white", fontsize=9)

        ax.set_xlim(self.start_s, self.end_s)
        ax.set_ylim(-0.75, max(0.75, len(z_values) - 0.25))
        ax.set_yticks(range(len(z_values)), [f"z={z}" for z in z_values])
        ax.set_ylabel("stack")
        _format_time_axis(ax, self)
        ax.set_title(f"Authored lighting · {_window_label(self)}")
        ax.grid(axis="x", color="white", alpha=0.12, linewidth=0.7)
        if not self.clips:
            ax.text(
                0.5,
                0.5,
                "no clips in this window",
                transform=ax.transAxes,
                ha="center",
                va="center",
                alpha=0.65,
            )
        fig.tight_layout()
        return fig

    def __repr__(self) -> str:
        return (
            f"<TrackWindow {self.start_s:g}..{self.end_s:g}s "
            f"clips={len(self.clips)} candidate={len(self._candidate)}>"
        )


class TrackOutput:
    """The real composited RGB output of one candidate window, loaded lazily."""

    def __init__(self, window: TrackWindow) -> None:
        self._window = window
        self._tensor: Any = None
        self._values: Any = None
        self._light_ids: list[str] | None = None
        self._times_s: Any = None

    @property
    def tensor(self) -> Any:
        self._load()
        return self._tensor

    @property
    def values(self) -> Any:
        self._load()
        return self._values

    @property
    def light_ids(self) -> list[str] | None:
        self._load()
        return list(self._light_ids) if self._light_ids is not None else None

    @property
    def times_s(self) -> Any:
        self._load()
        return self._times_s

    def heatmap(self) -> Any:
        """Plot final composited light color over the window (x=time, y=light)."""
        import matplotlib.pyplot as plt
        import numpy as np

        values = np.asarray(self.values)
        if values.ndim == 2:
            values = np.repeat(values[..., None], 3, axis=2)
        if values.ndim != 3 or values.shape[2] < 3:
            raise TrackError(
                "track.render tensor must have shape [light, time, channel>=3]"
            )
        rgb = np.clip(values[:, :, :3], 0.0, 1.0)
        light_count = rgb.shape[0]
        height = min(12.0, max(3.0, 1.8 + light_count * 0.16))
        fig, ax = plt.subplots(figsize=(12, height), dpi=100)
        ax.imshow(
            rgb,
            aspect="auto",
            interpolation="nearest",
            origin="upper",
            extent=(
                self._window.start_s,
                self._window.end_s,
                light_count - 0.5,
                -0.5,
            ),
        )
        labels = self.light_ids or [str(index) for index in range(light_count)]
        if labels:
            stride = max(1, math.ceil(len(labels) / 28))
            rows = list(range(0, len(labels), stride))
            ax.set_yticks(rows, [_short_light(labels[row]) for row in rows])
        ax.set_ylabel("light")
        _format_time_axis(ax, self._window)
        ax.set_title(f"Composited lighting · {_window_label(self._window)}")
        fig.tight_layout()
        return fig

    def _load(self) -> None:
        if self._values is not None:
            return
        window = self._window
        payload = {
            "baseRevision": window._track.revision,
            "candidate": [clip.to_wire() for clip in window._candidate],
            "startTime": window.start_s,
            "endTime": window.end_s,
        }
        response = window._track._call("track.render", payload)
        self._install(response)

    def _install(self, response: Any) -> None:
        import numpy as np

        value = response
        if isinstance(response, Mapping):
            tensor_spec = _field(response, "tensor", default=None)
            artifact = _field(response, "artifact", default=None)
            if isinstance(tensor_spec, Mapping) and tensor_spec.get("$kind") == "tensor":
                store = self._window._track._artifact_store
                if store is None:
                    raise TrackHostUnavailableError(
                        "track.render returned an artifact tensor, but Track has no artifact_store"
                    )
                artifact_id = str(
                    _field(tensor_spec, "artifact_id", "artifactId", default="")
                )
                if not artifact_id:
                    raise RuntimeError("track.render tensor has no artifact_id")
                if not isinstance(artifact, Mapping):
                    raise RuntimeError("track.render tensor has no artifact descriptor")
                descriptor = dict(artifact)
                descriptor.pop("id", None)
                store.artifacts[artifact_id] = descriptor
                from .bindings import LumaTensor

                value = LumaTensor(
                    tensor_spec,
                    store,
                    "luma.track.window.output",
                )
            elif tensor_spec is not None:
                value = tensor_spec
            else:
                value = _field(response, "values", "rgb", default=response)
            self._light_ids = _string_list(
                _field(response, "lightIds", "light_ids", default=None)
            )
            self._times_s = _field(response, "timesS", "times_s", default=None)

        self._tensor = value
        raw_values = getattr(value, "values", value)
        values = np.asarray(raw_values)
        values.flags.writeable = False
        self._values = values

        if self._light_ids is None:
            self._light_ids = _axis_labels(value, "light") or _axis_labels(
                value, "primitive"
            )
        if self._times_s is None:
            self._times_s = getattr(value, "times_s", None)

    def __repr__(self) -> str:
        if self._values is None:
            return "<TrackOutput lazy>"
        return f"<TrackOutput shape={tuple(self._values.shape)}>"


def _check_result(response: Any) -> CheckResult:
    if isinstance(response, CheckResult):
        return response
    if response is None or response is True:
        return CheckResult(True)
    if response is False:
        return CheckResult(False, ("host rejected the candidate",))
    if not isinstance(response, Mapping):
        raise RuntimeError("track.check returned an invalid response")
    errors = tuple(str(x) for x in _sequence(_field(response, "errors", default=[])))
    warnings = tuple(str(x) for x in _sequence(_field(response, "warnings", default=[])))
    ok = bool(_field(response, "ok", default=not errors))
    return CheckResult(ok=ok and not errors, errors=errors, warnings=warnings)


def _canonical_clips(clips: Iterable[Clip]) -> tuple[Clip, ...]:
    return tuple(sorted(clips, key=lambda clip: (clip.start_s, clip.z, clip.id)))


def _semantic_key(clip: Clip) -> tuple[Any, ...]:
    return (
        clip.pattern_id,
        clip.start_s,
        clip.end_s,
        clip.z,
        clip.blend,
        _json_key(clip.args),
    )


def _json_key(value: Any) -> str:
    import json

    return json.dumps(_thaw(value), sort_keys=True, separators=(",", ":"), default=repr)


def _clip_line(clip: Clip) -> str:
    return (
        f"{clip.id} {clip.pattern_name or clip.pattern_id} "
        f"{clip.start_s:g}..{clip.end_s:g}s z={clip.z} {clip.blend}"
    )


def _selection(expression: str) -> dict[str, str]:
    expression = str(expression).strip()
    if not expression:
        raise TrackError("Selection expression cannot be empty")
    return {"expression": expression, "spatialReference": "global"}


def _blend(value: str) -> str:
    value = str(value).lower()
    if value not in BLEND_MODES:
        raise TrackError(
            f"unknown blend {value!r}; expected one of {', '.join(sorted(BLEND_MODES))}"
        )
    return value


def _z(value: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TrackError("z must be an integer")
    return value


def _range_pair(value: Sequence[float], name: str) -> tuple[float, float]:
    if isinstance(value, (str, bytes)) or len(value) != 2:
        raise TrackError(f"{name} must be a (start, end) pair")
    start, end = float(value[0]), float(value[1])
    if not math.isfinite(start) or not math.isfinite(end):
        raise TrackError(f"{name} boundaries must be finite")
    if end <= start:
        raise TrackError(f"{name} is half-open and requires end > start")
    return start, end


def _downbeat_values(features: Any) -> tuple[float, ...]:
    try:
        downbeats = (
            _field(features, "downbeats", default=None) if features is not None else None
        )
    except Exception:
        return ()
    if downbeats is None:
        return ()
    try:
        values = getattr(downbeats, "values", downbeats)
    except Exception:
        return ()
    if callable(values):
        return ()
    try:
        return tuple(float(value) for value in values)
    except (TypeError, ValueError):
        return ()


def _overlap_pairs(clips: Iterable[Clip]) -> set[tuple[str, str]]:
    values = tuple(clips)
    pairs: set[tuple[str, str]] = set()
    for index, left in enumerate(values):
        for right in values[index + 1 :]:
            if left.z != right.z:
                continue
            if left.start_s < right.end_s and right.start_s < left.end_s:
                pairs.add(tuple(sorted((left.id, right.id))))
    return pairs


def _format_time_axis(ax: Any, window: TrackWindow) -> None:
    if window.bars is None:
        ax.set_xlabel("time (s)")
        return
    start, end = window.bars
    first = math.ceil(start)
    last = math.floor(end)
    integers = list(range(first, last + 1))
    stride = max(1, math.ceil(len(integers) / 12))
    shown = integers[::stride]
    if integers and integers[-1] == end and (not shown or shown[-1] != integers[-1]):
        shown.append(integers[-1])
    ax.set_xticks([window._track._bar_time(bar) for bar in shown], [str(bar) for bar in shown])
    ax.set_xlabel("bar (end exclusive)")


def _window_label(window: TrackWindow) -> str:
    if window.bars is not None:
        return f"bars [{window.bars[0]:g}, {window.bars[1]:g})"
    return f"seconds [{window.start_s:g}, {window.end_s:g})"


def _pattern_color(pattern_id: str) -> tuple[float, float, float, float]:
    digest = hashlib.sha256(pattern_id.encode("utf-8")).digest()
    # Avoid colors too close to black while remaining stable across processes.
    return tuple(0.28 + component / 255.0 * 0.62 for component in digest[:3]) + (0.9,)


def _short_light(light_id: str) -> str:
    if len(light_id) <= 28:
        return light_id
    return f"{light_id[:12]}…{light_id[-10:]}"


def _axis_labels(value: Any, name: str) -> list[str] | None:
    axis_method = getattr(value, "axis", None)
    if not callable(axis_method):
        return None
    axis = axis_method(name)
    if axis is None:
        return None
    labels = getattr(axis, "labels", None)
    if labels is None:
        return None
    return [str(label) for label in labels]


def _string_list(value: Any) -> list[str] | None:
    if value is None:
        return None
    return [str(item) for item in value]


def _field(value: Any, *names: str, default: Any = None) -> Any:
    if value is None:
        return default
    if isinstance(value, Mapping):
        for name in names:
            if name in value:
                return value[name]
        return default
    for name in names:
        try:
            return getattr(value, name)
        except AttributeError:
            pass
    return default


def _required(value: Any, *names: str) -> Any:
    missing = object()
    result = _field(value, *names, default=missing)
    if result is missing:
        raise TrackError(f"missing required field {names[0]!r}")
    return result


def _optional_string(value: Any, *names: str) -> str | None:
    result = _field(value, *names, default=None)
    return None if result is None else str(result)


def _items(value: Any) -> list[tuple[Any, Any]]:
    if value is None:
        return []
    if isinstance(value, Mapping):
        return list(value.items())
    method = getattr(value, "items", None)
    if callable(method):
        return list(method())
    raise TrackError(f"expected a mapping, got {type(value).__name__}")


def _sequence(value: Any) -> list[Any]:
    if value is None:
        return []
    if isinstance(value, (str, bytes, Mapping)):
        raise TrackError(f"expected a sequence, got {type(value).__name__}")
    return list(value)


def _freeze_mapping(value: Any) -> Mapping[str, Any]:
    if value is None:
        return MappingProxyType({})
    return MappingProxyType({str(key): _freeze(item) for key, item in _items(value)})


def _freeze(value: Any) -> Any:
    if isinstance(value, Mapping) or callable(getattr(value, "items", None)):
        return MappingProxyType({str(key): _freeze(item) for key, item in _items(value)})
    if isinstance(value, list):
        return tuple(_freeze(item) for item in value)
    if isinstance(value, tuple):
        return tuple(_freeze(item) for item in value)
    return copy.deepcopy(value)


def _thaw(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _thaw(item) for key, item in value.items()}
    if isinstance(value, tuple):
        return [_thaw(item) for item in value]
    return copy.deepcopy(value)
