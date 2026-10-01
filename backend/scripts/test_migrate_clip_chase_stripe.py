#!/usr/bin/env python3
"""Tests for migrate_clip_chase_stripe.py: the clip's graph in, the exact
graph out. With `LUMA_V3_PARITY` naming a version 3 clip_graph_parity
binary, the new graph passes the checker and plays the same as before on
both stand-in rigs.

    python3 backend/scripts/test_migrate_clip_chase_stripe.py
"""
import copy
import os
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_chase_stripe as m  # noqa: E402

W = 0.084859331449
OLD = {"version": 3, "nodes": {
    "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}, "color": [1.0, 0.0, 0.01]}},
    "curve1": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {
        "high": 0.839063466772, "low": 0.564461943555,
        "shape": {"points": [[0.0, 0.0, [0.0, 0.0, 0.2969581474143852, 1.0]],
                             [0.999999, 1.0, "hold"], [1.0, 1.0]]},
        "x": {"node": "time1"}}},
    "curve2": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {
        "high": 0.85, "low": 0.0,
        "shape": {"points": [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]},
        "x": {"node": "space1"}}},
    "mirror1": {"kind": "mirror", "inputs": {"direction": [1.0, 0.0, 0.0]}},
    "mirror2": {"kind": "mirror", "inputs": {"direction": [0.0, 0.0, 1.0], "heads": {"node": "mirror1"}}},
    "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}, "inputs": {
        "direction": [0.33, 0.0, 1.0], "heads": {"node": "mirror2"}, "scale": W,
        "shift": {"node": "curve1"}}},
    "time1": {"kind": "time", "inputs": {"duration": 8.0, "every": 8.0}}}}


def clip(g):
    return {"name": m.NAME, "start": 192.0, "duration": 32.0, "seed": 9504895740809610503,
            "blend_mode": "max", "graph": g}


class Rewrite(unittest.TestCase):
    def test_width_moves_into_the_curve(self):
        new = m.rewrite(OLD)
        expected = copy.deepcopy(OLD)
        del expected["nodes"]["space1"]["inputs"]["scale"]
        expected["nodes"]["curve2"]["inputs"]["shape"]["points"] = [[0, 0], [0, 1], [W, 1], [W, 0], [1, 0]]
        self.assertEqual(new, expected)
        with self.assertRaises(m.LeftAlone):
            m.rewrite(new)

    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY"), "needs a version 3 clip_graph_parity")
    def test_new_graph_passes_the_checker_and_plays_the_same(self):
        binary = os.environ["LUMA_V3_PARITY"]
        a, b = clip(OLD), clip(m.rewrite(OLD))
        [check] = m.front.v3.v2.evaluate(binary, [b], "--check")
        self.assertNotIn("error", check)
        for rig, cells in m.front.v3.RIGS.items():
            x, y = m.front.v3.play(binary, [a, b], cells)
            self.assertNotIn("error", x)
            self.assertLessEqual(m.front.v3.difference(x["ok"], y["ok"])[0], m.front.EXACT, rig)


if __name__ == "__main__":
    unittest.main()
