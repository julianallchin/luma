"""The agent-facing lighting-track editor.

This module is deliberately independent of the worker protocol and of Luma's
database.  ``Track`` is built from ordinary binding values and a synchronous
``host_call(method, payload)`` capability.  The Python object owns the cheap,
local work (candidate editing, windows, diffs, and figures); the host owns the
authoritative work (compilation, compositing, validation, and atomic apply).

Host calls
----------

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

    @property
    def selection(self) -> str | None:
        """The group expression this clip targets, if it has exactly one
        Selection argument."""
        value = self._selection_value()
        return None if value is None else str(_field(value, "expression", default=""))

    @property
    def subset(self) -> Any:
        """How much of the selection the clip lights: ``"all"``, a float
        fraction, or an integer count.  ``None`` when the clip has no single
        Selection argument."""
        value = self._selection_value()
        if value is None:
            return None
        raw = _field(value, "subset", default="all")
        if isinstance(raw, Mapping):
            if "fraction" in raw:
                return float(raw["fraction"])
            if "count" in raw:
                return int(raw["count"])
        return "all"

    def _selection_value(self) -> Mapping[str, Any] | None:
        if not isinstance(self.args, Mapping):
            return None
        found = [
            value
            for value in self.args.values()
            if isinstance(value, Mapping) and "expression" in value
        ]
        return found[0] if len(found) == 1 else None

    def __repr__(self) -> str:
        name = self.pattern_name or self.pattern_id
        return (
            f"<Clip {self.id!r} {name!r} {self.start_s:g}..{self.end_s:g}s "
            f"z={self.z} {self.blend}>"
        )


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

    def selection_arg_id(self, pattern_id: str) -> str:
        """The pattern's sole Selection argument id.  A pattern with none, or
        with several, cannot take the ``selection=`` shorthand at all."""
        ids = [
            str(_required(d, "id"))
            for d in _sequence(self._schemas.get(pattern_id, []))
            if str(_field(d, "arg_type", "argType", default="")).casefold() == "selection"
        ]
        if len(ids) != 1:
            detail = "none" if not ids else "more than one"
            raise TrackError(
                f"pattern {pattern_id!r} has {detail} Selection argument; "
                "put the value in args by argument id"
            )
        return ids[0]

    def normalize_args(
        self,
        pattern_id: str,
        args: Mapping[str, Any] | None,
        selection: str | None,
        subset: Any = None,
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
            normalized[self.selection_arg_id(pattern_id)] = _selection(selection, subset)
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


class TrackOutput:
    """The real composited RGB output of one candidate window, loaded lazily."""

    def __init__(self, window) -> None:
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
        response = self._window._render()
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


def _selection(expression: str, subset: Any = None) -> dict[str, Any]:
    expression = str(expression).strip()
    if not expression:
        raise TrackError("Selection expression cannot be empty")
    value: dict[str, Any] = {"expression": expression}
    if subset is not None:
        value["subset"] = _subset(subset)
    return value


def _subset(value: Any) -> Any:
    """Which fixtures of the expression to keep: ``"all"``, a float share, or an
    integer count.  A float is a fraction of the matched set (``0.5`` = half);
    an int is a fixed number of fixtures.  Omitting it entirely means all."""
    if value is None or (isinstance(value, str) and value.strip().casefold() == "all"):
        return "all"
    if isinstance(value, bool):
        raise TrackError(f"invalid subset {value!r}; expected 'all', a fraction, or a count")
    if isinstance(value, int):
        if value < 1:
            raise TrackError(f"subset count must be at least 1, got {value}")
        return {"count": value}
    if isinstance(value, float):
        if not 0.0 < value <= 1.0:
            raise TrackError(f"subset fraction must be in (0, 1], got {value}")
        return {"fraction": value}
    raise TrackError(f"invalid subset {value!r}; expected 'all', a fraction, or a count")


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


def _format_time_axis(ax: Any, window) -> None:
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


def _window_label(window) -> str:
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
