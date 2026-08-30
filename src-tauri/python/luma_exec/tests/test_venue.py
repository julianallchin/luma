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
    Distribution,
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
            "catalogRef": "truss/straight",
            "name": "Truss",
            "group": "Trusses",
            "pieceKind": "truss",
            "procedural": True,
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
        }
    ],
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
        self.assertIn("end_a", catalog.sockets("truss/straight"))
        self.assertEqual(catalog.length_step_m, 0.5)
        # The printable form is what an agent reads; it names every mating
        # socket and hides the grab, which nothing can be bolted to.
        text = str(catalog)
        self.assertIn("truss/straight", text)
        self.assertIn("end_a(n)", text)
        self.assertNotIn("grab", text)

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

    def test_place_sends_uv_and_radians_and_keeps_extra_params(self) -> None:
        self.venue.place("truss/straight", at=(1.5, -2.0), yaw=90.0, span=4.0)
        payload = self.host.last("venue.place")
        self.assertEqual((payload["u"], payload["v"]), (1.5, -2.0))
        self.assertAlmostEqual(payload["yaw"], math.pi / 2)
        self.assertEqual(payload["params"], {"span": 4.0})
        # Nothing named means the catalog decides both the surface and the
        # footing — that is what keeps socket names out of the caller's head.
        self.assertIsNone(payload["surfaceNodeId"])
        self.assertIsNone(payload["mySocket"])

    def test_a_placement_can_be_passed_straight_back_as_a_node(self) -> None:
        deck = self.venue.place("truss/straight")
        self.assertIsInstance(deck, Placement)
        self.venue.attach("truss/straight", to=deck, socket="end_a")
        self.assertEqual(self.host.last("venue.attach")["parentId"], deck.node_id)

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
        self.venue.distribute("run-1", "face_-y", "a.qxf", 4, mode="8-Channel")
        self.assertEqual(self.host.last("venue.distribute")["layout"], {"kind": "even"})
        self.venue.distribute(
            "run-1", "face_-y", "a.qxf", 4, mode="8-Channel", layout="spacing", spacing_m=0.75
        )
        self.assertEqual(
            self.host.last("venue.distribute")["layout"],
            {"kind": "spacing", "metres": 0.75},
        )
        self.venue.distribute(
            "run-1", "face_-y", "a.qxf", 4, mode="8-Channel", layout="span", span=(0.1, 0.9)
        )
        self.assertEqual(
            self.host.last("venue.distribute")["layout"],
            {"kind": "span", "from": 0.1, "to": 0.9},
        )

    def test_a_distributed_row_hands_back_nodes_the_next_verb_takes(self) -> None:
        """The row a `distribute` reports is aimable without a dictionary
        lookup: every verb that names a node takes what another verb returned."""
        row = self.venue.distribute("run-1", "face_-y", "a.qxf", 2, mode="8-Channel")
        head = row.fixtures[0]
        self.assertEqual(head.node_id, "fix-1")
        self.assertEqual((head.universe, head.address), (1, 1))
        self.assertEqual(head.group_path, ("wash", "left wing"))
        self.venue.aim(head, tilt=30)
        self.assertEqual(self.host.last("venue.params")["nodeId"], "fix-1")

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

    def test_a_layout_that_needs_a_number_refuses_before_the_host_sees_it(self) -> None:
        for kwargs in ({"layout": "spacing"}, {"layout": "span"}, {"layout": "wat"}):
            with self.assertRaises(LumaHostCallError):
                self.venue.distribute("run-1", "f", "a.qxf", 2, mode="m", **kwargs)
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
        row = venue.distribute("run-1", "face_-y", "a.qxf", 12, mode="8-Channel")
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
            lambda: venue.place("truss/straight"),
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
