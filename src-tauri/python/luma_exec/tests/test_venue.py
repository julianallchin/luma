#!/usr/bin/env python3
"""Focused stdlib tests for `luma.venue` and the one figure list.

Run directly with either the bundled environment or an ordinary Python::

    python3 src-tauri/python/luma_exec/tests/test_venue.py
"""

from __future__ import annotations

import math
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

os.environ.setdefault("MPLBACKEND", "Agg")
os.environ.setdefault("MPLCONFIGDIR", tempfile.mkdtemp(prefix="luma-mpl-"))

PACKAGE_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(PACKAGE_ROOT))

from luma_exec.bindings import LumaRecord, build_namespace  # noqa: E402
from luma_exec.figures import FigureSink  # noqa: E402
from luma_exec.host_errors import LumaHostCallError  # noqa: E402
from luma_exec.host_errors import VenueRefused  # noqa: E402
from luma_exec.venue import (  # noqa: E402
    Catalog,
    Cursor,
    Distribution,
    Draft,
    Extent,
    NodeInfo,
    Placement,
    Venue,
    VenueHostUnavailableError,
)

VIEWS = ["front", "audience", "overhead", "quarter_left", "quarter_right", "dj"]

TILE_MAP = "gauntlet view\nplan as the house sees it\n\n  T\n"


def record(**overrides: Any) -> LumaRecord:
    items: dict[str, Any] = {
        "id": "venue-1",
        "name": "The Room",
        "views": list(VIEWS),
    }
    items.update(overrides)
    return LumaRecord(items, "luma.venue")


class Host:
    """The Rust `venue.render` and `venue.tiles` contracts, minus the GPU."""

    def __init__(self, workspace: Path) -> None:
        self.workspace = workspace
        self.calls: list[tuple[str, Any]] = []

    def __call__(self, method: str, payload: Any) -> Any:
        self.calls.append((method, payload))
        if method == "venue.tiles":
            return {"map": TILE_MAP}
        assert method == "venue.render"
        outputs = self.workspace / "outputs"
        outputs.mkdir(parents=True, exist_ok=True)
        rel = f"outputs/stage-{len(self.calls)}.png"
        (self.workspace / rel).write_bytes(b"\x89PNG\r\n\x1a\n")
        # The host clamps `t` into the track's span; a test that echoed the
        # request back would not prove the facade reads the *response*.
        return {
            "artifactRel": rel,
            "width": payload["width"],
            "height": payload["height"],
            "view": payload["view"],
            "t": min(payload["t"], 100.0),
        }


class VenueRenderTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workspace = Path(tempfile.mkdtemp(prefix="luma-venue-"))
        self.host = Host(self.workspace)
        self.figures = FigureSink()
        self.venue = Venue(
            record(),
            host_call=self.host,
            figures=self.figures,
            workspace=self.workspace,
        )

    def test_binding_values_stay_reachable_through_the_facade(self) -> None:
        self.assertEqual(self.venue.id, "venue-1")
        self.assertEqual(self.venue["name"], "The Room")
        self.assertIn("positions", Venue(
            record(positions=[1, 2, 3]),
            workspace=self.workspace,
        ).keys())
        with self.assertRaises(AttributeError):
            self.venue.name = "renamed"

    def test_views_come_from_the_manifest_as_a_tuple(self) -> None:
        self.assertEqual(self.venue.views, tuple(VIEWS))
        self.assertIsInstance(self.venue.views, tuple)

    def test_render_sends_the_wire_shape_and_returns_a_described_image(self) -> None:
        shot = self.venue.render(view="dj", t=12.5, width=320, height=200)
        self.assertEqual(
            self.host.calls,
            [
                (
                    "venue.render",
                    {
                        "view": "dj",
                        "t": 12.5,
                        "width": 320,
                        "height": 200,
                        "highlight": None,
                        "aimArrows": True,
                    },
                )
            ],
        )
        self.assertEqual(shot.view, "dj")
        self.assertEqual((shot.width, shot.height), (320, 200))
        self.assertEqual(repr(shot), "<StageImage dj t=12.5s 320x200>")
        self.assertTrue(shot.path.is_absolute())
        self.assertEqual(shot.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")

    def test_aim_arrows_are_on_unless_the_caller_says_otherwise(self) -> None:
        """This is the verification channel, so the aims are drawn by default."""
        self.venue.render()
        self.venue.render(aim_arrows=False)
        self.assertEqual(
            [call[1]["aimArrows"] for call in self.host.calls], [True, False]
        )

    def test_the_host_clamp_wins_over_the_requested_time(self) -> None:
        self.assertEqual(self.venue.render(t=1e9).t, 100.0)

    def test_a_render_lands_in_the_cell_figure_list(self) -> None:
        first = self.venue.render(view="front")
        second = self.venue.render(view="overhead", width=64, height=64)
        collected, warnings = self.figures.collect(self.workspace, None)
        self.assertEqual(warnings, [])
        self.assertEqual(
            collected,
            [
                {"artifact_rel": first.artifact_rel, "width": 960, "height": 540},
                {"artifact_rel": second.artifact_rel, "width": 64, "height": 64},
            ],
        )
        # `collect` drains: the next cell starts with no figures.
        self.assertEqual(self.figures.collect(self.workspace, None)[0], [])

    def test_a_non_finite_time_is_refused_before_the_transport_sees_it(self) -> None:
        for bad in (float("nan"), float("inf"), float("-inf")):
            with self.assertRaises(LumaHostCallError) as caught:
                self.venue.render(t=bad)
            self.assertEqual(caught.exception.code, "invalid_argument")
            self.assertIn("t must be a finite number", str(caught.exception))
        self.assertEqual(self.host.calls, [])

    def test_a_frame_side_under_one_pixel_is_refused(self) -> None:
        for kwargs in ({"width": 0}, {"height": 0}, {"width": 0.5}, {"height": -4}):
            with self.assertRaises(LumaHostCallError) as caught:
                self.venue.render(**kwargs)
            self.assertEqual(caught.exception.code, "invalid_size")
            self.assertIn("at least 1 pixel", str(caught.exception))
        for kwargs in ({"width": float("nan")}, {"height": float("inf")}):
            with self.assertRaises(LumaHostCallError):
                self.venue.render(**kwargs)
        self.assertEqual(self.host.calls, [])

    def test_render_without_a_host_is_an_explicit_refusal(self) -> None:
        offline = Venue(record(), workspace=self.workspace)
        with self.assertRaises(VenueHostUnavailableError):
            offline.render()

    def test_the_namespace_installs_the_facade_over_the_venue_record(self) -> None:
        namespace = build_namespace(
            {
                "schema_version": 1,
                "revision": "r-1",
                "agent_kind": "pattern_graph",
                "scope": {},
                "root": {"venue": {"id": "venue-1", "views": list(VIEWS)}},
                "artifacts": {},
            },
            self.workspace,
            host_call=self.host,
            figures=self.figures,
        )
        self.assertIsInstance(namespace.venue, Venue)
        self.assertEqual(namespace.venue.views, tuple(VIEWS))
        # The catalog still walks the underlying record, facade or not.
        self.assertIn("luma.venue.id", namespace.catalog())


class VenueTilesTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workspace = Path(tempfile.mkdtemp(prefix="luma-venue-"))
        self.host = Host(self.workspace)
        self.venue = Venue(record(), host_call=self.host, workspace=self.workspace)

    def test_tiles_sends_the_cell_size_and_returns_the_map_itself(self) -> None:
        self.assertEqual(self.venue.tiles(cell_m=1.0), TILE_MAP)
        self.assertEqual(self.host.calls, [("venue.tiles", {"cellM": 1.0})])

    def test_the_default_cell_is_half_a_metre(self) -> None:
        self.venue.tiles()
        self.assertEqual(self.host.calls[0][1]["cellM"], 0.5)

    def test_a_map_is_not_a_figure(self) -> None:
        """Text is read in the transcript; only a picture goes in `figures`."""
        figures = FigureSink()
        venue = Venue(
            record(), host_call=self.host, figures=figures, workspace=self.workspace
        )
        venue.tiles()
        self.assertEqual(figures.collect(self.workspace, None)[0], [])

    def test_a_non_finite_cell_never_reaches_the_host(self) -> None:
        for bad in (float("nan"), float("inf")):
            with self.assertRaises(LumaHostCallError):
                self.venue.tiles(cell_m=bad)
        self.assertEqual(self.host.calls, [])

    def test_a_venue_with_no_host_says_so(self) -> None:
        with self.assertRaises(VenueHostUnavailableError):
            Venue(record(), workspace=self.workspace).tiles()


# ---------------------------------------------------------------------------
# The build verbs
# ---------------------------------------------------------------------------

CATALOG = {
    "kinds": ["stage", "run", "tower", "piece", "fixture", "array"],
    "rootSockets": ["floor", "rig"],
    "lengthStepM": 0.5,
    "pieces": [
        {
            "short": "truss",
            "catalogRef": "truss/straight",
            "name": "Truss",
            "group": "Trusses",
            "pieceKind": "truss",
            "procedural": True,
            "size": [3.0, 0.34, 0.34],
            "sockets": [
                {
                    "name": "grab",
                    "socketType": "grab",
                    "joint": "grab",
                    "polarity": "male",
                    "feature": False,
                },
                {
                    "name": "end_a",
                    "socketType": "truss_end",
                    "joint": "truss_end",
                    "polarity": "neutral",
                    "feature": False,
                },
                {
                    "name": "face_-y",
                    "socketType": "truss_mount",
                    "joint": "surface",
                    "polarity": "female",
                    "feature": True,
                },
            ],
        },
        {
            "short": "corner",
            "catalogRef": "truss/corner",
            "name": "Corner",
            "group": "Trusses",
            "pieceKind": "truss",
            "procedural": True,
            "size": [0.34, 0.34, 0.34],
            "sockets": [],
        },
    ],
}


def tip_row(node_id: str, direction: Any = (1.0, 0.0, 0.0)) -> dict[str, Any]:
    return {
        "node": node_id,
        "socket": "end_b",
        "direction": list(direction),
        "at": [1.5, 0.0, 0.0],
    }


def node_row(node_id: str, **overrides: Any) -> dict[str, Any]:
    row = {
        "id": node_id,
        "kind": "run",
        "catalogRef": "truss/straight",
        "short": "truss",
        "label": None,
        "host": "root",
        "at": [1.0, 2.0],
        "z": 0.17,
        "size": [3.0, 0.34, 0.34],
        "face": [0.0, 0.0, 1.0],
        "tips": [tip_row(node_id)],
    }
    row.update(overrides)
    return row


def chained(node_id: str, payload: Any) -> dict[str, Any]:
    """The host's answer to one chain op: what landed, and the end left over."""
    return {
        "node": node_id,
        "at": payload.get("at") or [0.0, 0.0],
        "z": 0.17,
        "size": [3.0, 0.34, 0.34],
        "tip": tip_row(node_id),
        "announce": [],
        "placement": placement(node_id)["placement"],
        "describe": "root  venue\n",
    }


def placement(node_id: str = "n-1", tree: str = "root  venue\n") -> dict[str, Any]:
    """The host's answer to every mutating verb: the resolver's own report, and
    the tree it produced."""
    return {
        "placement": {
            "nodeId": node_id,
            "outcome": "placed",
            "parentId": "root",
            "warnings": [],
            "dangling": [],
            "constraints": [],
            "venue": {},
        },
        "describe": tree,
    }


class BuildHost:
    """The Rust `venue.*` verb contracts, minus the database.

    Records what the facade sent, so the tests can assert on the *payload* —
    which is where the facade's own work is: coercing nodes to ids, degrees to
    radians, and keyword parameters to the graph's map of floats.
    """

    def __init__(self) -> None:
        self.calls: list[tuple[str, Any]] = []
        self.refuse: str | None = None
        self.built = 0

    def __call__(self, method: str, payload: Any) -> Any:
        self.calls.append((method, payload))
        if self.refuse is not None:
            raise LumaHostCallError("refused", self.refuse)
        if method == "venue.catalog":
            return {"catalog": CATALOG}
        if method == "venue.fixtures":
            return {
                "fixtures": [
                    {
                        "path": "Robe/Robe-Spiider.qxf",
                        "manufacturer": "Robe",
                        "model": "Spiider",
                        "kind": "Moving Head",
                        "moves": True,
                        "beamDeg": [4.0, 50.0],
                        "modes": [
                            {"name": "Mode 1", "channels": 39,
                             "moves": True, "role": "wash"},
                            {"name": "Basic", "channels": 12,
                             "moves": False, "role": "wash"},
                        ],
                    }
                ]
            }
        if method == "venue.describe":
            return {"text": "root  venue\n"}
        if method == "venue.chain":
            self.built += 1
            return chained(f"n-{self.built}", payload)
        if method == "venue.query":
            return {"nodes": [node_row("n-1"), node_row("n-2", label="wing_left")]}
        if method == "venue.extent":
            return {
                "extent": {
                    "count": 2,
                    "min": [-5.5, -0.2, 0.0],
                    "max": [5.5, 0.2, 8.0],
                    "centre": [0.0, 0.0, 4.0],
                    "size": [11.0, 0.4, 8.0],
                }
            }
        if method == "venue.tip":
            return {"tip": tip_row("n-1"), "node": node_row("n-1")}
        if method == "venue.aim":
            return {"aimed": list(payload["nodes"]), "describe": "root  venue\n"}
        if method == "venue.stamp":
            return {"nodes": ["s-1"], "describe": "root  venue\n"}
        if method == "venue.draft.create":
            return {"draftId": "draft-1"}
        if method == "venue.draft.discard":
            return {}
        if method == "venue.draft.describe":
            return {"text": "draft  venue\n"}
        if method == "venue.draft.render":
            return {
                "artifactRel": "outputs/stage-draft.png",
                "width": 960,
                "height": 540,
                "view": "front",
                "t": 0.0,
            }
        if method == "venue.open":
            return {
                "unplaced": [
                    {
                        "nodeId": "mover-9",
                        "kind": "fixture",
                        "label": "Spare",
                        "descendants": 0,
                    }
                ],
                "dangling": [
                    {"nodeId": "run-1", "socket": "end_b", "socketType": "truss_end"}
                ],
            }
        if method == "venue.reach":
            return {"reach": {"nodeId": "run-2", "socket": "end_a", "gapM": 4.0}}
        if method == "venue.remove":
            return {"describe": "root  venue\n"}
        if method == "venue.distribute":
            return {
                "report": {
                    "hostNodeId": "run-1",
                    "hostSocket": "face_-y",
                    "fixtures": [
                        {
                            "id": "fix-1",
                            "label": "Aura 1",
                            "universe": 1,
                            "address": 1,
                            "alongM": -0.5,
                            "groupPath": ["wash", "left wing"],
                        },
                        {
                            "id": "fix-2",
                            "label": "Aura 2",
                            "universe": 1,
                            "address": 17,
                            "alongM": 0.5,
                            "groupPath": ["wash", "left wing"],
                        },
                    ],
                    "refusal": None,
                    "warnings": [],
                    "dangling": [],
                    "unplaced": [],
                },
                "describe": "root  venue\n",
            }
        return placement()

    def last(self, method: str) -> Any:
        for name, payload in reversed(self.calls):
            if name == method:
                return payload
        raise AssertionError(f"{method} was never called")


class BuildTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workspace = Path(tempfile.mkdtemp(prefix="luma-venue-build-"))
        self.host = BuildHost()
        self.venue = Venue(record(), host_call=self.host, workspace=self.workspace)

    # -- reads ----------------------------------------------------------

    def test_the_catalog_is_the_hosts_answer_not_a_python_list(self) -> None:
        catalog = self.venue.catalog()
        self.assertIsInstance(catalog, Catalog)
        self.assertEqual(catalog.module_m, 0.5)
        # Short names are the primary vocabulary; the stored id is an alias
        # that still resolves.
        self.assertEqual(catalog["truss"].name, "truss")
        self.assertEqual(catalog["truss/straight"].name, "truss")
        self.assertEqual(catalog["truss"].size, (3.0, 0.34, 0.34))
        self.assertTrue(catalog["truss"].sized)
        self.assertIn("truss", catalog)
        # The printable page names pieces and sizes and does **not** steer a
        # reader toward socket names — those are the older layer's vocabulary.
        text = str(catalog)
        self.assertIn("truss", text)
        self.assertIn("3.00 x 0.34 x 0.34", text)
        self.assertNotIn("end_a", text)
        # Sockets are still reachable for `attach`/`extend`.
        self.assertIn("end_a", catalog.sockets("truss"))

    def test_unplaced_and_dangling_are_read_live(self) -> None:
        """Not the cell's binding snapshot: a build script asks after it has
        changed the room."""
        unplaced = self.venue.unplaced()
        self.assertEqual(unplaced[0].node_id, "mover-9")
        self.assertEqual(unplaced[0].descendants, 0)
        dangling = self.venue.dangling()
        self.assertEqual(dangling[0].socket, "end_b")
        self.assertEqual([name for name, _ in self.host.calls], ["venue.open", "venue.open"])

    def test_reach_measures_before_an_extend_refuses(self) -> None:
        reach = self.venue.reach("run-1", "end_b")
        self.assertEqual(reach.gap_m, 4.0)
        self.assertEqual(self.host.last("venue.reach")["nodeId"], "run-1")

    # -- writes ---------------------------------------------------------

    def test_place_states_intent_in_the_facade_frame(self) -> None:
        self.venue.place("truss", at=(1.5, -2.0), length=4.0, direction=(0, 0, 1))
        payload = self.host.last("venue.chain")
        self.assertEqual(payload["at"], [1.5, -2.0])
        self.assertEqual(payload["length"], 4.0)
        self.assertEqual(payload["direction"], [0.0, 0.0, 1.0])
        # Nothing else is invented on the way: no socket, no surface, no yaw.
        self.assertIsNone(payload["from"])
        self.assertIsNone(payload["on"])
        self.assertIsNone(payload["face"])

    def test_a_cursor_is_also_a_node_handle(self) -> None:
        deck = self.venue.place("truss")
        self.assertIsInstance(deck, Cursor)
        self.assertEqual(deck.size, (3.0, 0.34, 0.34))
        self.venue.attach("truss/straight", to=deck, socket="end_a")
        self.assertEqual(self.host.last("venue.attach")["parentId"], deck.id)

    def test_extend_without_a_length_asks_for_the_measured_gap(self) -> None:
        self.venue.extend("run-1", "end_b")
        self.assertIsNone(self.host.last("venue.extend")["lengthM"])
        self.venue.extend("run-1", "end_b", 3.5)
        self.assertEqual(self.host.last("venue.extend")["lengthM"], 3.5)

    def test_a_refusal_is_its_own_exception_carrying_the_resolvers_message(self) -> None:
        self.host.refuse = "5.00 m is longer than the 4.00 m gap"
        with self.assertRaises(VenueRefused) as caught:
            self.venue.extend("run-1", "end_b", 5.0)
        self.assertEqual(str(caught.exception), "5.00 m is longer than the 4.00 m gap")
        # Any other host failure keeps its own class: only a refusal means the
        # call changed nothing.
        self.host.refuse = None

    def test_a_non_refusal_host_error_is_not_a_refusal(self) -> None:
        def boom(_method: str, _payload: Any) -> Any:
            raise LumaHostCallError("internal", "the database is gone")

        venue = Venue(record(), host_call=boom, workspace=self.workspace)
        with self.assertRaises(LumaHostCallError):
            venue.detach("run-1")

    def test_trim_converts_only_the_angle(self) -> None:
        self.venue.trim("run-1", trim=6.0, yaw=45.0, span=3.0)
        params = self.host.last("venue.params")["params"]
        self.assertEqual(params["trim"], 6.0)
        self.assertEqual(params["span"], 3.0)
        self.assertAlmostEqual(params["yaw"], math.pi / 4)

    def test_duplicate_carries_the_flip_through(self) -> None:
        self.venue.duplicate("wing-1", to="deck-1", socket="corner_fr", flip=True)
        payload = self.host.last("venue.duplicate")
        self.assertEqual(payload["nodeId"], "wing-1")
        self.assertTrue(payload["flip"])

    def test_remove_answers_with_the_tree_that_is_left(self) -> None:
        self.assertIn("venue", self.venue.remove("wing-1"))

    def test_every_verb_hands_back_the_tree_it_produced(self) -> None:
        """One solve, both channels — a program never asks twice what its own
        call did."""
        placed = self.venue.attach("truss/straight", to="deck-1", socket="corner_fl")
        self.assertTrue(placed.placed)
        self.assertIn("root  venue", placed.describe())
        self.assertIn("root  venue", str(placed))

    # -- distribute ------------------------------------------------------

    def test_distribute_layouts_are_the_tagged_union_the_host_decodes(self) -> None:
        self.venue.distribute("a.qxf", 4, on="run-1", face=(0, -1, 0), mode="8-Channel")
        payload = self.host.last("venue.distribute")
        self.assertEqual(payload["layout"], {"kind": "even"})
        # The face is a vector, and the socket name never appears.
        self.assertEqual(payload["face"], [0.0, -1.0, 0.0])
        self.assertIsNone(payload["hostSocket"])
        self.venue.distribute(
            "a.qxf", 4, on="run-1", face=(0, -1, 0), mode="8-Channel", spacing_m=0.75
        )
        self.assertEqual(
            self.host.last("venue.distribute")["layout"],
            {"kind": "spacing", "metres": 0.75},
        )
        # A span is metres from midspan, like every other number here.
        self.venue.distribute(
            "a.qxf", 4, on="run-1", face=(0, -1, 0), mode="8-Channel", span=(-4, 4)
        )
        self.assertEqual(
            self.host.last("venue.distribute")["layout"],
            {"kind": "span", "from": -4.0, "to": 4.0},
        )
        # One mark on a stick host is signed metres from its middle.
        self.venue.distribute("a.qxf", 1, on="run-1", face=(0, -1, 0), mode="8-Channel", at=2.0)
        self.assertEqual(
            self.host.last("venue.distribute")["layout"], {"kind": "at", "metres": 2.0}
        )

    def test_two_ways_along_one_face_is_refused_before_the_host_sees_it(self) -> None:
        with self.assertRaises(LumaHostCallError):
            self.venue.distribute(
                "a.qxf", 4, on="run-1", face=(0, -1, 0), mode="m", span=(-1, 1), spacing_m=0.5
            )

    def test_a_distributed_row_hands_back_nodes_the_next_verb_takes(self) -> None:
        """The row a `distribute` reports is aimable without a dictionary
        lookup: every verb that names a node takes what another verb returned."""
        row = self.venue.distribute("a.qxf", 2, on="run-1", face=(0, -1, 0), mode="8-Channel")
        head = row.fixtures[0]
        self.assertEqual(head.node_id, "fix-1")
        self.assertEqual((head.universe, head.address), (1, 1))
        self.assertEqual(head.group_path, ("wash", "left wing"))
        self.venue.aim(head, tilt=30)
        self.assertEqual(self.host.last("venue.params")["nodeId"], "fix-1")

    # -- the cursor grammar ----------------------------------------------

    def test_a_chain_carries_its_own_tip_forward(self) -> None:
        """The whole point of a cursor: each step hands back the *actual* end,
        so the next step needs no arithmetic and drift cannot accumulate."""
        t = self.venue.place("truss", at=(-5.5, 0), length=8, direction=(0, 0, 1))
        t = t.add("corner")
        t = t.add("truss", length=11, direction=(1, 0, 0))
        payloads = [p for name, p in self.host.calls if name == "venue.chain"]
        self.assertEqual(len(payloads), 3)
        # The corner grew from the tower's end, and the beam from the corner's —
        # each `from` is the tip the previous answer carried.
        self.assertEqual(payloads[1]["from"]["node"], "n-1")
        self.assertEqual(payloads[2]["from"]["node"], "n-2")
        self.assertEqual(payloads[2]["direction"], [1.0, 0.0, 0.0])
        self.assertEqual(t.id, "n-3")

    def test_a_hinge_takes_an_axis_and_an_angle_in_degrees(self) -> None:
        g = self.venue.place("truss", at=(0, 8))
        g.add("hinge", axis=(0, 0, 1), angle=30)
        payload = self.host.last("venue.chain")
        self.assertEqual(payload["axis"], [0.0, 0.0, 1.0])
        self.assertEqual(payload["angle"], 30.0)
        # An angle is not a direction and the two never share a field.
        self.assertIsNone(payload["direction"])

    def test_a_cursor_with_no_single_end_says_so_rather_than_guessing(self) -> None:
        response = chained("n-9", {"at": [0.0, 0.0]})
        response["tip"] = None
        cursor = Cursor(response, self.venue)
        with self.assertRaises(LumaHostCallError):
            cursor.add("truss")

    def test_a_tip_is_grabbed_by_direction_not_by_name(self) -> None:
        top = self.venue.tip("tower-1", end=(0, 0, 1))
        self.assertEqual(self.host.last("venue.tip")["end"], [0.0, 0.0, 1.0])
        self.assertIsInstance(top, Cursor)
        # And it is a node handle as well as a cursor.
        self.assertEqual(top.size, (3.0, 0.34, 0.34))
        top.add("corner")
        self.assertEqual(self.host.last("venue.chain")["from"]["node"], "n-1")

    def test_toward_resolves_to_a_unit_vector_against_the_host(self) -> None:
        """One stated intent, resolved where the verb knows what it is measuring
        from. Only the resolved vector reaches the graph."""
        self.venue.distribute(
            "a.qxf", 4, on="n-1", face=self.venue.toward((1.0, 12.0)), mode="m"
        )
        face = self.host.last("venue.distribute")["face"]
        # The host node sits at (1, 2); the target is straight out at v=12.
        self.assertAlmostEqual(face[0], 0.0, places=6)
        self.assertAlmostEqual(face[1], 1.0, places=3)

    def test_toward_with_nothing_to_measure_from_is_refused(self) -> None:
        with self.assertRaises(LumaHostCallError):
            self.venue.place("truss", direction=self.venue.toward((0, 5)))

    # -- the read side ---------------------------------------------------

    def test_nodes_come_back_in_the_frame_the_caller_builds_in(self) -> None:
        found = self.venue.nodes(kind="run", label="wing_*")
        payload = self.host.last("venue.query")
        self.assertEqual((payload["kind"], payload["label"]), ("run", "wing_*"))
        self.assertIsInstance(found[0], NodeInfo)
        self.assertEqual(found[0].at, (1.0, 2.0))
        self.assertEqual(found[0].piece, "truss")
        self.assertEqual(found[0].face, (0.0, 0.0, 1.0))
        # A returned node feeds straight back into any verb that names one.
        self.venue.detach(found[0])
        self.assertEqual(self.host.last("venue.detach")["nodeId"], "n-1")

    def test_extent_answers_the_is_it_centred_question(self) -> None:
        span = self.venue.extent(kind="tower")
        self.assertIsInstance(span, Extent)
        self.assertEqual(span.centre, (0.0, 0.0, 4.0))
        self.assertEqual(span.size, (11.0, 0.4, 8.0))
        # A selection of handles is the same question asked of a list.
        self.venue.extent(self.venue.nodes())
        self.assertEqual(self.host.last("venue.extent")["ids"], ["n-1", "n-2"])

    def test_aim_takes_a_direction_or_a_point_but_not_both(self) -> None:
        self.venue.aim(["fix-1", "fix-2"], direction=(0, 1, -0.5))
        payload = self.host.last("venue.aim")
        self.assertEqual(payload["nodes"], ["fix-1", "fix-2"])
        self.assertEqual(payload["direction"], [0.0, 1.0, -0.5])
        self.venue.aim("fix-1", at=(0, 8, 0))
        self.assertEqual(self.host.last("venue.aim")["at"], [0.0, 8.0, 0.0])
        with self.assertRaises(LumaHostCallError):
            self.venue.aim("fix-1", direction=(0, 1, 0), at=(0, 8, 0))

    # -- drafts ----------------------------------------------------------

    def test_a_draft_runs_the_component_against_a_scratch_graph(self) -> None:
        def portal(s: Any, width: float = 11.0) -> None:
            t = s.place("truss", at=(-width / 2, 0), length=8, direction=(0, 0, 1))
            t = t.add("corner")
            beam = t.add("truss", length=width, direction=(1, 0, 0))
            s.distribute("a.qxf", 8, on=beam, face=(0, 1, 0), mode="m")

        gate = self.venue.draft(portal, width=11)
        self.assertIsInstance(gate, Draft)
        # Every call inside the function carried the draft, so the venue was
        # never touched.
        for name, payload in self.host.calls:
            if name in ("venue.chain", "venue.distribute"):
                self.assertEqual(payload["draftId"], "draft-1", name)
        self.assertEqual(self.host.last("venue.chain")["at"], None)

    def test_a_draft_previews_both_ways_and_stamps_as_copies(self) -> None:
        gate = self.venue.draft()
        self.assertIn("draft", gate.describe())
        self.assertEqual(gate.extent.count, 2)
        shot = gate.render()
        self.assertEqual(shot.artifact_rel, "outputs/stage-draft.png")
        nodes = self.venue.stamp(gate, at=(0, 5), yaw=90.0)
        payload = self.host.last("venue.stamp")
        self.assertEqual(payload["at"], [0.0, 5.0])
        self.assertAlmostEqual(payload["yaw"], math.pi / 2)
        self.assertEqual(nodes, ("s-1",))

    def test_a_component_that_raises_leaves_no_draft_open(self) -> None:
        def broken(_s: Any) -> None:
            raise ValueError("nope")

        with self.assertRaises(ValueError):
            self.venue.draft(broken)
        self.assertEqual(self.host.last("venue.draft.discard")["draftId"], "draft-1")

    # -- the library -----------------------------------------------------

    def test_the_library_page_carries_what_distribute_is_named_out_of(self) -> None:
        found = self.venue.fixtures("robe spiider")
        self.assertEqual(self.host.last("venue.fixtures")["query"], "robe spiider")
        self.assertEqual(found[0].path, "Robe/Robe-Spiider.qxf")
        self.assertEqual(found[0].beam_deg, (4.0, 50.0))
        self.assertEqual(found[0].mode(12), "Basic")
        # Printing the page is the whole of reading it.
        self.assertIn("Robe/Robe-Spiider.qxf", str(found))
        self.assertIn("39 ch", str(found))

    def test_a_mode_that_is_not_there_names_the_ones_that_are(self) -> None:
        head = self.venue.fixtures()[0]
        with self.assertRaises(LumaHostCallError) as caught:
            head.mode(7)
        self.assertIn("Mode 1 (39)", str(caught.exception))
        self.assertIn("Basic (12)", str(caught.exception))

    # -- aim -------------------------------------------------------------

    def test_aim_is_degrees_here_and_radians_on_the_wire(self) -> None:
        self.venue.aim("fix-1", pan=-90.0, tilt=45.0)
        params = self.host.last("venue.params")["params"]
        self.assertAlmostEqual(params["pan"], -math.pi / 2)
        self.assertAlmostEqual(params["tilt"], math.pi / 4)

    def test_aim_leaves_the_angle_it_was_not_given(self) -> None:
        """Half an aim is not a reset: tilting a panned head keeps the pan."""
        self.venue.aim("fix-1", tilt=20.0)
        self.assertEqual(list(self.host.last("venue.params")["params"]), ["tilt"])
        with self.assertRaises(LumaHostCallError):
            self.venue.aim("fix-1")

    def test_trim_and_aim_agree_about_which_parameters_are_angles(self) -> None:
        self.venue.trim("fix-1", pan=90.0, tilt=90.0, yaw=90.0, u=1.0)
        params = self.host.last("venue.params")["params"]
        for key in ("pan", "tilt", "yaw"):
            self.assertAlmostEqual(params[key], math.pi / 2, msg=key)
        self.assertEqual(params["u"], 1.0)

    def test_a_malformed_span_refuses_before_the_host_sees_it(self) -> None:
        for kwargs in ({"span": (1.0,)}, {"span": (1.0, 2.0, 3.0)}, {"at": float("nan")}):
            with self.assertRaises(LumaHostCallError):
                self.venue.distribute("a.qxf", 2, on="run-1", face=(0, -1, 0), mode="m", **kwargs)
        self.assertEqual(self.host.calls, [])

    def test_a_row_that_does_not_fit_is_a_report_not_an_exception(self) -> None:
        def refused(_method: str, _payload: Any) -> Any:
            return {
                "report": {
                    "hostNodeId": "run-1",
                    "hostSocket": "face_-y",
                    "fixtures": [],
                    "refusal": {
                        "kind": "tooLong",
                        "neededM": 4.0,
                        "availableM": 3.0,
                        "extendNodeId": "run-1",
                        "suggestion": "needs 4.00 m, the face is 3.00 m",
                    },
                    "warnings": [],
                    "dangling": [],
                    "unplaced": [],
                },
                "describe": "root  venue\n",
            }

        venue = Venue(record(), host_call=refused, workspace=self.workspace)
        row = venue.distribute("a.qxf", 12, on="run-1", face=(0, -1, 0), mode="8-Channel")
        self.assertIsInstance(row, Distribution)
        self.assertFalse(row.ok)
        self.assertEqual(row.needed_m, 4.0)
        self.assertIn("4.00 m", row.message)
        self.assertEqual(row.fixtures, ())

    # -- no room ---------------------------------------------------------

    def test_a_thread_with_no_venue_cannot_build(self) -> None:
        venue = Venue(record(), workspace=self.workspace)
        for call in (
            venue.describe,
            venue.dangling,
            venue.unplaced,
            lambda: venue.place("truss"),
            lambda: venue.detach("n"),
        ):
            with self.assertRaises(VenueHostUnavailableError):
                call()


class FigureCapTests(unittest.TestCase):
    def test_host_figures_count_against_the_per_cell_cap(self) -> None:
        workspace = Path(tempfile.mkdtemp(prefix="luma-venue-"))
        sink = FigureSink()
        for index in range(10):
            sink.register(f"outputs/stage-{index}.png", 8, 8)
        collected, warnings = sink.collect(workspace, None)
        self.assertEqual(len(collected), 8)
        self.assertTrue(any("only the first 8" in w for w in warnings), warnings)

    def test_a_figure_must_live_directly_under_outputs(self) -> None:
        sink = FigureSink()
        for bad in ("scratch/x.png", "outputs/nested/x.png", "/tmp/x.png"):
            with self.assertRaises(ValueError):
                sink.register(bad, 1, 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
