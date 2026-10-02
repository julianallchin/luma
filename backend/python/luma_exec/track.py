"""Shared pieces of the agent-facing score editor in ``score.py``.

This module is independent of the worker protocol and of Luma's database. It
holds the errors, the immutable-snapshot guard, check results, the lazily
loaded composited output of a window, and small value helpers.

Host calls
----------

The canonical render response reuses the normal Luma artifact system::

    {
      "tensor": {"$kind": "tensor", "artifact_id": ..., "dtype": "f32",
                 "shape": [primitive, time, channel], "axes": [...]},
      "artifact": {"id": ..., "kind": "tensor", "encoding": "raw_le",
                   "rel_path": ..., "byte_len": ...}
    }

The tensor is registered into the track's ``ArtifactStore`` and materialized
as the ordinary lazy, read-only ``LumaTensor``. Tests and other in-process
callers may return an ndarray, a LumaTensor, or
``{"values": array, "primitiveIds": [...], "timesS": [...]}`` instead.
"""

from __future__ import annotations

import bisect
import copy
import hashlib
import math
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any


BLEND_MODES = frozenset(
    {
        "replace",
        "add",
        "multiply",
        "screen",
        "max",
        "min",
        "subtract",
        "offset",
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


# The 12 lighting channels, in the host's order, with the labels they may carry.
_CHANNELS = {
    "r": (0, ("r", "red")), "g": (1, ("g", "green")), "b": (2, ("b", "blue")),
    "dimmer": (3, ("dimmer",)), "strobe": (6, ("strobe",)),
    "aim_u": (8, ("aim_u", "u")), "aim_v": (9, ("aim_v", "v")), "aim_z": (10, ("aim_z", "z")),
    "weight": (11, ("weight", "aim_weight")),
}


@dataclass(frozen=True)
class AimOutput:
    """Composited aim: `values` [primitive, time, 3] unit vectors in UVZ, `weight` [primitive, time]."""
    values: Any
    weight: Any


@dataclass(frozen=True)
class StrobeOutput:
    """Composited strobe: `values` [primitive, time], shutter 0..1."""
    values: Any


class TrackOutput:
    """The real composited output of one candidate window, loaded lazily.

    `values` is light color [primitive, time, rgb]: linear Rec. 2020, already
    darkened by the dimmer. A primitive is one light cell, as in the venue.
    `primitive_ids` names them. `aim` and `strobe` hold the other channels.
    """

    def __init__(self, window) -> None:
        self._window = window
        self._tensor: Any = None
        self._values: Any = None
        self._channels: Any = None
        self._channel_labels: list[str] | None = None
        self._primitive_ids: list[str] | None = None
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
    def aim(self) -> AimOutput:
        """Aim per primitive and time: unit vectors in UVZ plus the aim weight."""
        import numpy as np

        self._load()
        vector = np.stack([self._channel("aim_u"), self._channel("aim_v"), self._channel("aim_z")], axis=-1)
        return AimOutput(_readonly(vector), _readonly(self._channel("weight")))

    @property
    def strobe(self) -> StrobeOutput:
        """Strobe shutter per light and time, 0..1."""
        self._load()
        return StrobeOutput(_readonly(self._channel("strobe")))

    def _channel(self, name: str) -> Any:
        index, labels = _CHANNELS[name]
        known = self._channel_labels or []
        for label in labels:
            if label in known:
                return self._channels[:, :, known.index(label)]
        if self._channels.ndim != 3 or self._channels.shape[2] <= index or (known and len(known) <= 3):
            raise TrackError(f"the host rendered no {name} channel; this render has RGB only")
        return self._channels[:, :, index]

    @property
    def primitive_ids(self) -> list[str] | None:
        self._load()
        return list(self._primitive_ids) if self._primitive_ids is not None else None

    @property
    def times_s(self) -> Any:
        self._load()
        return self._times_s

    def heatmap(self) -> Any:
        """Plot final composited light color over the window (x=time, y=light).

        The values are light in linear Rec. 2020; the plot shows them in sRGB.
        """
        import matplotlib.pyplot as plt
        import numpy as np

        from .color import to_display

        values = np.asarray(self.values)
        if values.ndim == 2:
            values = np.repeat(values[..., None], 3, axis=2)
        if values.ndim != 3 or values.shape[2] < 3:
            raise TrackError(
                "track.score_render tensor must have shape [primitive, time, channel>=3]"
            )
        rgb = to_display(np.clip(values[:, :, :3], 0.0, 1.0))
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
        labels = self.primitive_ids or [str(index) for index in range(light_count)]
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
                        "track.score_render returned an artifact tensor, but the track has no artifact_store"
                    )
                artifact_id = str(
                    _field(tensor_spec, "artifact_id", "artifactId", default="")
                )
                if not artifact_id:
                    raise RuntimeError("track.score_render tensor has no artifact_id")
                if not isinstance(artifact, Mapping):
                    raise RuntimeError("track.score_render tensor has no artifact descriptor")
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
            self._primitive_ids = _string_list(
                _field(response, "primitiveIds", default=None)
            )
            self._times_s = _field(response, "timesS", "times_s", default=None)

        self._tensor = value
        raw_values = getattr(value, "values", value)
        channels = np.asarray(raw_values)
        self._channels = channels
        self._channel_labels = _axis_labels(value, "channel")
        if isinstance(response, Mapping) and self._channel_labels is None:
            self._channel_labels = _string_list(_field(response, "channels", default=None))
        if channels.ndim == 3 and channels.shape[2] > 3:
            # The full lighting tensor; its r, g, b already carry the dimmer.
            values = np.stack([self._channel(name) for name in ("r", "g", "b")], axis=-1)
        else:
            values = channels
        self._values = _readonly(values)

        if self._primitive_ids is None:
            self._primitive_ids = _axis_labels(value, "primitive")
        if self._times_s is None:
            self._times_s = getattr(value, "times_s", None)

    def __repr__(self) -> str:
        if self._values is None:
            return "<TrackOutput lazy>"
        return f"<TrackOutput shape={tuple(self._values.shape)}>"


def _readonly(values: Any) -> Any:
    import numpy as np

    values = np.array(values)
    values.flags.writeable = False
    return values


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


def _selection(expression: str) -> dict[str, Any]:
    expression = str(expression).strip()
    if not expression:
        raise TrackError("Selection expression cannot be empty")
    return {"expression": expression}


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


_POSITION = re.compile(r"\s*(-?\d+)(?:\.(\d+)(?:\.(\d+(?:\.\d+)?))?)?\s*")


class BarGrid:
    """Bars as the editor ruler draws them, and positions `bar.beat.16th`.

    Bar n starts at the n-th detected downbeat. `starts` holds those starts
    in beats from the first downbeat, so bar 1 starts at 0. Beats inside a
    bar are the detected beats; a 16th is a quarter of a beat. Before the
    first downbeat and after the last, bars are `beats_per_bar` long.
    Bar, beat and 16th all count from 1. A position between 16ths has a
    fraction: 17.2.3.5 is half way from 16th 3 to 16th 4.
    """

    def __init__(self, starts, beats_per_bar: int) -> None:
        self.starts = tuple(float(start) for start in starts) or (0.0,)
        self.beats_per_bar = int(beats_per_bar)

    def start(self, bar: int) -> float:
        """The beat where a bar (from 1) starts."""
        index = bar - 1
        if index < 0:
            return self.starts[0] + index * self.beats_per_bar
        if index >= len(self.starts):
            return self.starts[-1] + (index - len(self.starts) + 1) * self.beats_per_bar
        return self.starts[index]

    def length(self, bar: int) -> float:
        """The number of beats in a bar."""
        return self.start(bar + 1) - self.start(bar)

    def bar_at(self, beat: float) -> int:
        """The bar (from 1) that holds a beat."""
        if beat < self.starts[0]:
            return 1 - math.ceil((self.starts[0] - beat) / self.beats_per_bar - 1e-9)
        if beat >= self.starts[-1]:
            return len(self.starts) + math.floor((beat - self.starts[-1]) / self.beats_per_bar + 1e-9)
        return bisect.bisect_right(self.starts, beat)

    def bar_number(self, beat: float) -> float:
        """A beat as a bar number with a fraction: 17.5 is half way through bar 17."""
        bar = self.bar_at(beat)
        return bar + (beat - self.start(bar)) / self.length(bar)

    def format(self, beat: float) -> str:
        """A beat as `bar.beat.16th`."""
        bar = self.bar_at(beat)
        sixteenths = round((beat - self.start(bar)) * 4, 2)
        if sixteenths >= round(self.length(bar) * 4, 2):
            bar, sixteenths = bar + 1, 0.0
        in_bar, sixteenth = divmod(sixteenths, 4)
        return f"{bar}.{int(in_bar) + 1}.{round(sixteenth + 1, 2):g}"

    def format_range(self, start: float, end: float) -> str:
        """A range as `start-end`. It includes its start and excludes its end."""
        return f"{self.format(start)}-{self.format(end)}"

    def parse(self, text: str) -> float:
        """`bar.beat.16th` as a beat. "17" is 17.1.1 and "17.2" is 17.2.1."""
        match = _POSITION.fullmatch(text) if isinstance(text, str) else None
        if not match:
            raise TrackError(f'a position is "bar.beat.16th", such as "17", "17.2" or "17.2.3"; got {text!r}')
        bar, beat, sixteenth = int(match[1]), int(match[2] or 1), float(match[3] or 1)
        beats = math.ceil(self.length(bar) - 1e-9)
        if not 1 <= beat <= beats or not 1 <= sixteenth < 5:
            raise TrackError(f"position {text!r}: bar {bar} has beats 1-{beats}, and a 16th is 1-4")
        return self.start(bar) + (beat - 1) + (sixteenth - 1) / 4


def _feature_times(features: Any, name: str) -> tuple[float, ...]:
    """The times in seconds of one feature array, such as "beats"; () when absent."""
    try:
        times = _field(features, name, default=None) if features is not None else None
    except Exception:
        return ()
    if times is None:
        return ()
    try:
        values = getattr(times, "values", times)
    except Exception:
        return ()
    if callable(values):
        return ()
    try:
        return tuple(float(value) for value in values)
    except (TypeError, ValueError):
        return ()


def _format_time_axis(ax: Any, window) -> None:
    """Label the x axis (seconds) with the bar numbers of the window."""
    start, end = window.bars
    first = math.ceil(start)
    last = math.floor(end)
    integers = list(range(first, last + 1))
    stride = max(1, math.ceil(len(integers) / 12))
    shown = integers[::stride]
    if integers and integers[-1] == end and (not shown or shown[-1] != integers[-1]):
        shown.append(integers[-1])
    grid = window._track.bars
    ax.set_xticks([window._track._seconds_at(grid.start(bar)) for bar in shown], [str(bar) for bar in shown])
    ax.set_xlabel("bar")


def _window_label(window) -> str:
    return "-".join(window.at)


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
