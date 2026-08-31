"""`luma.venue` — the room, plus a camera over it.

The binding half of `luma.venue` (id, name, fixtures, pieces, unplaced, groups,
positions, uv, views) is an ordinary manifest record. This module wraps that
record in one object that also carries a capability: `render()`, which asks the
host for a photorealistic frame of the venue at a moment in the track and hands
back a `StageImage`.

Where a verb and a record field share a name the **verb wins**, because a verb
asks the room as it stands and the record is the snapshot this cell walked into:
`unplaced()` is the live version of `venue["unplaced"]`, and `fixtures()` is a
different question altogether — the *library* an agent picks a head out of,
where `venue["fixtures"]` is the lights already patched in this room.

    luma.venue.render()                              # front, t=0
    luma.venue.render(view="dj", t=64.0)             # the operator's own view
    luma.venue.render(aim_arrows=False)              # drop the aim overlay
    luma.venue.render(highlight="moving_spots")      # light only those heads
    shot = luma.venue.render(view="overhead")
    Image.open(shot.path)                            # the PNG on disk
    print(luma.venue.tiles())                        # the room as a text map

Every render is also a figure: it lands in the cell's `figures` list next to any
matplotlib output, so the model sees the picture without being handed bytes.

Host call
---------

``venue.render`` receives::

    {"view": str, "t": float, "width": int, "height": int,
     "highlight": str | None, "aimArrows": bool}

and returns::

    {"artifactRel": "outputs/stage-<uuid>.png", "width": int, "height": int,
     "view": str, "t": float}

`view` and `t` come back because the host clamps both: an out-of-span `t` is
pulled inside the track rather than refused.

``venue.tiles`` receives ``{"cellM": float}`` and returns ``{"map": str}``.

Building
--------

The same object carries the build verbs. They are the stage page's gestures in
words — there are no coordinates in the vocabulary except `(u, v)` on a surface
and metres for a length or a trim, and there is no `set_position`.

**Start by reading the two vocabularies.** Nothing below can be guessed: a piece
is named by a catalog id and a light by a library path, and both lists are the
catalog's own, not this module's::

    print(luma.venue.catalog())        # every piece, with its sockets
    print(luma.venue.fixtures("spot")) # every head that answers "spot"

Then build::

    deck  = luma.venue.place("stage_lab/stage_praticavel_2x1x1.glb", at=(0, 2))
    left  = luma.venue.extend(deck, "corner_fl", 4.0)
    print(luma.venue.describe())

Reading this surface
--------------------

Everything that *asks the room* is a verb and takes parentheses: `render()`,
`tiles()`, `catalog()`, `fixtures()`, `describe()`, `dangling()`, `unplaced()`,
`reach()`, and the build verbs below. Everything that is already *in your hand*
is a plain attribute and takes none — `venue.views`, `placement.placed`,
`placement.node_id`, `distribution.ok`, `distribution.message`,
`distribution.needed_m`, `distribution.fixtures`, `head.path`, `head.modes`.
A machine-extracted listing of this module will show several of the second kind
as if they were the first; they are properties, and calling one returns the
value's `__call__` error rather than the value.

Angles are **degrees** everywhere on this surface — `yaw`, `roll`, `pan`,
`tilt` — and lengths are metres. There is no other unit.

`describe()` is the check channel and it is the only one. Every verb hands you
the fresh tree, and every line on it carries `at=(x, y, z)` in metres,
`heading=` in degrees, and — on a fixture — `beam=<word>`. A verb returning
`placed` means the graph accepted it, not that it is the shape you asked for;
the tree is where that second question is answered.

Every verb returns a `Placement` — the resolver's own report, with the fresh
`describe()` on it — so a program reads its own effect without a second call.
The only exception a verb raises is `luma.VenueRefused`, carrying the
resolver's message verbatim.

A fixture's rest direction is the outward normal of the socket it hangs from —
hung under a truss it points down, clamped to the downstage face it points at
the house — so the usual way to aim a rig is to hang it on the right face.
`aim` is for the rest: it turns one head off that normal, in degrees, and the
turn is a parameter of the fixture, so the stored pose, the beam and the aim
arrows all move together.

    heads = luma.venue.fixtures("rogue spot")     # what can be distributed
    row   = luma.venue.distribute(bar, "face_-y", heads[0].path, 6,
                                  mode=heads[0].modes[0].name)
    luma.venue.aim(row.fixtures[0], tilt=30)
"""

from __future__ import annotations

import math
from pathlib import Path
from typing import Any, Callable, Iterator, Mapping, Sequence

from .host_errors import LumaHostCallError, VenueRefused

HostCall = Callable[[str, Any], Any]

DEFAULT_VIEW = "front"
DEFAULT_WIDTH = 960
DEFAULT_HEIGHT = 540
DEFAULT_CELL_M = 0.5
#: Aim arrows are on by default here and nowhere else: this is the channel an
#: agent verifies a patch through, and a picture that does not say which way the
#: heads point cannot answer "is this rig aimed the way I asked".
DEFAULT_AIM_ARROWS = True


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



def _node(value: Any) -> str:
    """A node id out of whatever the caller is holding.

    A verb returns a `Placement`, and the next verb wants the node it placed, so
    every argument that names a node accepts the report as well as the id. The
    alternative is `deck.node_id` at every call site, which is the caller doing
    the facade's job.
    """
    for attribute in ("node_id", "id"):
        found = getattr(value, attribute, None)
        if isinstance(found, str):
            return found
    if isinstance(value, Mapping):
        for key in ("nodeId", "node_id", "id"):
            found = value.get(key)
            if isinstance(found, str):
                return found
    if isinstance(value, str):
        return value
    raise LumaHostCallError(
        "invalid_argument", f"{value!r} is not a node or a node id"
    )


def _params(params: Mapping[str, Any]) -> dict[str, float]:
    """Keyword parameters as the graph's own map of floats."""
    return {str(key): _finite(str(key), value) for key, value in params.items()}


class OpenSocket:
    """A structural socket no relation accounts for."""

    __slots__ = ("node_id", "socket", "socket_type")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.node_id = str(row["nodeId"])
        self.socket = str(row["socket"])
        self.socket_type = str(row["socketType"])

    def __repr__(self) -> str:
        return f"<open {self.node_id}.{self.socket} {self.socket_type}>"


class UnplacedBranch:
    """A subtree the solve never reached, by its root."""

    __slots__ = ("node_id", "kind", "label", "descendants")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.node_id = str(row["nodeId"])
        self.kind = str(row["kind"])
        self.label = row.get("label")
        self.descendants = int(row.get("descendants") or 0)

    def __repr__(self) -> str:
        return f"<unplaced {self.node_id} {self.kind} +{self.descendants}>"


class Reach:
    """What an extend ray met, and how far away it is."""

    __slots__ = ("node_id", "socket", "gap_m")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.node_id = str(row["nodeId"])
        self.socket = str(row["socket"])
        self.gap_m = float(row["gapM"])

    def __repr__(self) -> str:
        return f"<reach {self.node_id}.{self.socket} gap={self.gap_m:g} m>"


class DistributedFixture:
    """One light a `distribute` put up: where it hangs and how it is patched.

    A row's fixtures come back in face order — `along_m` ascending — which is
    the order they hang in, not the order their labels are numbered in.
    """

    __slots__ = ("node_id", "label", "universe", "address", "along_m", "group_path")

    def __init__(self, row: Mapping[str, Any]) -> None:
        #: The `fixtures` row id, which is also its venue-graph node id — what
        #: `aim` and `trim` take.
        self.node_id = str(row["id"])
        self.label = str(row["label"])
        self.universe = int(row["universe"])
        self.address = int(row["address"])
        #: Metres along the host face from its middle, ascending across the row.
        self.along_m = float(row["alongM"])
        #: The deepest derived group it landed in, as a display path.
        self.group_path = tuple(str(name) for name in row.get("groupPath") or ())

    def __repr__(self) -> str:
        return (
            f"<{self.label} u{self.universe}/{self.address} "
            f"at {self.along_m:+.2f} m>"
        )


class Placement:
    """What one verb did, in the resolver's own words.

    `outcome` is a fact about the *graph*, not a verdict on the call: `detach`
    reports `unplaced` and did exactly what it was asked. A call that was
    refused raises `VenueRefused` and produces no `Placement` at all.

    `warnings` are the things the solve decided for the caller — a roll a joint
    does not have, a catalog entry that is gone. They never mean the call
    failed.
    """

    __slots__ = ("node_id", "outcome", "parent_id", "warnings", "dangling",
                 "constraints", "_tree")

    def __init__(self, response: Mapping[str, Any]) -> None:
        report = response["placement"]
        self.node_id = str(report["nodeId"])
        self.outcome = str(report["outcome"])
        self.parent_id = report.get("parentId")
        self.warnings = tuple(str(w) for w in report.get("warnings") or ())
        self.dangling = tuple(OpenSocket(d) for d in report.get("dangling") or ())
        self.constraints = tuple(report.get("constraints") or ())
        self._tree = str(response.get("describe") or "")

    @property
    def placed(self) -> bool:
        """Whether the solve reached it — whether it is in the room."""
        return self.outcome == "placed"

    def describe(self) -> str:
        """The whole tree as it stands after this call. No round trip: the host
        solved it once and sent both halves."""
        return self._tree

    def __repr__(self) -> str:
        warned = f" {len(self.warnings)} warnings" if self.warnings else ""
        return f"<Placement {self.node_id} {self.outcome}{warned}>"

    def __str__(self) -> str:
        return f"{self!r}\n{self._tree}"


class Distribution:
    """What a `distribute` placed, or why the row did not fit.

    A refusal is not an error: nothing was written, `needed_m` is the length
    that would make the same call succeed, and `message` is that fix in words.
    The absence of a refusal is the whole of "it worked" — there is no second
    flag beside it to disagree with.
    """

    __slots__ = ("fixtures", "refusal", "warnings", "dangling", "unplaced", "_tree")

    def __init__(self, response: Mapping[str, Any]) -> None:
        report = response["report"]
        self.fixtures = tuple(
            DistributedFixture(row) for row in report.get("fixtures") or ()
        )
        self.refusal = report.get("refusal")
        self.warnings = tuple(str(w) for w in report.get("warnings") or ())
        self.dangling = tuple(OpenSocket(d) for d in report.get("dangling") or ())
        self.unplaced = tuple(UnplacedBranch(u) for u in report.get("unplaced") or ())
        self._tree = str(response.get("describe") or "")

    @property
    def ok(self) -> bool:
        return self.refusal is None

    @property
    def message(self) -> str | None:
        """The fix, in words — `None` when nothing was refused."""
        return None if self.refusal is None else str(self.refusal["suggestion"])

    @property
    def needed_m(self) -> float | None:
        """How long the host face would have to be, for a row that is too long."""
        if self.refusal is None or "neededM" not in self.refusal:
            return None
        return float(self.refusal["neededM"])

    def describe(self) -> str:
        return self._tree

    def __repr__(self) -> str:
        if self.refusal is not None:
            return f"<Distribution refused: {self.message}>"
        return f"<Distribution {len(self.fixtures)} fixtures>"

    def __str__(self) -> str:
        return f"{self!r}\n{self._tree}"


class LibraryMode:
    """One mode of a library fixture — the string `distribute` takes as `mode`."""

    __slots__ = ("name", "channels", "moves", "role")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.name = str(row["name"])
        self.channels = int(row["channels"])
        self.moves = bool(row["moves"])
        self.role = str(row["role"])

    def __repr__(self) -> str:
        aims = ", moves" if self.moves else ""
        return f"<{self.name} {self.channels}ch {self.role}{aims}>"


class LibraryFixture:
    """A fixture in the library, far enough resolved to be named in `distribute`.

    `path` is the first argument; `modes[n].name` is the `mode` keyword. Nothing
    else here is an argument — it is what a chooser needs to tell two heads
    apart.
    """

    __slots__ = ("path", "manufacturer", "model", "kind", "moves", "beam_deg", "modes")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.path = str(row["path"])
        self.manufacturer = str(row["manufacturer"])
        self.model = str(row["model"])
        self.kind = str(row["kind"])
        self.moves = bool(row["moves"])
        beam = row.get("beamDeg")
        #: Lens range in degrees, or `None` where the definition measures none.
        self.beam_deg = None if beam is None else (float(beam[0]), float(beam[1]))
        self.modes = tuple(LibraryMode(m) for m in row.get("modes") or ())

    def mode(self, channels: int) -> str:
        """The name of this fixture's mode with `channels` channels.

        The lookup a caller otherwise writes as a comprehension over `modes`,
        and the one that raises with the choices in the message rather than an
        `IndexError` twenty lines later.
        """
        for mode in self.modes:
            if mode.channels == channels:
                return mode.name
        offered = ", ".join(f"{m.name} ({m.channels})" for m in self.modes)
        raise LumaHostCallError(
            "invalid_argument",
            f"{self.model} has no {channels}-channel mode; it has {offered}",
        )

    def __repr__(self) -> str:
        return f"<{self.manufacturer} {self.model} at {self.path!r}>"

    def __str__(self) -> str:
        beam = "" if self.beam_deg is None else f"  beam {self.beam_deg[0]:g}-{self.beam_deg[1]:g}°"
        head = (
            f"{self.path}\n"
            f"  {self.manufacturer} {self.model}  [{self.kind}]"
            f"{'  moves' if self.moves else ''}{beam}"
        )
        modes = "\n".join(
            f"    {m.name!r}  {m.channels} ch  {m.role}{'  moves' if m.moves else ''}"
            for m in self.modes
        )
        return f"{head}\n{modes}" if modes else head


class Library(tuple):
    """The page of fixtures a search answered with, printable as it stands."""

    __slots__ = ()

    def __str__(self) -> str:
        if not self:
            return "no fixture in the library matches that"
        return "\n".join(str(entry) for entry in self)


class Catalog:
    """The placeable vocabulary: node kinds, the venue's own surfaces, and every
    catalog piece with the sockets a verb will accept on it.

    Read it before naming anything structural. It is the catalog's own answer,
    resolved against the shipped meshes — not a list maintained in Python.

    `str(catalog)` is the page to read; `pieces` is the same entries as plain
    records, `piece(ref)` is one of them, and `sockets(ref)` is the socket names
    a verb will take on it.

    Structure only. The *lights* are a different vocabulary and a different
    question — "which head, in which mode" rather than "which piece, on which
    socket" — and they come from `luma.venue.fixtures()`, which is what
    `distribute`'s `fixture` and `mode` arguments are named out of.
    """

    __slots__ = ("kinds", "root_sockets", "length_step_m", "pieces")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.kinds = tuple(str(k) for k in row.get("kinds") or ())
        self.root_sockets = tuple(str(k) for k in row.get("rootSockets") or ())
        self.length_step_m = float(row.get("lengthStepM") or 0.0)
        self.pieces = tuple(row.get("pieces") or ())

    def piece(self, catalog_ref: str) -> Mapping[str, Any]:
        """One entry, by the id `place`/`attach` take."""
        for piece in self.pieces:
            if piece["catalogRef"] == catalog_ref:
                return piece
        raise KeyError(catalog_ref)

    def sockets(self, catalog_ref: str) -> tuple[str, ...]:
        """Every socket name on one piece, mating sockets and footings alike."""
        return tuple(s["name"] for s in self.piece(catalog_ref)["sockets"])

    def __str__(self) -> str:
        lines = [
            f"kinds: {', '.join(self.kinds)}",
            f"venue surfaces: {', '.join(self.root_sockets)}",
            f"lengths step {self.length_step_m:g} m",
            "",
            "  <piece>  <name>  [sockets]",
            "  socket(m) is only ever held, (f) only ever a host, (n) either;",
            "  socket* is a face — the kind `distribute` spreads a row along.",
            "  span= marks a piece whose length you choose, in metres; every",
            "  other piece is a fixed mesh and comes only in the size listed.",
            "",
        ]
        # Grouped by palette section rather than merely *runs* of one: the
        # catalog's own order interleaves sections, and a reader who saw
        # "Accessories" once and moved on missed half of it.
        sections: dict[str, list[Any]] = {}
        for piece in self.pieces:
            sections.setdefault(str(piece["group"]), []).append(piece)
        for group, pieces in sections.items():
            lines.append(f"{group}")
            for piece in pieces:
                names = " ".join(
                    f"{s['name']}({s['polarity'][0]}){'*' if s['feature'] else ''}"
                    for s in piece["sockets"]
                    if s["joint"] != "grab"
                )
                span = "  span=" if piece["procedural"] else ""
                lines.append(
                    f"  {piece['catalogRef']}{span}  {piece['name']}  [{names}]"
                )
        return "\n".join(lines)

    def __repr__(self) -> str:
        return f"<Catalog {len(self.pieces)} pieces>"


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
        highlight: str | None = None,
        aim_arrows: bool = DEFAULT_AIM_ARROWS,
    ) -> StageImage:
        """Render the stage at `t` seconds and return the resulting figure.

        `view` is one of `luma.venue.views`. `t` is absolute track time, clamped
        to the track's span. The room is always drawn under the editor's work
        light and ground grid, so the hardware stays legible next to whatever
        the score is doing at `t`.

        `highlight` is a group selection expression (`"moving_spots"`,
        `"~back_wash & left"`). It *replaces* the lighting: every head it
        resolves to comes on open and white and every other head goes dark, so
        the picture is the answer to "which fixtures is this?". The score at `t`
        is not drawn; `t` still only picks the moment, and the view still picks
        the camera.

        `aim_arrows` draws an arrow out of every head along the beam it leaves
        at rest, whatever the score is doing. On by default: an aim is the half
        of a patch a photograph of a dark room cannot show. Turn it off for a
        picture of the light itself.

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
                "highlight": None if highlight is None else str(highlight),
                "aimArrows": bool(aim_arrows),
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

    def tiles(self, *, cell_m: float = DEFAULT_CELL_M) -> str:
        """The room as a top-down text map, one character per `cell_m` square.

        The cheapest way to see where everything ended up: a plan of the venue
        as the house sees it, with a header naming the convention and the
        unplaced branches listed below it. Reach for this before `render()` —
        it answers "is the rig the shape I asked for" in a few hundred
        characters, and a diff of two maps localises a moved piece to one row.

        Raises `LumaHostCallError` if `cell_m` is not a finite number of metres.
        """
        host_call = object.__getattribute__(self, "_host_call")
        if host_call is None:
            raise VenueHostUnavailableError(
                "this thread has no venue in scope, so there is no room to map"
            )
        response = host_call("venue.tiles", {"cellM": _finite("cell_m", cell_m)})
        return str(response["map"])

    # -- building --------------------------------------------------------

    def _verb(self, method: str, payload: Any) -> Any:
        """One host call, with the two failure modes the facade owns.

        A thread with no venue in scope has no room to build in, and a refusal
        is `VenueRefused` rather than the generic host error — those are the
        only calls that changed nothing, and a program that retries should be
        able to tell them apart in one `except`.
        """
        host_call = object.__getattribute__(self, "_host_call")
        if host_call is None:
            raise VenueHostUnavailableError(
                "this thread has no venue in scope, so there is nothing to build"
            )
        try:
            return host_call(method, payload)
        except LumaHostCallError as error:
            if error.code == "refused":
                raise VenueRefused(str(error)) from None
            raise

    def catalog(self) -> Catalog:
        """Every node kind, surface and catalog piece a verb will accept.

        Answers on an empty room, which is exactly when it is needed.
        """
        return Catalog(self._verb("venue.catalog", {})["catalog"])

    def fixtures(self, query: str | None = None, *, limit: int = 20) -> Library:
        """The lights `distribute` can name, searched by manufacturer and model.

        `catalog()` is the structure half of the vocabulary; this is the other
        half. Every term in `query` must appear somewhere in "manufacturer
        model", in any order and any case, so `"rogue spot"` and `"spot rogue"`
        find the same head; no query at all is the first page of the library.

            for head in luma.venue.fixtures("mac aura"):
                print(head)

        Each entry carries the two arguments `distribute` wants — `path` and a
        `modes[n].name` — plus what tells two heads apart: whether it `moves`,
        its lens `beam_deg`, and each mode's channel count and role (`wash`,
        `spot`, `beam`, `strobe`, `blinder`, `pixel`, `fx`). Print the result
        to read the page; the library is thousands of files, so narrow the
        query rather than raise `limit`.
        """
        response = self._verb(
            "venue.fixtures",
            {
                "query": None if query is None else str(query),
                "limit": max(1, int(limit)),
            },
        )
        return Library(LibraryFixture(row) for row in response["fixtures"])

    def describe(self) -> str:
        """The rig as an indented tree: what is bolted to what, by which
        sockets, with which parameters, which way each light points, and
        everything the solve left open.

        The verification channel. `tiles()` says where a piece *is*; this says
        what it is *on*, which is the sentence a plan of metres cannot carry.
        Every verb hands back the same text, so this is for reading a room
        somebody else built.

        A fixture's line ends in `beam=<word>` — the direction its beam leaves
        at rest, as one stage word (`down`, `up`, `house`, `upstage`,
        `stage-left`, `stage-right`). Check it after every `distribute`: which
        face of a piece is its underside depends on how that piece is hung, so
        the beam word is the only thing here that answers "is this rig pointing
        where I meant".
        """
        return str(self._verb("venue.describe", {})["text"])

    def dangling(self) -> tuple[OpenSocket, ...]:
        """Every structural socket no relation accounts for.

        Neither half of a joint is dangling, and neither is an end a *resolved*
        far-end check covers — so a bridging run that met its target closes two
        sockets rather than leaving them open.
        """
        return tuple(OpenSocket(row) for row in self._verb("venue.open", {})["dangling"])

    def unplaced(self) -> tuple[UnplacedBranch, ...]:
        """Every branch the solve never reached, by its root.

        A patched fixture nobody has hung is the ordinary case; `detach` is the
        other. Read live, so it answers about the room as this cell has left it
        rather than as it found it.
        """
        return tuple(
            UnplacedBranch(row) for row in self._verb("venue.open", {})["unplaced"]
        )

    def reach(self, node: Any, socket: str) -> Reach | None:
        """What a run out of `socket` would meet, and the buildable gap to it.

        `None` means the ray met nothing, so any length is buildable. This is
        the number `extend` refuses against; asking first is how a program
        avoids the refusal rather than catching it.
        """
        response = self._verb(
            "venue.reach", {"nodeId": _node(node), "socket": str(socket)}
        )
        row = response.get("reach")
        return None if row is None else Reach(row)

    def place(
        self,
        piece: str,
        *,
        at: Sequence[float] | None = None,
        on: Any = None,
        surface: str | None = None,
        socket: str | None = None,
        kind: str = "piece",
        yaw: float = 0.0,
        trim: float = 0.0,
        label: str | None = None,
        **params: Any,
    ) -> Placement:
        """Seat a catalog piece on a surface at `(u, v)`, metres across it.

        `+u` is stage right and `+v` runs **toward the crowd**, so upstage is
        at `-v`: a riser behind the band is at negative `v`, a barricade in
        front of it at positive. The pair is a plan of the room, and it reads
        the same on every surface — the floor, a deck top, and the grid a flown
        piece hangs from all take the same `(u, v)`.

        With no `on`, the surface is the venue's own floor — the room's ground
        plane, which is where a rig starts. `on=<node>, surface="top"` seats it
        on a deck instead; `surface="rig"` is the same plane as the floor facing
        *down*, which is what a flown piece hangs from.

        `trim` is how high it flies, in metres, and it carries the whole subtree
        with it. `yaw` is degrees about the surface normal. `socket` names the
        piece's own footing; leaving it out lets the catalog pick the underside,
        which is what makes "put a deck on the floor" not require knowing that a
        deck's underside is spelled `bottom` and a stick's is spelled `seat`.

        Extra keywords are graph parameters — `span` for a generated stick,
        `count`/`span` for an array. Only a piece the catalog marks `span=` has
        a length you choose; every other entry is a fixed mesh and comes in the
        one size the catalog lists, so an 8 m deck is decks bolted end to end,
        not a parameter.

        `kind` is one of `catalog().kinds` and says what the node *is* to the
        solve, not what it looks like: `piece` (the default, anything in the
        catalog), `stage` (a deck — a surface other things stand on), `run` (a
        horizontal truss), `tower` (a vertical one), `array` (`count` copies
        spread over `span`). Naming it wrong places the geometry correctly and
        files it under the wrong noun, which is what `describe()` and the
        derived groups then read back.

        There is no pitch or rake in this vocabulary. `yaw` turns about the
        surface normal and that is the only angle a placement has, so a wing
        raked 45° out of the vertical is built as a piece bolted to a joint
        that already turns — not as an angle on a flat placement.

        Raises `VenueRefused` if nothing on the piece can rest on a surface,
        and if `piece` is not a catalog entry.
        """
        u, v = (0.0, 0.0) if at is None else (float(at[0]), float(at[1]))
        return Placement(
            self._verb(
                "venue.place",
                {
                    "kind": str(kind),
                    "catalogRef": str(piece),
                    "label": None if label is None else str(label),
                    "surfaceNodeId": None if on is None else _node(on),
                    "surfaceSocket": None if surface is None else str(surface),
                    "mySocket": None if socket is None else str(socket),
                    "u": _finite("at[0]", u),
                    "v": _finite("at[1]", v),
                    "yaw": math.radians(_finite("yaw", yaw)),
                    "trim": _finite("trim", trim),
                    "params": _params(params),
                },
            )
        )

    def attach(
        self,
        piece: str,
        *,
        to: Any,
        socket: str,
        my_socket: str | None = None,
        kind: str = "piece",
        roll: float = 0.0,
        label: str | None = None,
        **params: Any,
    ) -> Placement:
        """Bolt a new catalog piece onto an existing node's socket.

        `socket` is the host's — read them off `catalog()` or off `dangling()`.
        `my_socket` is the new piece's half of the joint; leaving it out lets
        the catalog pick the one that mates, using the same rule the stage
        page's snap search scores candidates with. That rule scores *socket
        types*, not intent: it will happily mate a piece's front edge to the
        host's right edge, which bolts the piece on turned a quarter turn. The
        joint it chose is written in `describe()` as `by <mine> on .<theirs>` —
        read it back before building a chain out of it, or name `my_socket`.

        `roll` is degrees about the shared normal, and a joint that does not
        turn quantizes it and says so in the report's warnings rather than
        refusing.

        Raises `VenueRefused` for a pair the catalog forbids, a parent that
        would close a cycle, and an array asked to host.
        """
        return Placement(
            self._verb(
                "venue.attach",
                {
                    "kind": str(kind),
                    "catalogRef": str(piece),
                    "label": None if label is None else str(label),
                    "parentId": _node(to),
                    "mySocket": None if my_socket is None else str(my_socket),
                    "theirSocket": str(socket),
                    "yaw": math.radians(_finite("roll", roll)),
                    "params": _params(params),
                },
            )
        )

    def extend(self, node: Any, socket: str, length: float | None = None) -> Placement:
        """Run a stick out of an open socket, along the way it faces.

        With no `length`, the run goes as far as the ray found — the measured
        gap when something is in the way, half a metre when nothing is.

        A length **equal** to the gap bridges it: the run hangs off the socket it
        grew from and the socket it reaches is written down as a far-end check,
        so `dangling()` reports a joint instead of two open ends. **Less** is a
        stub. **More** raises `VenueRefused` — that is what stops structure
        growing through structure, and the message carries the gap.
        """
        return Placement(
            self._verb(
                "venue.extend",
                {
                    "nodeId": _node(node),
                    "socket": str(socket),
                    "lengthM": None if length is None else _finite("length", length),
                },
            )
        )

    def duplicate(
        self, node: Any, *, to: Any, socket: str, flip: bool = False
    ) -> Placement:
        """Copy a subtree onto another socket, optionally mirrored.

        The copy arrives as ordinary rows every other verb can edit. `flip`
        turns its handedness over about the joint — which is how symmetry is
        built here: there is no `mirror` node kind and no `mirror` op, only the
        same wing bolted to the other side the other way round.
        """
        return Placement(
            self._verb(
                "venue.duplicate",
                {
                    "nodeId": _node(node),
                    "parentId": _node(to),
                    "theirSocket": str(socket),
                    "flip": bool(flip),
                },
            )
        )

    def detach(self, node: Any) -> Placement:
        """Unplace a node and its subtree. The rows stay — that is the whole
        difference between unplaced and deleted — so `attach`ing it somewhere
        else restores the branch intact. It shows up in `unplaced()` meanwhile.
        """
        return Placement(self._verb("venue.detach", {"nodeId": _node(node)}))

    def remove(self, node: Any) -> str:
        """Delete a node and everything structural under it, and return the tree
        that is left.

        Pulling a truss down loses the rig its shape, not its lights: every
        fixture under it is **trayed**, so it turns up in `unplaced()` and can be
        hung somewhere else. Only a fixture named here directly is deleted, and
        then its patch row goes with it.
        """
        return str(self._verb("venue.remove", {"nodeId": _node(node)})["describe"])

    def trim(self, node: Any, *, label: str | None = None, **params: Any) -> Placement:
        """Edit a node's parameters, and optionally rename it.

        The verb is not the parameter: this edits *any* parameter, and `trim`
        the parameter — fly height — is only one of the things it can set.
        `venue.trim(node, trim=6.0)` is the flying one and reads as a stutter
        because it is one.

        The vocabulary is the graph's: `trim` (how high it flies, metres),
        `u`/`v` (across the surface it sits on), `span` and `count` on a
        generated piece or an array, `yaw` about the joint and `pan`/`tilt` off
        a head's rest direction — every angle in **degrees**, as `aim` takes
        them. Everything bolted to the node comes along.

        Raises `VenueRefused` if `yaw` is given for a node no edge places.
        """
        return self._set_params(node, params, label)

    def aim(
        self,
        fixture: Any,
        *,
        pan: float | None = None,
        tilt: float | None = None,
    ) -> Placement:
        """Point one head, in degrees off the direction it rests in.

        A fixture rests along the outward normal of the socket it hangs from —
        under a truss it points down, on the downstage face it points at the
        house — so the first way to aim a rig is to hang it on the right face.
        This is the second way, for the head that has to look somewhere its
        mount does not: `tilt` swings the beam away from that normal, `pan`
        turns it about the normal, and zero of each is rest.

            luma.venue.aim(head, tilt=35)            # off the truss, into the room
            luma.venue.aim(head, pan=-20, tilt=35)   # and around to stage left

        Both are parameters of the fixture, not of the structure, so the stored
        pose, the beam, a POV camera and `render()`'s aim arrows all move
        together, and duplicating a wing with `flip=True` mirrors the pan with
        the rest of the handedness. Omitting one leaves it where it was, so
        `aim(head, tilt=20)` does not silently un-pan the head.

        `pan` turns about the rest direction, so which way round the room a
        given sign points depends on the face the head hangs from — there
        is no stage-absolute answer to quote here. Set it, then read that
        head's `beam=` word back off `describe()`: that word *is*
        stage-absolute (`down`, `house`, `stage-left`) and is the check
        this verb is meant to be verified through.
        """
        angles: dict[str, Any] = {}
        if pan is not None:
            angles["pan"] = pan
        if tilt is not None:
            angles["tilt"] = tilt
        if not angles:
            raise LumaHostCallError(
                "invalid_argument", "aim needs a pan, a tilt, or both"
            )
        return self._set_params(fixture, angles, None)

    #: Every parameter quoted in degrees at this surface and held in radians in
    #: the graph. `yaw` turns a joint and lives on the edge; `pan` and `tilt`
    #: turn a head and are `luma_scene::venue::ANGLE_PARAMS`, which is what
    #: `describe()` renders them back out of.
    _DEGREES = ("yaw", "pan", "tilt")

    def _set_params(
        self, node: Any, params: Mapping[str, Any], label: str | None
    ) -> Placement:
        """One `venue.params` call, with the degree-quoted keys converted."""
        values = _params(params)
        for key in Venue._DEGREES:
            if key in values:
                values[key] = math.radians(values[key])
        return Placement(
            self._verb(
                "venue.params",
                {
                    "nodeId": _node(node),
                    "params": values,
                    "label": None if label is None else str(label),
                },
            )
        )

    def distribute(
        self,
        host: Any,
        socket: str,
        fixture: str,
        count: int,
        *,
        mode: str,
        layout: str = "even",
        spacing_m: float | None = None,
        span: Sequence[float] | None = None,
        label: str | None = None,
    ) -> Distribution:
        """Place, name, group and patch a row of `count` fixtures along one face.

        The only way a fixture is created with a position — there is no bare
        insert and no self-authored pose, so a light in the room is always a
        light on something.

        `host` of `None` is the venue root, whose two faces are `floor` and
        `rig`; on a truss the faces are `face_-y`, `face_+y`, `face_-z` and
        `face_+z`. Beam is the mount normal, so the face *is* the aim, and
        `face_-y` is a stick's underside wherever that stick ended up: a piece
        hangs *under* a down-facing surface rather than turning over, so one
        flown from the `rig` plane has the same underside it had on the floor.

        Read the beam back anyway. Every fixture line in `describe()` ends in
        `beam=<word>` — `down`, `house`, `stage-left` — and that word is what
        the light is actually doing, which is the one check a face name and a
        tree of parents cannot give you.

        `layout` is `"even"` (the whole face), `"spacing"` with `spacing_m` (a
        fixed centre-to-centre pitch), or `"span"` with `span=(from, to)` as
        fractions of the face.

        The result's `fixtures` are in **face order**, the order they hang
        along the face, each carrying the `along_m` it sits at — so a fan or a
        per-position offset indexes the row rather than guessing it. Their
        *labels* are numbered by the naming rule, which is a different order.

        `label` renames those *fixtures* — it is the prefix the minted names
        are numbered off — and it is **not** a group name.
        `render(highlight=...)` takes a group selection expression, and the
        group each head landed in is on the head itself:
        `row.fixtures[0].group_path[-1]` is the name to highlight. The
        derivation picked it from the fixture's role and where it hangs, not
        from anything named here.

        A row that does not fit is **not** an error: the result has `ok` false
        and `needed_m`, the length that would make the same call succeed. Only
        `spacing` and `span` can fail to fit; `"even"` divides whatever face it
        is given and always fits, so a program that only ever calls the default
        needs no retry path.
        """
        if layout == "even":
            plan: dict[str, Any] = {"kind": "even"}
        elif layout == "spacing":
            if spacing_m is None:
                raise LumaHostCallError(
                    "invalid_argument", "layout='spacing' needs spacing_m"
                )
            plan = {"kind": "spacing", "metres": _finite("spacing_m", spacing_m)}
        elif layout == "span":
            if span is None or len(span) != 2:
                raise LumaHostCallError(
                    "invalid_argument", "layout='span' needs span=(from, to)"
                )
            plan = {
                "kind": "span",
                "from": _finite("span[0]", span[0]),
                "to": _finite("span[1]", span[1]),
            }
        else:
            raise LumaHostCallError(
                "invalid_argument",
                f"layout must be 'even', 'spacing' or 'span', not {layout!r}",
            )
        return Distribution(
            self._verb(
                "venue.distribute",
                {
                    "hostNodeId": None if host is None else _node(host),
                    "hostSocket": None if socket is None else str(socket),
                    "fixturePath": str(fixture),
                    "modeName": str(mode),
                    "count": int(count),
                    "layout": plan,
                    "labelPrefix": None if label is None else str(label),
                },
            )
        )

    def __repr__(self) -> str:
        values = object.__getattribute__(self, "_values")
        try:
            name = values["name"]
        except Exception:  # noqa: BLE001 - an unavailable name is not an error here
            name = None
        return f"<Venue {name!r}>" if name else "<Venue>"
