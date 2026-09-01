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

The same object carries the build verbs. You state **intent in the venue
frame**, and the room answers with what it actually built.

    +u = stage right      +v = toward the crowd      +z = up
    upstage is -v. Angles in degrees, lengths in metres.
    Structure comes in 0.5 m modules. Speakers, players and other
    endpoints are exempt — nothing chains off them.

Five rules, and every one of them is enforced rather than advised:

1. `at=` is the footprint **centre**, in plan, everywhere. A free `place`
   anchors by centre; `add` grows from a tip. Same piece, two verbs.
2. `v.tip(node, end=vector)` grabs a cursor on any existing node's free end.
   `end` is a direction, not a name.
3. A hinge's sign is the right-hand rule: positive is counterclockwise about
   `+axis`. `angle` is in 5 degree steps, ±90; 0 is a straight coupler.
4. `direction=` is an exit direction and `axis=` is a rotation axis. They are
   different parameters and a wrong-kind error says which was expected.
5. 1-D `at=` on a stick host is signed metres from **midspan** — `at=0` is
   the middle, negative is the other way along it.

There are **no socket names on this surface**. Vectors are how you name faces
and ends. There are no shape verbs either: a box, a circle, a spiral are your
own Python loops over these primitives.

A chain::

    t = v.place("truss", at=(-5.5, 5), length=8, direction=(0, 0, 1))  # tower
    t = t.add("corner")
    t = t.add("truss", length=11, direction=(1, 0, 0))   # names the corner's exit
    t = t.add("corner")
    t = t.add("truss", length=8, direction=(0, 0, -1))

    g = v.place("guardrail", at=(0, 8))
    g = g.add("hinge", axis=(0, 0, 1), angle=30)
    g = g.add("guardrail")               # follows the hinge; needs no direction

Every `place` and `add` returns a **cursor**, which is also a node handle
(`.id`, `.at`, `.size`, `.face`) — so it feeds every other verb. The cursor is
the *actual* tip: an off-module length is snapped and announced rather than
refused (`c.announce`), and because the return value is always the truth, drift
never accumulates down a chain.

Three things refuse, each carrying its own fix: a turn the joint cannot make
(listing the legal directions as vectors), a collision (naming the blocking
node), and a piece the catalog does not have.

Hosting and lights::

    v.place("dbr15", on=stage, at=(4, -1))
    v.distribute("ledbeam", on=beam, count=8, face=(0, 1, 0))   # crowd face
    v.distribute("spiider", on=beam, count=6, span=(-4, 4))     # metres from midspan
    v.place("pointe", on=beam, at=2.0, face=(0, 0, -1))
    v.aim(heads, direction=(0, 1, -0.5))       # or v.aim(heads, at=(0, 8, 0))

`on=` reframes `at=` into the host's own frame: 2-D `(u, v)` on a deck top, 1-D
signed metres from midspan on a stick. `face=` is a **vector** mapped to the
piece's nearest mounting face, and beam is the mount normal — so choosing the
face is choosing where the light points at rest. `v.toward(point_or_node)` is
accepted anywhere a direction is, and resolves per host, so two flanking wings
each get their own inward vector out of one stated intent.

Reading the room
----------------

The read side is an equal partner, and every field it hands back is legal input
to a write verb::

    v.nodes(kind="run", label="wing_*")   # -> .id .label .kind .at .size .face .tips
    v.extent(v.nodes(kind="tower"))       # -> span and centre in u/v: "is it centred?"
    print(v.describe())                   # the tree, with facade coordinates
    print(v.tiles())                      # the plan as a text map
    v.catalog()["guardrail"].size         # machine-readable dimensions

Drafts
------

A component is a function over the same verbs, run somewhere that is not the
venue yet::

    def portal(s, width=11, height=8):
        # A corner is a box on the run: two of them push the legs apart by their
        # own length, so the start is offset by half a block to land centred.
        corner = s.catalog()["corner"].size[0]
        t = s.place("truss", at=(-width / 2 - corner / 2, 0), length=height,
                    direction=(0, 0, 1))
        t = t.add("corner")
        beam = t = t.add("truss", length=width, direction=(1, 0, 0))
        t = t.add("corner")
        t.add("truss", length=height, direction=(0, 0, -1))
        s.distribute("ledbeam", on=beam, count=8, face=(0, 1, 0))

    gate = v.draft(portal, width=11)   # runs on a scratch graph; venue untouched
    gate.render()                      # visual preview, isolated
    print(gate.extent)                 # textual preview
    for i in range(7):
        v.stamp(gate, at=(0, 5 + 6 * i))

Reading this surface
--------------------

Everything that *asks the room* is a verb and takes parentheses: `render()`,
`tiles()`, `catalog()`, `fixtures()`, `describe()`, `nodes()`, `extent()`,
`tip()`, `dangling()`, `unplaced()`, and the build verbs. Everything already
*in your hand* is a plain attribute — `venue.views`, `cursor.at`,
`cursor.size`, `placement.placed`, `distribution.ok`, `head.path`. A
machine-extracted listing of this module shows several of the second kind as if
they were the first; they are properties.

Angles are **degrees** everywhere on this surface and lengths are metres. There
is no other unit.

`describe()` is the tree channel: every line carries `at=(u, v, z)` in the same
frame you build in, `heading=` in degrees, and — on a fixture — `beam=<word>`.
A verb reporting `placed` means the graph accepted it, not that it is the shape
you asked for; `nodes()`, `extent()` and the tree are where that is answered.

The older, lower layer
----------------------

`attach(piece, to=node, socket="end_b")`, `extend(node, "corner_fl", 4.0)` and
`reach()` still work and still take socket names. They are the layer the cursor
grammar compiles onto, not a second way to build — reach for them only when you
are reading a rig somebody else built through `dangling()`.
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


def _along(at: Any) -> float:
    """A 1-D `at=` on a stick host: signed metres from midspan.

    A pair is accepted and its first number taken, so a caller that reached for
    the 2-D spelling on a one-dimensional host is not stopped by punctuation.
    """
    if isinstance(at, (list, tuple)):
        return _finite("at", at[0])
    return _finite("at", at)


def _near(wanted: str, names: Mapping[str, str]) -> str:
    """The catalog names closest to a miss, as a phrase to append to a refusal.

    A shared prefix of three characters, which is what a typo usually leaves
    intact. Empty when nothing is close: a suggestion that is not one is worse
    than none.
    """
    stem = str(wanted).lower()[:3]
    close = sorted({short for key, short in names.items() if key.startswith(stem)})
    return "" if not close else f"; did you mean {', '.join(close[:3])}?"


def _params(params: Mapping[str, Any]) -> dict[str, float]:
    """Keyword parameters as the graph's own map of floats."""
    return {str(key): _finite(str(key), value) for key, value in params.items()}


class Toward:
    """A direction stated as a *place* rather than as a vector.

    Accepted anywhere a direction is, and resolved **per host**: one
    `toward(centre)` on two flanking wings is two different inward vectors,
    because each is measured from where that wing actually is. What reaches the
    graph is always the resolved vector — the intent is not stored.
    """

    __slots__ = ("target",)

    def __init__(self, target: Any) -> None:
        #: A point `(u, v)` or `(u, v, z)`, or anything with an `.at` — a node
        #: handle, a cursor, a query result.
        self.target = target

    def __repr__(self) -> str:
        return f"<toward {self.target!r}>"


def _point3(name: str, value: Any) -> list[float]:
    """A `(u, v)` or `(u, v, z)` out of whatever the caller is holding.

    A bare pair is a point in plan, at ground level. A node, a cursor or a query
    result is its footprint centre, which is exactly the number the read side
    hands back — so `toward(some_truss)` needs no unpacking at the call site.
    """
    at = getattr(value, "at", None)
    if at is not None and not isinstance(value, (list, tuple)):
        z = float(getattr(value, "z", 0.0) or 0.0)
        return [_finite(name, at[0]), _finite(name, at[1]), z]
    if isinstance(value, Mapping):
        at = value.get("at")
        if at is not None:
            return [
                _finite(name, at[0]),
                _finite(name, at[1]),
                float(value.get("z") or 0.0),
            ]
    if isinstance(value, (list, tuple)) and len(value) in (2, 3):
        return [
            _finite(name, value[0]),
            _finite(name, value[1]),
            _finite(name, value[2]) if len(value) == 3 else 0.0,
        ]
    raise LumaHostCallError(
        "invalid_argument",
        f"{name} must be (u, v), (u, v, z) or something with an .at, not {value!r}",
    )


def _vector(name: str, value: Any, *, origin: Sequence[float] | None = None) -> list[float]:
    """A facade direction `(u, v, z)`.

    A `Toward` is resolved here, against the `origin` the calling verb knows —
    the host's own centre, or the tip the chain is growing from. A caller that
    passes one where no origin is available is told so rather than given a
    vector measured from nowhere.
    """
    if isinstance(value, Toward):
        if origin is None:
            raise LumaHostCallError(
                "invalid_argument",
                f"{name}=toward(...) needs something to measure from — state it on a "
                "verb that names a host, or pass a plain (u, v, z)",
            )
        target = _point3(f"{name} target", value.target)
        delta = [target[i] - float(origin[i]) for i in range(3)]
        length = math.sqrt(sum(c * c for c in delta))
        if length < 1e-9:
            raise LumaHostCallError(
                "invalid_argument", f"{name}=toward(...) points at where it already is"
            )
        return [c / length for c in delta]
    if isinstance(value, (list, tuple)) and len(value) == 3:
        return [_finite(f"{name}[{i}]", value[i]) for i in range(3)]
    raise LumaHostCallError(
        "invalid_argument",
        f"{name} must be a direction (u, v, z) or v.toward(...), not {value!r}",
    )


class Tip:
    """One free end of a piece: where a chain can grow, and which way.

    Handed back by every `place`/`add` and by `v.tip(node, end=...)`, and passed
    straight back in — you never construct one. `direction` and `at` are facade
    vectors you can read, compare and state elsewhere.
    """

    __slots__ = ("node_id", "direction", "at", "_row")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self._row = dict(row)
        self.node_id = str(row["node"])
        #: Unit facade vector the end faces.
        self.direction = tuple(float(c) for c in row["direction"])
        #: Where the end is, facade metres.
        self.at = tuple(float(c) for c in row["at"])

    def wire(self) -> dict[str, Any]:
        """The row the host handed over, round-tripped verbatim."""
        return dict(self._row)

    def __repr__(self) -> str:
        d = self.direction
        return f"<tip of {self.node_id} facing ({d[0]:.2f}, {d[1]:.2f}, {d[2]:.2f})>"


class NodeInfo:
    """One node as the read side reports it, in the frame you build in.

    Every field here is legal input to a write verb, which is what makes
    read → edit → verify a round trip: `.at` goes back into `place(at=)`,
    `.face` into `face=`, a tip's `.direction` into `end=` or `direction=`.
    """

    __slots__ = (
        "id", "kind", "piece", "catalog_ref", "label", "host", "at", "z", "size",
        "face", "tips",
    )

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.id = str(row["id"])
        self.kind = str(row["kind"])
        #: The catalog's short name — `truss`, `deck`, `guardrail`.
        self.piece = row.get("short")
        self.catalog_ref = row.get("catalogRef")
        self.label = row.get("label")
        #: The node it is bolted to, or `None` for a piece on the room itself.
        self.host = row.get("host")
        #: Footprint centre in plan, facade metres.
        self.at = tuple(float(c) for c in row["at"])
        #: Centre height, facade metres.
        self.z = float(row["z"])
        #: How far it reaches on `(u, v, z)`, facade metres. `(0, 0, 0)` for a
        #: fixture: a light's size is a patch row rather than a mesh, so it
        #: counts as the point it hangs at — in `extent()` too.
        self.size = tuple(float(c) for c in row["size"])
        #: The outward normal of the face it sits on. On a light, the beam it
        #: leaves at rest.
        self.face = None if row.get("face") is None else tuple(float(c) for c in row["face"])
        self.tips = tuple(Tip(t) for t in row.get("tips") or ())

    @property
    def node_id(self) -> str:
        """The same id, under the name every verb's `node` argument takes."""
        return self.id

    def __repr__(self) -> str:
        name = self.label or self.piece or self.kind
        return (
            f"<{name} {self.id} at ({self.at[0]:.2f}, {self.at[1]:.2f}, {self.z:.2f}) "
            f"{self.size[0]:.2f}x{self.size[1]:.2f}x{self.size[2]:.2f}>"
        )


class Extent:
    """The span and centre of a set of nodes — the one-line "is it centred".

    `centre` against zero says whether the rig sits on the room's midline;
    `size` against the room says whether it fits.
    """

    __slots__ = ("count", "min", "max", "centre", "size")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.count = int(row["count"])
        self.min = tuple(float(c) for c in row["min"])
        self.max = tuple(float(c) for c in row["max"])
        self.centre = tuple(float(c) for c in row["centre"])
        self.size = tuple(float(c) for c in row["size"])

    def __repr__(self) -> str:
        return (
            f"<extent of {self.count}: u {self.min[0]:.2f}..{self.max[0]:.2f}  "
            f"v {self.min[1]:.2f}..{self.max[1]:.2f}  z {self.min[2]:.2f}..{self.max[2]:.2f}  "
            f"centre ({self.centre[0]:.2f}, {self.centre[1]:.2f})>"
        )

    def __str__(self) -> str:
        return repr(self)


class Cursor:
    """A piece that was just built, and the end the next one grows from.

    Two things in one, on purpose: it is a **node handle** (`.id`, `.at`,
    `.size`, `.face`) that every other verb accepts, and it is a **cursor**
    whose `.add()` continues the chain. That is why a chain reads as
    `t = t.add(...)` — each step hands you the truth about what now exists.

    `.at`, `.z` and `.size` are what was **built**, not what was asked for. If a
    length was snapped to the module or a mark could not be taken exactly,
    `.announce` says so in words; it is empty when nothing was.
    """

    __slots__ = ("id", "at", "z", "size", "announce", "_tip", "_owner", "_draft", "_placement")

    def __init__(
        self,
        response: Mapping[str, Any],
        owner: "Venue",
        draft: str | None = None,
    ) -> None:
        self.id = str(response["node"])
        self.at = tuple(float(c) for c in response["at"])
        self.z = float(response["z"])
        self.size = tuple(float(c) for c in response["size"])
        self.announce = tuple(str(line) for line in response.get("announce") or ())
        self._tip = None if response.get("tip") is None else Tip(response["tip"])
        self._owner = owner
        # Which graph this piece is in. A cursor carries it so `.add()` keeps
        # building where the chain started — a draft cursor that continued into
        # the venue would be the one way a preview could touch the room.
        self._draft = draft
        self._placement = Placement(response)

    @property
    def node_id(self) -> str:
        return self.id

    @property
    def tip(self) -> Tip | None:
        """The free end a chain continues from, or `None` where the piece left
        several open and no one of them is *the* next one (a four-way block)."""
        return self._tip

    @property
    def placement(self) -> Placement:
        """The resolver's own report for this node — warnings, open ends."""
        return self._placement

    def add(self, piece: str, **kwargs: Any) -> "Cursor":
        """Bolt the next piece onto this one's free end.

        Takes everything `place` does except `at=` and `on=`: the joint decides
        where it goes. `direction=` names the way out — required after a corner,
        which is how a corner's exit face gets chosen, and refused when it
        contradicts a joint that has already decided (a straight run leaves the
        way it points).

        `add("corner")` and `add("hinge", axis=, angle=)` are joints in their
        own right, so a turn is a chain element rather than a hidden parameter.
        Where the **joint itself** articulates — a guardrail chains post to post
        — `add(piece, angle=N)` turns at the shared post instead, in that
        joint's own steps, with no block between the two pieces.

        Takes `label=`, `length=`, `trim=` and `to=` exactly as `place` does.

        Raises `luma.VenueRefused` for a turn no joint here makes (the message
        lists the ones it does, as vectors) and for a collision (naming the
        node in the way).
        """
        if self._tip is None:
            raise LumaHostCallError(
                "invalid_argument",
                f"`{self.id}` left no single free end to continue from; "
                "grab one with v.tip(node, end=(u, v, z))",
            )
        return self._owner._chain(piece, tip=self._tip, draft=self._draft, **kwargs)

    def describe(self) -> str:
        return self._placement.describe()

    def __repr__(self) -> str:
        said = f" — {'; '.join(self.announce)}" if self.announce else ""
        return (
            f"<{self.id} at ({self.at[0]:.2f}, {self.at[1]:.2f}, {self.z:.2f}) "
            f"{self.size[0]:.2f}x{self.size[1]:.2f}x{self.size[2]:.2f}{said}>"
        )

    def __str__(self) -> str:
        return repr(self)


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
        report = response.get("placement") or {"nodeId": response.get("node", "")}
        self.node_id = str(report["nodeId"])
        self.outcome = str(report.get("outcome") or "placed")
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
        # The summary, not the tree. A verb's report is printed constantly and
        # the whole rig is tens of kilobytes of it; `describe()` is one call
        # away for the reader who wants the tree, and nobody wants it twice a
        # line.
        lines = [repr(self)]
        lines += [f"  warning: {w}" for w in self.warnings[:3]]
        if len(self.warnings) > 3:
            lines.append(f"  ... {len(self.warnings) - 3} more warnings")
        if self.dangling:
            lines.append(f"  {len(self.dangling)} open sockets — describe() for the tree")
        return "\n".join(lines)


class Distribution:
    """What a `distribute` placed, or why the row did not fit.

    A refusal is not an error: nothing was written, `needed_m` is the length
    that would make the same call succeed, and `message` is that fix in words.
    The absence of a refusal is the whole of "it worked" — there is no second
    flag beside it to disagree with.
    """

    __slots__ = (
        "fixtures", "refusal", "warnings", "dangling", "unplaced", "draft_row", "_tree",
    )

    def __init__(self, response: Mapping[str, Any]) -> None:
        report = response["report"]
        #: Which recorded row this is, inside a draft — what `draft.aim` takes.
        #: `None` for a row in a venue, whose fixtures are real and are aimed
        #: through `luma.venue.aim` by their own ids.
        self.draft_row = report.get("draftRow")
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
        lines = [repr(self)]
        if self.fixtures:
            first, last = self.fixtures[0], self.fixtures[-1]
            lines.append(
                f"  {first.label} .. {last.label}  "
                f"{first.along_m:+.2f} m .. {last.along_m:+.2f} m along the face"
            )
        lines += [f"  warning: {w}" for w in self.warnings[:3]]
        return "\n".join(lines)


class Environment:
    """How a room is lit when the score is not lighting it: its house, or the sky.

    One dial, because a room has one: `house` is the level of the house rig on
    an indoor room and `sun` is how far the sun stands above the horizon on an
    open-air one. The dial the room does not have is `None` rather than zero:
    an indoor room does not have a sun on the horizon, it has no sun.
    """

    __slots__ = ("mode", "house", "sun", "_line")

    def __init__(self, response: Mapping[str, Any]) -> None:
        #: `"indoor"` or `"outdoor"`.
        self.mode = str(response["mode"])
        #: House level, 0 to 1, or `None` outdoors.
        house = response.get("house")
        self.house = None if house is None else float(house)
        #: Sun elevation in degrees above the horizon, or `None` indoors.
        sun = response.get("sun")
        self.sun = None if sun is None else float(sun)
        self._line = str(response.get("describe") or self.mode)

    def __repr__(self) -> str:
        return f"<Environment {self._line}>"

    def __str__(self) -> str:
        return self._line


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


class CatalogPiece:
    """One placeable piece: what to call it, and how big it is.

    `name` is what you pass to `place` — the short one. `size` is `(u, v, z)`
    metres of the box the piece fills **as `place` lays it with no
    `direction=`**, so it is the number to lay a rig out with rather than an
    asset's own dimensions; for a piece the catalog marks `sized`, `length=`
    moves the axis it runs along and this is only its default.
    """

    __slots__ = ("name", "catalog_ref", "display_name", "group", "piece_kind", "sized", "size", "_row")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self._row = row
        #: The short name `place` takes.
        self.name = str(row.get("short") or row["catalogRef"])
        #: The stored id, and an alias `place` also takes.
        self.catalog_ref = str(row["catalogRef"])
        self.display_name = str(row["name"])
        self.group = str(row["group"])
        #: Snap taxonomy: `floor`, `truss`, `speaker`, ...
        self.piece_kind = str(row["pieceKind"])
        #: Whether its length is yours to choose (`length=` on `place`/`add`).
        self.sized = bool(row.get("procedural"))
        self.size = tuple(float(c) for c in row.get("size") or (0.0, 0.0, 0.0))

    def sockets(self) -> tuple[str, ...]:
        """Socket names, for the older `attach`/`extend` layer. The cursor
        grammar never needs these."""
        return tuple(str(s["name"]) for s in self._row.get("sockets") or ())

    def __repr__(self) -> str:
        length = "  length=" if self.sized else ""
        return (
            f"<{self.name}{length}  {self.display_name}  "
            f"{self.size[0]:.2f}x{self.size[1]:.2f}x{self.size[2]:.2f} m>"
        )


class Catalog:
    """The structural vocabulary: every piece you can `place`, and how big it is.

    Read it before naming anything structural. It is the catalog's own answer,
    resolved against the shipped meshes — not a list maintained in Python.

        print(luma.venue.catalog())
        luma.venue.catalog()["guardrail"].size      # (along, across, up), metres

    Structure only. The *lights* are the other vocabulary and the other question
    — "which head, in which mode" rather than "which piece, how long" — and they
    come from `luma.venue.fixtures()`.
    """

    __slots__ = ("kinds", "root_sockets", "module_m", "pieces", "_by_name")

    def __init__(self, row: Mapping[str, Any]) -> None:
        self.kinds = tuple(str(k) for k in row.get("kinds") or ())
        self.root_sockets = tuple(str(k) for k in row.get("rootSockets") or ())
        #: The structural module every length snaps to, metres.
        self.module_m = float(row.get("lengthStepM") or 0.0)
        self.pieces = tuple(row.get("pieces") or ())
        self._by_name: dict[str, CatalogPiece] = {}
        for entry in self.pieces:
            piece = CatalogPiece(entry)
            for key in (piece.name, piece.catalog_ref, piece.display_name):
                self._by_name[key.lower()] = piece

    def __getitem__(self, name: str) -> CatalogPiece:
        """One piece, by short name, stored id or display name."""
        try:
            return self._by_name[str(name).lower()]
        except KeyError:
            raise KeyError(
                f"{name!r} is not a catalog piece; try one of "
                + ", ".join(sorted({p.name for p in self._by_name.values()}))
            ) from None

    def __contains__(self, name: object) -> bool:
        return str(name).lower() in self._by_name

    def __iter__(self) -> Iterator[CatalogPiece]:
        seen: set[str] = set()
        for piece in self._by_name.values():
            if piece.name not in seen:
                seen.add(piece.name)
                yield piece

    def piece(self, name: str) -> Mapping[str, Any]:
        """The raw catalog row, for a caller working at the socket layer."""
        return self[name]._row

    def sockets(self, name: str) -> tuple[str, ...]:
        """Socket names on one piece — the older `attach`/`extend` layer."""
        return self[name].sockets()

    @property
    def length_step_m(self) -> float:
        """The module, under the name the older surface called it."""
        return self.module_m

    def __str__(self) -> str:
        lines = [
            f"structure comes in {self.module_m:g} m modules; "
            "speakers, players and other endpoints are exempt",
            f"node kinds: {', '.join(self.kinds)}",
            "",
            "  <name>  <what it is>  <u x v x up, metres>",
            "  the box the piece fills where `place` puts it with no direction=,",
            "  in the frame you build in — so these are the numbers to lay a rig",
            "  out with. length= marks a piece whose length is yours to choose;",
            "  every other is a fixed mesh and comes in the one size listed.",
            "  A joint is a box too: a corner spends its own length along the run",
            "  it turns, which is why a leg-corner-beam-corner-leg run is wider",
            "  than the beam.",
            "",
        ]
        sections: dict[str, list[CatalogPiece]] = {}
        for piece in self:
            sections.setdefault(piece.group, []).append(piece)
        for group, pieces in sections.items():
            lines.append(group)
            for piece in pieces:
                length = "  length=" if piece.sized else ""
                lines.append(
                    f"  {piece.name}{length}  {piece.display_name}  "
                    f"{piece.size[0]:.2f} x {piece.size[1]:.2f} x {piece.size[2]:.2f}"
                )
        return "\n".join(lines)

    def __repr__(self) -> str:
        return f"<Catalog {len(self.pieces)} pieces>"


class Venue:
    """The venue binding record, with the host's camera attached."""

    __slots__ = ("_values", "_host_call", "_figures", "_workspace", "_cache")

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
        #: Per-cell reads that never change under us: the catalog's vocabulary,
        #: the library page a bare fixture name resolved to.
        object.__setattr__(self, "_cache", {})

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
        house: float | None = None,
        sun: float | None = None,
    ) -> StageImage:
        """Render the stage at `t` seconds and return the resulting figure.

        `view` is one of `luma.venue.views`. `t` is absolute track time, clamped
        to the track's span. The room is drawn under its own environment — see
        `environment()` — with a ground grid over the floor, so the hardware
        stays legible next to whatever the score is doing at `t`.

        `house` and `sun` light *this frame only*. They are a camera setting,
        not an edit: pass `house=0.2` to see the rig in a dim room, or `sun=10`
        to see it in daylight, and the venue is exactly as it was afterwards.
        Pass one or the other — a frame has one sky. `environment()` is the way
        to change the room itself.

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
                "house": None if house is None else float(house),
                "sun": None if sun is None else float(sun),
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

    # -- the room's light -------------------------------------------------

    def environment(
        self,
        *,
        mode: str | None = None,
        house: float | None = None,
        sun: float | None = None,
    ) -> Environment:
        """Read the room's lighting environment, or move it.

        With no arguments this reads. With any argument it writes, and the
        write is venue truth: it lands on the venue itself, so every picture
        taken afterwards — here, in another cell, in the editor — is taken
        under it. For a one-off exposure that changes nothing, pass `house=` or
        `sun=` to `render()` instead.

        `house` is the level of the room's house rig, 0 (dark) to 1 (full), and
        setting it makes the room indoor. `sun` is how far the sun stands above
        the horizon in degrees, -90 to 90, and setting it makes the room open
        air. A room is lit by one or the other, so passing both is an error.
        Values outside those ranges are clamped, not refused.

        `mode` on its own — `"indoor"` or `"outdoor"` — switches without moving
        a dial: the room keeps the level it last had in that mode.
        """
        # `_verb`, like every other call that writes the venue: a thread with no
        # venue in scope has no room to light, and that is the one failure this
        # facade owns.
        response = self._verb(
            "venue.environment",
            {
                "mode": None if mode is None else str(mode),
                "house": None if house is None else float(house),
                "sun": None if sun is None else float(sun),
            },
        )
        return Environment(response)

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

    # -- the build surface -----------------------------------------------

    def toward(self, target: Any) -> Toward:
        """A direction stated as a place: "point at that".

        Accepted anywhere a direction vector is — `face=`, `direction=` — and
        resolved against whatever the verb is measuring from, so one
        `toward(dj_booth)` on two flanking wings is two inward vectors. Only the
        resolved vector reaches the graph.

            v.distribute("spiider", on=left, count=4, face=v.toward((0, 6)))
        """
        return Toward(target)

    def _pieces(self) -> dict[str, str]:
        """Every catalog name that can be `place`d, mapped to its short name.

        Read once per cell and cached: `place` has to know whether it was handed
        a structural piece or a light, and the catalog is the only thing that
        can say. Two vocabularies, one verb — because "put that there" is one
        thought.
        """
        cache = object.__getattribute__(self, "_cache")
        if "pieces" not in cache:
            catalog = self.catalog()
            names: dict[str, str] = {}
            for piece in catalog.pieces:
                short = str(piece.get("short") or piece["catalogRef"])
                for key in (short, str(piece["catalogRef"]), str(piece["name"])):
                    names[key.lower()] = short
            cache["pieces"] = names
        return cache["pieces"]

    def _chain(
        self,
        piece: str,
        *,
        tip: Tip | None = None,
        at: Any = None,
        on: Any = None,
        face: Any = None,
        direction: Any = None,
        axis: Any = None,
        angle: float | None = None,
        length: float | None = None,
        to: Any = None,
        trim: float = 0.0,
        label: str | None = None,
        draft: str | None = None,
    ) -> Cursor:
        """One `place` or `add`. The single lowering both verbs go through."""
        origin = list(tip.at) if tip is not None else None
        payload: dict[str, Any] = {
            "draftId": draft,
            "piece": str(piece),
            "from": None if tip is None else tip.wire(),
            "at": None if at is None else [_finite("at[0]", at[0]), _finite("at[1]", at[1])],
            "on": None if on is None else _node(on),
            "face": None if face is None else _vector("face", face, origin=origin),
            "direction": (
                None if direction is None else _vector("direction", direction, origin=origin)
            ),
            "axis": None if axis is None else _vector("axis", axis),
            "angle": None if angle is None else _finite("angle", angle),
            "length": None if length is None else _finite("length", length),
            "to": None if to is None else _node(to),
            "trim": _finite("trim", trim),
            "label": None if label is None else str(label),
        }
        return Cursor(self._verb("venue.chain", payload), self, draft)

    def place(
        self,
        piece: str,
        *,
        at: Any = None,
        on: Any = None,
        face: Any = None,
        direction: Any = None,
        axis: Any = None,
        angle: float | None = None,
        length: float | None = None,
        to: Any = None,
        trim: float = 0.0,
        label: str | None = None,
        mode: str | None = None,
        draft: str | None = None,
    ) -> Any:
        """Put one thing in the room, anchored by its footprint **centre**.

        `piece` is a catalog short name (`"truss"`, `"deck"`, `"guardrail"` —
        read `catalog()`) or a light (a library path, or a search term matched
        against `fixtures()`). Structure comes back as a `Cursor` you can chain
        off; a light comes back as a `Distribution` of one.

            v.place("truss", at=(-5.5, 5), length=8, direction=(0, 0, 1))
            v.place("deck", at=(0, -2))
            v.place("dbr15", on=stage, at=(4, -1))
            v.place("pointe", on=beam, at=2.0, face=(0, 0, -1))

        `at=` is the footprint centre in plan: `(u, v)` metres, `+u` stage
        right, `+v` toward the crowd. **With `on=`** it is reframed into the
        host and measured from the *host's own footprint centre*: 2-D `(u, v)`
        across a deck top, and a single signed number on a stick, metres from
        midspan, `0` being the middle. On a host whose joint is a point rather
        than a plane — a speaker stand's pole — leave `at=` out and the piece
        seats on the joint.

        `direction=` is the way the piece runs — `(0, 0, 1)` stands a truss on
        end as a tower, `(1, 0, 0)` lays it along stage right. On a speaker or
        a player, which has a front rather than a run, it is the way the box is
        turned; with none stated an endpoint faces the house. `length=` is
        metres and snaps to the 0.5 m module; the cursor's `.announce` says so
        when it did. `trim=` is how high it flies and carries everything bolted
        to it.

        `face=` is a **vector** mapped to the host's nearest mounting face.
        Beam is the mount normal, so on a light this is also where it points at
        rest — `face=(0, 1, 0)` looks at the crowd, `(0, 0, -1)` looks down.

        Raises `luma.VenueRefused` for a name the catalog does not have (the
        message names the near ones), a turn no joint makes (listing the legal
        directions), and a collision (naming the node in the way).
        """
        if str(piece).lower() in self._pieces():
            return self._chain(
                piece,
                at=at,
                on=on,
                face=face,
                direction=direction,
                axis=axis,
                angle=angle,
                length=length,
                to=to,
                trim=trim,
                label=label,
                draft=draft,
            )
        # Not structure: a light, and a light is always a row of one on a host
        # face — there is no bare insert and no self-authored pose.
        along = 0.0 if at is None else _along(at)
        try:
            return self.distribute(
                piece,
                1,
                on=on,
                face=face,
                mode=mode,
                at=along,
                label=label,
                draft=draft,
            )
        except LumaHostCallError as error:
            if error.code != "invalid_argument" or "no fixture in the library" not in str(error):
                raise
            # A name in neither vocabulary is almost always a typo in the one
            # the caller meant, so the refusal names both rather than the half
            # this branch happened to be standing in.
            raise VenueRefused(
                f"{piece!r} is neither a catalog piece nor a fixture in the library"
                + _near(piece, self._pieces())
                + " — read catalog() for pieces and fixtures() for heads"
            ) from None

    def tip(self, node: Any, *, end: Any = None, draft: str | None = None) -> Cursor:
        """Grab a cursor on an existing node's free end.

        `end` is the **direction** that end faces, not a name: `(0, 0, 1)` is
        the top of a tower, `(1, 0, 0)` the stage-right end of a run. Omit it
        where the piece has only one end open.

            top = v.tip(tower, end=(0, 0, 1))
            top.add("corner")

        Raises `luma.VenueRefused` when nothing is open, and when several are
        and none was named — the message lists them as vectors.
        """
        response = self._verb(
            "venue.tip",
            {
                "draftId": draft,
                "nodeId": _node(node),
                "end": None if end is None else _vector("end", end),
            },
        )
        row = response.get("node") or {}
        return Cursor(
            {
                "node": _node(node),
                "at": row.get("at") or [0.0, 0.0],
                "z": row.get("z") or 0.0,
                "size": row.get("size") or [0.0, 0.0, 0.0],
                "tip": response["tip"],
                "announce": [],
            },
            self,
            draft,
        )

    def nodes(
        self,
        *,
        kind: str | None = None,
        label: str | None = None,
        on: Any = None,
        region: Sequence[float] | None = None,
        ids: Sequence[Any] | None = None,
        draft: str | None = None,
    ) -> tuple[NodeInfo, ...]:
        """Every placed node a filter names, in the frame you build in.

        No filter is the whole room. `kind` is one of `catalog().kinds`,
        `label` is a glob (`"wing_*"`), `on` narrows to what hangs off one node
        at any depth, and `region` is `(u_min, v_min, u_max, v_max)` against the
        footprint centre.

            for tower in v.nodes(kind="tower"):
                print(tower.id, tower.at, tower.size)

        Every field comes back in facade metres and is legal input to a write
        verb, so read → edit → verify round-trips without arithmetic in between.
        """
        response = self._verb(
            "venue.query",
            {
                "draftId": draft,
                "ids": None if ids is None else [_node(i) for i in ids],
                "kind": None if kind is None else str(kind),
                "label": None if label is None else str(label),
                "on": None if on is None else _node(on),
                "region": None if region is None else [_finite("region", r) for r in region],
            },
        )
        return tuple(NodeInfo(row) for row in response["nodes"])

    def extent(
        self,
        selection: Any = None,
        *,
        kind: str | None = None,
        label: str | None = None,
        on: Any = None,
        region: Sequence[float] | None = None,
        draft: str | None = None,
    ) -> Extent | None:
        """The span and centre of everything named — the "is it centred" check.

            print(v.extent(kind="tower"))
            print(v.extent(v.nodes(label="portal_*")))

        `selection` is a node, a list of them, or anything `nodes()` returned;
        the keyword filters are the same as `nodes()`. `None` back means nothing
        matched — including an **empty** selection, which is a question about no
        nodes and not a question about the room.
        """
        ids: list[str] | None = None
        if selection is not None:
            items = selection if isinstance(selection, (list, tuple, set)) else [selection]
            ids = [_node(item) for item in items]
        response = self._verb(
            "venue.extent",
            {
                "draftId": draft,
                "ids": ids,
                "kind": None if kind is None else str(kind),
                "label": None if label is None else str(label),
                "on": None if on is None else _node(on),
                "region": None if region is None else [_finite("region", r) for r in region],
            },
        )
        row = response.get("extent")
        return None if row is None else Extent(row)

    def draft(self, fn: Callable[..., Any] | None = None, /, **params: Any) -> "Draft":
        """Run a component function against a scratch graph. The venue is not
        touched.

            def portal(s, width=11, height=8):
                # Each corner block spends its own length along the run, so the
                # legs stand `width` plus two corners apart. Start half a corner
                # early and the finished portal is centred — `catalog()` prints
                # the block's size and the cursor announces it.
                corner = s.catalog()["corner"].size[0]
                t = s.place("truss", at=(-width / 2 - corner / 2, 0), length=height,
                            direction=(0, 0, 1))
                t = t.add("corner")
                beam = t = t.add("truss", length=width, direction=(1, 0, 0))
                t = t.add("corner")
                t.add("truss", length=height, direction=(0, 0, -1))
                s.distribute("ledbeam", on=beam, count=8, face=(0, 1, 0))
                # And `extent` is the check, not the arithmetic:
                #   print(s.extent())   # centre ~ (0, 0)

            gate = v.draft(portal, width=11)
            gate.render()          # look at it
            print(gate.extent)     # measure it
            v.stamp(gate, at=(0, 5))

        Same verbs inside: anything buildable in a venue is draftable. Lights
        are *recorded* rather than patched — a draft has no patch — and laid
        when it is stamped, so a draft renders as structure.

        `v.draft()` with no function hands back an empty draft to build into by
        hand.
        """
        draft = Draft(self, str(self._verb("venue.draft.create", {})["draftId"]))
        if fn is not None:
            try:
                fn(draft, **params)
            except BaseException:
                draft.discard()
                raise
        return draft

    def stamp(
        self,
        draft: "Draft",
        *,
        at: Sequence[float] = (0.0, 0.0),
        yaw: float = 0.0,
        trim: float = 0.0,
    ) -> tuple[str, ...]:
        """Copy a draft into the venue at `at`, turned by `yaw` degrees.

        The copies are ordinary rows every other verb can edit — the *function*
        stays the source of truth, so a change to the component is a re-run and
        re-stamp rather than an edit to seven copies. Returns the ids of the
        pieces that landed on the room's own floor.

            for i in range(7):
                v.stamp(gate, at=(0, 5 + 6 * i))
        """
        response = self._verb(
            "venue.stamp",
            {
                "draftId": draft.id,
                "at": [_finite("at[0]", at[0]), _finite("at[1]", at[1])],
                "yaw": math.radians(_finite("yaw", yaw)),
                "trim": _finite("trim", trim),
            },
        )
        return tuple(str(node) for node in response["nodes"])

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
        """Delete nodes and everything structural under them, and return the
        tree that is left.

        `node` is one node or a selection of them — a list, or anything
        `nodes()` returned. An id that is already gone (a subtree cascaded away
        with its parent) is **satisfied**, not an error: removing a node that is
        not there is the state the call was asking for.

        Pulling a truss down loses the rig its shape, not its lights: every
        fixture under it is **trayed**, so it turns up in `unplaced()` and can be
        hung somewhere else. Only a fixture named here directly is deleted, and
        then its patch row goes with it.
        """
        items = node if isinstance(node, (list, tuple, set)) else [node]
        return str(
            self._verb("venue.remove", {"nodeIds": [_node(item) for item in items]})[
                "describe"
            ]
        )

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
        selection: Any,
        *,
        direction: Any = None,
        at: Any = None,
        pan: float | None = None,
        tilt: float | None = None,
    ) -> Any:
        """Point heads. Aiming is separate from mounting.

        A head rests along the outward normal of the face it hangs from — under
        a truss it points down, on the downstage face it looks at the house — so
        the first way to aim a rig is to hang it on the right face. This is the
        second way.

            v.aim(heads, direction=(0, 1, -0.5))   # out over the crowd, angled down
            v.aim(heads, at=(0, 8, 0))             # at one point in the room
            v.aim(heads, at=dj_booth)              # at a node's own centre

        `selection` is one fixture, a list of them, or a `Distribution`'s
        `.fixtures`. `direction=` gives them all the same beam; `at=` gives each
        its own, solved from where that head actually is — which is why two
        flanking wings converge from one call.

        `pan=` / `tilt=` are the lower layer, in degrees off the rest direction,
        and they set the graph's own parameters directly. Reach for them when
        you want a relative nudge rather than a stated aim.

        The turn is a parameter of the fixture, so the stored pose, the beam,
        a POV camera and `render()`'s arrows all move together.
        """
        items = selection if isinstance(selection, (list, tuple, set)) else [selection]
        nodes = [_node(item) for item in items]
        if not nodes:
            raise LumaHostCallError(
                "invalid_argument",
                "aim was given an empty selection — there is nothing to point; "
                "check what nodes() or the distribution handed back",
            )
        if direction is not None or at is not None:
            if direction is not None and at is not None:
                raise LumaHostCallError(
                    "invalid_argument", "aim takes a direction= or an at=, not both"
                )
            payload: dict[str, Any] = {"nodes": nodes, "direction": None, "at": None}
            if direction is not None:
                payload["direction"] = _vector("direction", direction)
            else:
                payload["at"] = _point3("at", at)
            response = self._verb("venue.aim", payload)
            return tuple(str(node) for node in response["aimed"])
        angles: dict[str, Any] = {}
        if pan is not None:
            angles["pan"] = pan
        if tilt is not None:
            angles["tilt"] = tilt
        if not angles:
            raise LumaHostCallError(
                "invalid_argument",
                "aim needs a direction=, an at=, or a pan= / tilt= in degrees",
            )
        last: Any = None
        for node in nodes:
            last = self._set_params(node, angles, None)
        return last

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

    def _head(self, fixture: Any, mode: str | None) -> tuple[str, str]:
        """A library path and a mode name out of whatever the caller named.

        A `LibraryFixture` from `fixtures()` is exact. A path is taken as given.
        Anything else is **searched**, and the first match wins — which is what
        makes `distribute("ledbeam", ...)` work at all, and why pinning the head
        matters as soon as the choice does:

            head = v.fixtures("robe spiider")[0]
            v.distribute(head, on=beam, count=6, face=(0, 0, -1))
        """
        path = getattr(fixture, "path", None)
        if isinstance(path, str):
            modes = getattr(fixture, "modes", ())
            return path, str(mode or (modes[0].name if modes else ""))
        name = str(fixture)
        cache = object.__getattribute__(self, "_cache").setdefault("heads", {})
        if name not in cache:
            # A library **path** is a name too — it is the field `fixtures()`
            # hands back and the docstrings tell a caller to pass. Named with a
            # mode it needs no lookup at all; named without one it is looked up
            # by its own file stem, because the mode has to come from somewhere.
            if "/" in name or name.endswith(".qxf"):
                if mode is not None:
                    return name, str(mode)
                stem = name.rsplit("/", 1)[-1].removesuffix(".qxf").replace("-", " ")
                found = [head for head in self.fixtures(stem, limit=20) if head.path == name]
            else:
                found = self.fixtures(name, limit=1)
            if not found:
                raise LumaHostCallError(
                    "invalid_argument",
                    f"no fixture in the library answers to {name!r} — search "
                    "`fixtures()` for one",
                )
            cache[name] = found[0]
        head = cache[name]
        return head.path, str(mode or (head.modes[0].name if head.modes else ""))

    def distribute(
        self,
        fixture: Any,
        count: int = 1,
        *,
        on: Any = None,
        face: Any = None,
        mode: str | None = None,
        at: Any = None,
        span: Sequence[float] | None = None,
        spacing_m: float | None = None,
        label: str | None = None,
        draft: str | None = None,
    ) -> Distribution:
        """Hang, name, group and patch a row of `count` lights along one face.

        The only way a fixture is created with a position: a light in the room
        is always a light on something.

            v.distribute("ledbeam", on=beam, count=8, face=(0, 1, 0))
            v.distribute("spiider", on=beam, count=6, span=(-4, 4))
            v.distribute(head, on=None, count=4, face=(0, 0, -1))   # off the grid

        `on=` is the host — `None` is the room itself, whose two faces are the
        floor (`face=(0, 0, 1)`) and the grid above it (`face=(0, 0, -1)`).
        `face=` is a **vector**, mapped to the host's nearest mounting face, and
        beam is the mount normal — so the face you name is where the row points
        at rest. Read it back: every fixture line in `describe()` ends in
        `beam=<word>`, and that word is what the light is actually doing.

        Where along the face, in **metres from midspan**, positive one way and
        negative the other:

        - nothing  — evenly across the whole face;
        - `span=(a, b)` — evenly across that window, clipped to the face;
        - `spacing_m=` — a fixed centre-to-centre pitch, centred;
        - `at=` — one mark, which is what `place(light, on=..., at=...)` uses.

        A row that does not fit is **not** an error: the result has `ok` false
        and `needed_m`, the length that would make the same call succeed.

        Inside a draft the row is *recorded* rather than patched — a draft has
        no patch — and laid when the draft is stamped.
        """
        chosen = [k for k, v in (("at", at), ("span", span), ("spacing_m", spacing_m)) if v is not None]
        if len(chosen) > 1:
            raise LumaHostCallError(
                "invalid_argument",
                f"a row sits one way along its face; {' and '.join(chosen)} is two",
            )
        if at is not None:
            plan: dict[str, Any] = {"kind": "at", "metres": _along(at)}
        elif span is not None:
            if len(span) != 2:
                raise LumaHostCallError(
                    "invalid_argument", "span=(from_m, to_m), metres from midspan"
                )
            plan = {
                "kind": "span",
                "from": _finite("span[0]", span[0]),
                "to": _finite("span[1]", span[1]),
            }
        elif spacing_m is not None:
            plan = {"kind": "spacing", "metres": _finite("spacing_m", spacing_m)}
        else:
            plan = {"kind": "even"}
        origin = None
        if on is not None and isinstance(face, Toward):
            found = self.nodes(ids=[on], draft=draft)
            if found:
                origin = [found[0].at[0], found[0].at[1], found[0].z]
        path, mode_name = self._head(fixture, mode)
        return Distribution(
            self._verb(
                "venue.distribute",
                {
                    "draftId": draft,
                    "hostNodeId": None if on is None else _node(on),
                    "face": None if face is None else _vector("face", face, origin=origin),
                    "hostSocket": None,
                    "fixturePath": path,
                    "modeName": mode_name,
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


class Draft:
    """A component being built somewhere that is not the venue yet.

    Handed back by `v.draft(fn, **params)`. It carries the same build verbs as
    the venue — `place`, `distribute`, `tip`, `nodes`, `extent` — over a scratch
    graph with no rows and no patch, so previewing costs nothing and stamping it
    seven times is seven copies the venue's own verbs made.

        gate = v.draft(portal, width=11)
        gate.render()              # a picture of the piece alone
        print(gate.extent)         # its span and centre
        print(gate.describe())     # its tree
        v.stamp(gate, at=(0, 5))

    Lights are recorded, not patched: a draft renders as structure, and the rows
    are laid against the copies when it is stamped.
    """

    __slots__ = ("id", "_venue")

    def __init__(self, venue: Venue, draft_id: str) -> None:
        self.id = draft_id
        self._venue = venue

    # -- building, in the venue's own vocabulary --------------------------

    def place(self, piece: str, **kwargs: Any) -> Any:
        """As `luma.venue.place`, against the scratch graph."""
        return self._venue.place(piece, draft=self.id, **kwargs)

    def distribute(self, fixture: Any, count: int = 1, **kwargs: Any) -> Distribution:
        """As `luma.venue.distribute`. The row is recorded and laid at the stamp."""
        return self._venue.distribute(fixture, count, draft=self.id, **kwargs)

    def tip(self, node: Any, **kwargs: Any) -> Cursor:
        """As `luma.venue.tip`, on a piece this draft built."""
        return self._venue.tip(node, draft=self.id, **kwargs)

    def nodes(self, **kwargs: Any) -> tuple[NodeInfo, ...]:
        """As `luma.venue.nodes`, over the draft alone."""
        return self._venue.nodes(draft=self.id, **kwargs)

    def toward(self, target: Any) -> Toward:
        """As `luma.venue.toward` — a direction stated as a place."""
        return self._venue.toward(target)

    def aim(
        self, rows: Any, *, direction: Any = None, at: Any = None
    ) -> int:
        """Point a row of lights this draft recorded.

        `rows` is what `draft.distribute(...)` handed back, or a list of them.
        A draft has no fixtures to turn — its lights are rows on paper until the
        stamp — so the aim is recorded against the row and solved per head when
        the draft is stamped. That is what makes a component with pointed heads
        authorable as a component: one `at=` converges from **every** stamp of
        it, each from where that copy actually stands.

            gate = v.draft()
            beam = gate.place("truss", at=(0, 0), length=8, direction=(1, 0, 0))
            row = gate.distribute("ledbeam", 8, on=beam, face=(0, 1, 0))
            gate.aim(row, at=(0, 6, 0))

        Raises `LumaHostCallError` for a `Distribution` some other draft
        recorded, and for neither a `direction=` nor an `at=`.
        """
        items = rows if isinstance(rows, (list, tuple, set)) else [rows]
        indexes = []
        for item in items:
            index = getattr(item, "draft_row", None)
            if index is None:
                raise LumaHostCallError(
                    "invalid_argument",
                    f"{item!r} is not a row this draft recorded — pass what "
                    "draft.distribute() handed back",
                )
            indexes.append(int(index))
        payload: dict[str, Any] = {
            "draftId": self.id,
            "rows": indexes,
            "direction": None,
            "at": None,
        }
        if direction is not None and at is not None:
            raise LumaHostCallError(
                "invalid_argument", "aim takes a direction= or an at=, not both"
            )
        if direction is not None:
            payload["direction"] = _vector("direction", direction)
        elif at is not None:
            payload["at"] = _point3("at", at)
        else:
            raise LumaHostCallError(
                "invalid_argument", "aim needs a direction= or an at="
            )
        return int(self._venue._verb("venue.draft.aim", payload)["aimed"])

    def remove(self, node: Any) -> str:
        """Drop a piece and everything on it, and return the tree that is left.

        As `luma.venue.remove`, over the scratch graph: a draft is edited the
        way a room is, or it is not a place you can build. Any row of lights
        recorded on what went goes with it.
        """
        items = node if isinstance(node, (list, tuple, set)) else [node]
        return str(
            self._venue._verb(
                "venue.draft.remove",
                {"draftId": self.id, "nodeIds": [_node(item) for item in items]},
            )["text"]
        )

    def trim(self, node: Any, **params: Any) -> str:
        """Edit one of the draft's own nodes, in `luma.venue.trim`'s vocabulary
        — `trim`, `u`, `v`, `span`, `count`, and the angles in degrees."""
        values = _params(params)
        for key in Venue._DEGREES:
            if key in values:
                values[key] = math.radians(values[key])
        return str(
            self._venue._verb(
                "venue.draft.params",
                {"draftId": self.id, "nodeId": _node(node), "params": values},
            )["text"]
        )

    # -- previewing --------------------------------------------------------

    @property
    def extent(self) -> Extent | None:
        """The draft's span and centre — the textual preview.

        An attribute rather than a verb because a draft is small and finished:
        there is nothing to narrow, and `gate.extent` is the whole question.
        """
        return self._venue.extent(draft=self.id)

    def describe(self) -> str:
        """The draft's tree, in the shape `luma.venue.describe()` prints, with
        any recorded rows of lights listed under it."""
        return str(self._venue._verb("venue.draft.describe", {"draftId": self.id})["text"])

    def render(
        self,
        *,
        view: str = DEFAULT_VIEW,
        width: int = DEFAULT_WIDTH,
        height: int = DEFAULT_HEIGHT,
    ) -> StageImage:
        """A picture of the draft alone: no room, no floor, no grid.

        The same renderer the venue camera uses, framed on nothing but what the
        component built — so a preview and the stamped rig cannot look like two
        different pieces. Structure only; the lights arrive at the stamp.
        """
        response = self._venue._verb(
            "venue.draft.render",
            {
                "draftId": self.id,
                "view": str(view),
                "width": _pixels("width", width),
                "height": _pixels("height", height),
            },
        )
        shot = StageImage(
            view=str(response["view"]),
            t=float(response["t"]),
            width=int(response["width"]),
            height=int(response["height"]),
            artifact_rel=str(response["artifactRel"]),
            workspace=object.__getattribute__(self._venue, "_workspace"),
        )
        figures = object.__getattribute__(self._venue, "_figures")
        if figures is not None:
            figures.register(shot.artifact_rel, shot.width, shot.height)
        return shot

    def discard(self) -> None:
        """Drop the scratch graph. Nothing in the venue is touched either way —
        a draft that is never discarded simply goes when the cell does."""
        self._venue._verb("venue.draft.discard", {"draftId": self.id})

    def __repr__(self) -> str:
        return f"<Draft {self.id}>"

    def __str__(self) -> str:
        return f"{self!r}\n{self.extent}"
