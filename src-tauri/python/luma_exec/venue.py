"""`luma.venue` — the room, plus a camera over it.

The binding half of `luma.venue` (id, name, fixtures, groups, positions, uv,
views) is an ordinary manifest record. This module wraps that record in one
object that also carries a capability: `render()`, which asks the host for a
photorealistic frame of the venue at a moment in the track and hands back a
`StageImage`.

    luma.venue.render()                              # front, t=0
    luma.venue.render(view="dj", t=64.0)             # the operator's own view
    shot = luma.venue.render(view="overhead")
    Image.open(shot.path)                            # the PNG on disk

Every render is also a figure: it lands in the cell's `figures` list next to any
matplotlib output, so the model sees the picture without being handed bytes.

Host call
---------

``venue.render`` receives::

    {"view": str, "t": float, "width": int, "height": int}

and returns::

    {"artifactRel": "outputs/stage-<uuid>.png", "width": int, "height": int,
     "view": str, "t": float}

`view` and `t` come back because the host clamps both: an out-of-span `t` is
pulled inside the track rather than refused.
"""

from __future__ import annotations

import math
from pathlib import Path
from typing import Any, Callable, Iterator

from .host_errors import LumaHostCallError

HostCall = Callable[[str, Any], Any]

DEFAULT_VIEW = "front"
DEFAULT_WIDTH = 960
DEFAULT_HEIGHT = 540


class VenueHostUnavailableError(RuntimeError):
    """`render()` was called on a venue with no host capability."""


class StageImage:
    """One rendered frame of the stage, on disk in the workspace."""

    __slots__ = ("view", "t", "width", "height", "artifact_rel", "_workspace")

    def __init__(
        self,
        *,
        view: str,
        t: float,
        width: int,
        height: int,
        artifact_rel: str,
        workspace: Path,
    ) -> None:
        self.view = view
        self.t = t
        self.width = width
        self.height = height
        self.artifact_rel = artifact_rel
        self._workspace = Path(workspace)

    @property
    def path(self) -> Path:
        """Absolute path to the PNG, for `PIL.Image.open` and friends."""
        return self._workspace / self.artifact_rel

    def read_bytes(self) -> bytes:
        """The PNG bytes. Read on demand: a frame is megabytes, not kilobytes."""
        return self.path.read_bytes()

    def __repr__(self) -> str:
        return f"<StageImage {self.view} t={self.t:g}s {self.width}x{self.height}>"


def _finite(name: str, value: Any) -> float:
    """`value` as a float the transport can carry.

    JSON has no NaN or Infinity, so a non-finite argument cannot reach the
    host at all. Rejecting it here names the argument; letting it through
    surfaces as a serialization failure over a request the caller never sees.
    """
    number = float(value)
    if not math.isfinite(number):
        raise LumaHostCallError(
            "invalid_argument", f"{name} must be a finite number, not {value!r}"
        )
    return number


def _pixels(name: str, value: Any) -> int:
    """`value` as a frame dimension. The host owns the maximum; this is the floor."""
    pixels = int(_finite(name, value))
    if pixels < 1:
        raise LumaHostCallError("invalid_size", f"{name} must be at least 1 pixel")
    return pixels


class Venue:
    """The venue binding record, with the host's camera attached."""

    __slots__ = ("_values", "_host_call", "_figures", "_workspace")

    def __init__(
        self,
        values: Any,
        *,
        host_call: HostCall | None = None,
        figures: Any = None,
        workspace: Path,
    ) -> None:
        object.__setattr__(self, "_values", values)
        object.__setattr__(self, "_host_call", host_call)
        object.__setattr__(self, "_figures", figures)
        object.__setattr__(self, "_workspace", Path(workspace))

    # -- the binding record ---------------------------------------------

    def __getattr__(self, name: str) -> Any:
        if name.startswith("__") and name.endswith("__"):
            raise AttributeError(name)
        return getattr(object.__getattribute__(self, "_values"), name)

    def __getitem__(self, key: str) -> Any:
        return object.__getattribute__(self, "_values")[key]

    def keys(self) -> list[str]:
        return list(object.__getattribute__(self, "_values").keys())

    @property
    def views(self) -> tuple[str, ...]:
        """Every name `render(view=...)` accepts, in the order they are offered.

        Comes from the manifest, which gets it from the renderer's own `View`
        enum — there is no second list of view names in Python.
        """
        return tuple(object.__getattribute__(self, "_values")["views"])

    def _luma_catalog_items(self) -> Iterator[tuple[str, Any]]:
        """`luma.catalog()` walks the underlying record, not this facade."""
        values = object.__getattribute__(self, "_values")
        for key in values.keys():
            yield key, values[key]

    def __setattr__(self, name: str, value: Any) -> None:
        raise AttributeError("luma.venue is an immutable binding snapshot")

    # -- the camera ------------------------------------------------------

    def render(
        self,
        *,
        view: str = DEFAULT_VIEW,
        t: float = 0.0,
        width: int = DEFAULT_WIDTH,
        height: int = DEFAULT_HEIGHT,
    ) -> StageImage:
        """Render the stage at `t` seconds and return the resulting figure.

        `view` is one of `luma.venue.views`. `t` is absolute track time, clamped
        to the track's span. The room is always drawn under the editor's work
        light and ground grid, so the hardware stays legible next to whatever
        the score is doing at `t`.

        Raises `LumaHostCallError` if `t` is not finite or a frame side is
        under one pixel.
        """
        host_call = object.__getattribute__(self, "_host_call")
        if host_call is None:
            raise VenueHostUnavailableError(
                "this thread has no venue in scope, so the stage cannot be rendered"
            )
        t = _finite("t", t)
        width = _pixels("width", width)
        height = _pixels("height", height)
        response = host_call(
            "venue.render",
            {
                "view": str(view),
                "t": t,
                "width": width,
                "height": height,
            },
        )
        shot = StageImage(
            view=str(response["view"]),
            t=float(response["t"]),
            width=int(response["width"]),
            height=int(response["height"]),
            artifact_rel=str(response["artifactRel"]),
            workspace=object.__getattribute__(self, "_workspace"),
        )
        figures = object.__getattribute__(self, "_figures")
        if figures is not None:
            figures.register(shot.artifact_rel, shot.width, shot.height)
        return shot

    def __repr__(self) -> str:
        values = object.__getattribute__(self, "_values")
        try:
            name = values["name"]
        except Exception:  # noqa: BLE001 - an unavailable name is not an error here
            name = None
        return f"<Venue {name!r}>" if name else "<Venue>"
