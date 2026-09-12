#!/usr/bin/env python3
"""Focused stdlib tests for the agent-facing track API.

Run directly with either the bundled environment or an ordinary Python that
has numpy/matplotlib::

    python3 backend/python/luma_exec/tests/test_track.py
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

from luma_exec.track import Track  # noqa: E402


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


def make_track(host: Any = None, **kwargs: Any) -> Track:
    return Track(
        kwargs.pop("values", values()),
        patterns=kwargs.pop("patterns", PATTERNS),
        features=kwargs.pop("features", FEATURES),
        host_call=host,
        **kwargs,
    )


class TrackSnapshotTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
