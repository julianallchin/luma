#!/usr/bin/env python3
"""Focused stdlib tests for the agent-facing track API.

Run directly with either the bundled environment or an ordinary Python that
has numpy/matplotlib::

    python3 src-tauri/python/luma_exec/tests/test_track.py
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

import numpy as np  # noqa: E402

from luma_exec.bindings import ArtifactStore  # noqa: E402
from luma_exec.track import (  # noqa: E402
    Track,
    TrackClosedError,
    TrackError,
)


PATTERNS = {
    "summaries": [
        {"id": "wash-blue", "name": "Blue Wash"},
        {"id": "hit", "name": "Hit"},
    ],
    "argument_schemas": {
        "wash-blue": [
            {"id": "selection", "name": "Fixtures", "argType": "Selection"},
            {"id": "intensity", "name": "Intensity", "argType": "Scalar"},
        ],
        "hit": [],
    },
}

FEATURES = {
    # One two-second bar. Bar n starts at 2 * (n - 1).
    "downbeats": np.arange(0.0, 22.0, 2.0),
    "bpm": 120.0,
    "beats_per_bar": 4,
}


def clip(
    clip_id: str,
    pattern_id: str,
    start: float,
    end: float,
    z: int,
    *,
    name: str | None = None,
    args: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "id": clip_id,
        "pattern_id": pattern_id,
        "pattern_name": name,
        "start_s": start,
        "end_s": end,
        "z": z,
        "blend": "replace",
        "args": args or {},
    }


def values(*, clips: list[dict[str, Any]] | None = None) -> dict[str, Any]:
    return {
        "id": "track-1",
        "title": "Synthetic",
        "artist": "Test",
        "duration_s": 20.0,
        "revision": "sha256:base",
        "editable": True,
        "clips": clips
        or [
            clip("left", "wash-blue", 0.0, 5.0, 0, name="Blue Wash"),
            clip("inside", "hit", 5.0, 7.0, 1, name="Hit"),
            clip("right", "wash-blue", 7.0, 12.0, 2, name="Blue Wash"),
            clip("outside", "hit", 14.0, 16.0, 0, name="Hit"),
        ],
    }


class Host:
    def __init__(self, render: Any = None) -> None:
        self.calls: list[tuple[str, Any]] = []
        self.render = render

    def __call__(self, method: str, payload: Any) -> Any:
        self.calls.append((method, payload))
        if method == "track.check":
            # The real Rust result is TrackEditCheck, not an `{ok: true}` shell.
            return {
                "baseRevision": payload["baseRevision"],
                "candidate": payload["candidate"],
            }
        if method == "track.render":
            if self.render is not None:
                return self.render
            return {
                "values": np.full((3, 16, 3), 0.25, dtype=np.float32),
                "lightIds": ["front-left", "front-right", "back"],
                "timesS": np.linspace(payload["startTime"], payload["endTime"], 16),
            }
        if method == "track.apply":
            id_map = {
                item["id"]: "added-uuid"
                for item in payload["candidate"]
                if item["id"].startswith("new:")
            }
            materialized = []
            for item in payload["candidate"]:
                got = dict(item)
                got["id"] = id_map.get(item["id"], item["id"])
                materialized.append(got)
            return {
                "revision": "sha256:next",
                "clips": materialized,
                "idMap": id_map,
                "added": len(id_map),
                "updated": 0,
                "removed": 0,
            }
        raise AssertionError(f"unexpected host method {method}")


def make_track(host: Host | None = None, **kwargs: Any) -> Track:
    return Track(
        kwargs.pop("values", values()),
        patterns=kwargs.pop("patterns", PATTERNS),
        features=kwargs.pop("features", FEATURES),
        host_call=host,
        **kwargs,
    )


class TrackEditingTests(unittest.TestCase):
    def test_track_snapshot_is_immutable_without_losing_bound_scalar_access(self) -> None:
        source = values()
        source["album"] = "Bound album"
        track = make_track(values=source)

        self.assertEqual(track.album, "Bound album")
        for field, replacement in [
            ("clips", ()),
            ("revision", "sha256:forged"),
            ("editable", False),
        ]:
            with self.subTest(field=field), self.assertRaisesRegex(
                AttributeError, "immutable"
            ):
                setattr(track, field, replacement)

        # The explicit mutable surface remains available over the intact base.
        self.assertEqual(len(track.edit().clips), 4)

    def test_edit_is_full_candidate_and_base_is_immutable(self) -> None:
        track = make_track()
        edit = track.edit()
        added = edit.add_clip(
            "Blue Wash",
            seconds=(16.0, 18.0),
            z=3,
            args={"Intensity": 0.7},
            selection="front_wash",
        )

        self.assertTrue(added.id.startswith("new:"))
        self.assertEqual(added.pattern_id, "wash-blue")
        self.assertEqual(added.pattern_name, "Blue Wash")
        self.assertEqual(added.args["intensity"], 0.7)
        self.assertEqual(
            dict(added.args["selection"]),
            {"expression": "front_wash", "spatialReference": "global"},
        )
        self.assertEqual(len(track.clips), 4)
        self.assertEqual(len(edit.clips), 5)
        with self.assertRaises(TypeError):
            added.args["intensity"] = 1.0  # type: ignore[index]

    def test_update_merges_args_and_remove_is_exact(self) -> None:
        original = values(
            clips=[
                clip(
                    "a",
                    "wash-blue",
                    0.0,
                    2.0,
                    0,
                    name="Blue Wash",
                    args={"selection": {"expression": "all", "spatialReference": "global"}},
                )
            ]
        )
        edit = make_track(values=original).edit()
        updated = edit.update_clip(
            "a",
            seconds=(2.0, 4.0),
            args={"Intensity": 0.4},
            selection="back_wall",
        )
        self.assertEqual((updated.start_s, updated.end_s), (2.0, 4.0))
        self.assertEqual(updated.args["intensity"], 0.4)
        self.assertEqual(updated.args["selection"]["expression"], "back_wall")
        self.assertEqual(edit.remove_clip("a").id, "a")
        self.assertEqual(edit.clips, ())
        with self.assertRaisesRegex(TrackError, "unknown clip"):
            edit.remove_clip("a")

    def test_timing_edit_preserves_legacy_non_object_args_losslessly(self) -> None:
        legacy = values(
            clips=[
                {
                    **clip("legacy", "hit", 0.0, 2.0, 0),
                    "args": ["old", {"shape": 3}],
                }
            ]
        )
        edit = make_track(values=legacy).edit()
        updated = edit.update_clip("legacy", seconds=(2.0, 4.0))
        self.assertEqual(updated.args, ("old", {"shape": 3}))
        self.assertEqual(edit._plan()["candidate"][0]["args"], ["old", {"shape": 3}])
        with self.assertRaisesRegex(TrackError, "legacy non-object"):
            edit.update_clip("legacy", args={"anything": 1})

    def test_pattern_and_argument_names_must_be_unambiguous(self) -> None:
        ambiguous = {
            "summaries": [
                {"id": "one", "name": "Wash"},
                {"id": "two", "name": "wash"},
            ],
            "argument_schemas": {},
        }
        edit = make_track(patterns=ambiguous).edit()
        with self.assertRaisesRegex(TrackError, "ambiguous"):
            edit.add_clip("Wash", seconds=(1, 2), z=0)

    def test_new_unknown_arguments_are_rejected_but_untouched_legacy_extras_survive(
        self,
    ) -> None:
        original = values(
            clips=[
                clip(
                    "a",
                    "wash-blue",
                    0.0,
                    2.0,
                    0,
                    name="Blue Wash",
                    args={"intensity": 0.2, "legacy-extra": {"shape": 3}},
                )
            ]
        )
        edit = make_track(values=original).edit()
        updated = edit.update_clip("a", args={"Intensity": 0.4})
        self.assertEqual(updated.args["intensity"], 0.4)
        self.assertEqual(updated.args["legacy-extra"], {"shape": 3})

        with self.assertRaisesRegex(TrackError, "unknown argument"):
            edit.add_clip(
                "Blue Wash",
                seconds=(3.0, 4.0),
                z=1,
                args={"Intensitty": 0.7},
            )
        with self.assertRaisesRegex(TrackError, "unknown argument"):
            edit.update_clip("a", args={"made-up-id": 1})
        with self.assertRaisesRegex(TrackError, "unknown argument"):
            edit.update_clip("a", unset_args=("legacy-extra",))

    def test_diff_is_semantic_and_compact(self) -> None:
        edit = make_track().edit()
        edit.update_clip("inside", blend="add")
        edit.remove_clip("outside")
        edit.add_clip("Hit", seconds=(18, 19), z=4)
        difference = edit.diff()
        self.assertEqual(len(difference.added), 1)
        self.assertEqual(len(difference.updated), 1)
        self.assertEqual(len(difference.removed), 1)
        self.assertIn("<TrackDiff +1 ~1 -1>", repr(difference))

    def test_local_check_catches_same_layer_overlap_before_host(self) -> None:
        host = Host()
        edit = make_track(host).edit()
        edit.add_clip("Hit", seconds=(1.0, 2.0), z=0)
        checked = edit.check()
        self.assertFalse(checked)
        self.assertIn("overlap", checked.errors[0])
        self.assertEqual(host.calls, [])

    def test_local_check_rejects_new_out_of_track_ranges(self) -> None:
        for seconds, message in [
            ((-1.0, 0.5), "starts before the track"),
            ((19.0, 21.0), "ends after the track"),
        ]:
            with self.subTest(seconds=seconds):
                host = Host()
                edit = make_track(host).edit()
                edit.add_clip("Hit", seconds=seconds, z=9)
                checked = edit.check()
                self.assertFalse(checked)
                self.assertTrue(any(message in error for error in checked.errors))
                self.assertEqual(host.calls, [])

    def test_local_check_allows_unchanged_legacy_out_of_track_ranges(self) -> None:
        legacy = values(
            clips=[
                clip("before", "hit", -1.0, 1.0, 0),
                clip("after", "hit", 19.0, 21.0, 1),
            ]
        )
        host = Host()
        edit = make_track(host, values=legacy).edit()
        edit.update_clip("before", z=2)
        edit.update_clip("after", blend="add")
        self.assertTrue(edit.check())
        self.assertEqual(host.calls[-1][0], "track.check")

        changed = make_track(Host(), values=legacy).edit()
        changed.update_clip("before", seconds=(-0.5, 1.0))
        checked = changed.check()
        self.assertFalse(checked)
        self.assertTrue(any("starts before" in error for error in checked.errors))

    def test_local_check_preserves_a_legacy_overlap(self) -> None:
        host = Host()
        legacy = values(
            clips=[
                clip("a", "hit", 0.0, 3.0, 0),
                clip("b", "wash-blue", 2.0, 4.0, 0),
            ]
        )
        checked = make_track(host, values=legacy).edit().check()
        self.assertTrue(checked)
        self.assertEqual(host.calls[-1][0], "track.check")

    def test_check_sends_only_revision_and_complete_candidate(self) -> None:
        host = Host()
        edit = make_track(host).edit()
        checked = edit.check()
        self.assertTrue(checked)
        method, payload = host.calls[-1]
        self.assertEqual(method, "track.check")
        self.assertEqual(set(payload), {"baseRevision", "candidate"})
        self.assertEqual(len(payload["candidate"]), 4)
        self.assertNotIn("scoreId", payload)
        self.assertEqual(
            set(payload["candidate"][0]),
            {"id", "patternId", "startTime", "endTime", "zIndex", "blendMode", "args"},
        )

    def test_check_turns_expected_host_validation_rejections_into_results(self) -> None:
        from luma_exec.host_errors import LumaHostCallError

        class RejectingHost:
            def __init__(self, code: str) -> None:
                self.code = code

            def __call__(self, _method: str, _payload: Any) -> Any:
                raise LumaHostCallError(self.code, f"rejected as {self.code}")

        for code in ["invalid_edit", "compile_error"]:
            with self.subTest(code=code):
                checked = make_track(RejectingHost(code)).edit().check()
                self.assertFalse(checked)
                self.assertEqual(checked.errors, (f"rejected as {code}",))

        for code in ["conflict", "forbidden", "internal"]:
            with self.subTest(code=code), self.assertRaises(LumaHostCallError) as caught:
                make_track(RejectingHost(code)).edit().check()
            self.assertEqual(caught.exception.code, code)

    def test_apply_materializes_temp_ids_and_closes_edit(self) -> None:
        host = Host()
        edit = make_track(host).edit()
        added = edit.add_clip("Hit", seconds=(18.0, 19.0), z=4)
        result = edit.apply()
        self.assertEqual(result.revision, "sha256:next")
        self.assertEqual(result.id_map[added.id], "added-uuid")
        self.assertEqual(result.added, 1)
        with self.assertRaises(TrackClosedError):
            edit.add_clip("Hit", seconds=(19, 20), z=4)

    def test_noop_apply_still_uses_authoritative_cas_and_response(self) -> None:
        host = Host()
        edit = make_track(host).edit()
        result = edit.apply()

        self.assertEqual(host.calls[-1][0], "track.apply")
        self.assertEqual(result.revision, "sha256:next")
        self.assertEqual(len(result.clips), 4)
        self.assertFalse(result.applied)
        with self.assertRaises(TrackClosedError):
            edit.remove_clip("inside")

    def test_noop_apply_does_not_hide_a_stale_base(self) -> None:
        from luma_exec.host_errors import LumaHostCallError

        class ConflictHost:
            def __call__(self, _method: str, _payload: Any) -> Any:
                raise LumaHostCallError("conflict", "the live track changed")

        edit = make_track(ConflictHost()).edit()
        with self.assertRaises(LumaHostCallError) as caught:
            edit.apply()
        self.assertEqual(caught.exception.code, "conflict")
        # A failed authoritative apply never pretends the local draft closed.
        self.assertIn("open", repr(edit))


class TrackWindowTests(unittest.TestCase):
    def tearDown(self) -> None:
        import matplotlib.pyplot as plt

        plt.close("all")

    def test_ranges_are_half_open_and_include_crossing_unchanged_clips(self) -> None:
        track = make_track()
        # bars [2, 5) -> seconds [2, 8). left and right cross the edges;
        # outside begins later and is excluded.
        window = track.window(bars=(2, 5))
        self.assertEqual((window.start_s, window.end_s), (2.0, 8.0))
        self.assertEqual([item.id for item in window.clips], ["left", "inside", "right"])

        boundary = values(
            clips=[
                clip("ends-at-start", "hit", 0, 2, 0),
                clip("inside", "hit", 2, 4, 0),
                clip("starts-at-end", "hit", 4, 6, 0),
            ]
        )
        got = make_track(values=boundary).window(seconds=(2, 4))
        self.assertEqual([item.id for item in got.clips], ["inside"])

    def test_window_is_an_immutable_candidate_snapshot(self) -> None:
        edit = make_track().edit()
        before = edit.window(seconds=(0, 10))
        edit.remove_clip("inside")
        edit.add_clip("Hit", seconds=(8.0, 9.0), z=4)
        self.assertIn("inside", [item.id for item in before.clips])
        self.assertEqual(len(before._candidate), 4)
        for field, replacement in [
            ("start_s", 4.0),
            ("clips", ()),
            ("bars", (2.0, 3.0)),
        ]:
            with self.subTest(field=field), self.assertRaisesRegex(
                AttributeError, "immutable"
            ):
                setattr(before, field, replacement)

    def test_timeline_returns_a_real_matplotlib_figure(self) -> None:
        window = make_track().window(seconds=(2.0, 8.0))
        figure = window.timeline()
        self.assertEqual(len(figure.axes), 1)
        self.assertEqual(len(figure.axes[0].patches), 3)
        self.assertIn("seconds [2, 8)", figure.axes[0].get_title())

    def test_heatmap_calls_real_candidate_render_with_explicit_range(self) -> None:
        host = Host()
        edit = make_track(host).edit()
        edit.remove_clip("outside")
        added = edit.add_clip("Hit", seconds=(8.0, 9.0), z=4)
        window = edit.window(seconds=(2.0, 10.0))
        figure = window.output.heatmap()

        method, payload = host.calls[-1]
        self.assertEqual(method, "track.render")
        self.assertEqual(payload["startTime"], 2.0)
        self.assertEqual(payload["endTime"], 10.0)
        # The host receives the whole candidate, not merely the window or delta.
        ids = {item["id"] for item in payload["candidate"]}
        self.assertIn("left", ids)
        self.assertIn(added.id, ids)
        self.assertNotIn("outside", ids)
        self.assertEqual(window.output.values.shape, (3, 16, 3))
        self.assertEqual(window.output.light_ids, ["front-left", "front-right", "back"])
        self.assertEqual(len(figure.axes), 1)

    def test_artifact_render_response_reuses_luma_tensor_loader(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            workspace = Path(root)
            (workspace / "inputs").mkdir()
            data = np.linspace(0, 1, 2 * 4 * 3, dtype="<f4").reshape(2, 4, 3)
            data.tofile(workspace / "inputs" / "render.bin")
            store = ArtifactStore(workspace, {})
            response = {
                "tensor": {
                    "$kind": "tensor",
                    "artifact_id": "render-1",
                    "dtype": "f32",
                    "shape": [2, 4, 3],
                    "byte_offset": 0,
                    "axes": [
                        {"kind": "labels", "name": "light", "labels": ["a", "b"]},
                        {
                            "kind": "linear",
                            "name": "time",
                            "start": 2.0,
                            "step": 0.5,
                            "count": 4,
                            "unit": "s",
                        },
                        {"kind": "labels", "name": "channel", "labels": ["r", "g", "b"]},
                    ],
                    "unit": None,
                    "provenance": {"source": "track_compositor"},
                },
                "artifact": {
                    "id": "render-1",
                    "kind": "tensor",
                    "encoding": "raw_le",
                    "rel_path": "inputs/render.bin",
                    "byte_len": data.nbytes,
                },
            }
            host = Host(render=response)
            track = make_track(host, artifact_store=store)
            output = track.window(seconds=(2.0, 4.0)).output
            np.testing.assert_allclose(output.values, data)
            self.assertEqual(output.light_ids, ["a", "b"])
            self.assertEqual(output.tensor.path, "luma.track.window.output")


if __name__ == "__main__":
    unittest.main(verbosity=2)
