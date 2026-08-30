#!/usr/bin/env python3
"""Focused stdlib tests for `luma.venue` and the one figure list.

Run directly with either the bundled environment or an ordinary Python::

    python3 src-tauri/python/luma_exec/tests/test_venue.py
"""

from __future__ import annotations

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
from luma_exec.venue import Venue, VenueHostUnavailableError  # noqa: E402

VIEWS = [
    "front",
    "audience",
    "overhead",
    "quarter_left",
    "quarter_right",
    "dj",
    "pov:mover-1",
]

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
                    },
                )
            ],
        )
        self.assertEqual(shot.view, "dj")
        self.assertEqual((shot.width, shot.height), (320, 200))
        self.assertEqual(repr(shot), "<StageImage dj t=12.5s 320x200>")
        self.assertTrue(shot.path.is_absolute())
        self.assertEqual(shot.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")

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

    def test_a_head_is_offered_as_a_view_like_any_other(self) -> None:
        """`pov:<id>` entries arrive in the same manifest list as `front`."""
        self.assertIn("pov:mover-1", self.venue.views)


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
