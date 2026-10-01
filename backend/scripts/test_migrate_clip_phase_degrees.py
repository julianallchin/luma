#!/usr/bin/env python3
"""Tests for migrate_clip_phase_degrees.py: a graph in, the exact graph out.

    python3 backend/scripts/test_migrate_clip_phase_degrees.py
"""
import collections
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_phase_degrees as m  # noqa: E402

# "Aim · line · every 16" (Windhorse), cut down: a phase curve over space
# with both bounds, and a lean curve on yaw that is not a phase.
OLD = {"version": 3, "nodes": {
    "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
    "curve1": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {
        "x": {"node": "space1"}, "shape": {"points": [[0, 0], [1, 1]]},
        "low": 1.0, "high": 0.7222222222222222}},
    "time1": {"kind": "time", "inputs": {"every": 16.0, "phase": {"node": "curve1"}}},
    "curve2": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {
        "x": {"node": "time1"}, "shape": {"points": [[0, 0], [1, 1]]},
        "low": -15.0, "high": 15.0}},
    "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "curve2"}}}}}


def rewritten(graph):
    return m.rewrite(graph, collections.Counter())


def copy(graph):
    return json.loads(json.dumps(graph))


class Rewrite(unittest.TestCase):
    def test_a_phase_curve_scales_its_bounds_and_keeps_its_shape(self):
        changes = []
        new = m.rewrite(OLD, collections.Counter(), changes)
        expected = copy(OLD)
        expected["nodes"]["curve1"]["inputs"]["low"] = 360.0
        expected["nodes"]["curve1"]["inputs"]["high"] = 260.0
        self.assertEqual(new, expected)
        self.assertEqual(list(new["nodes"]), list(OLD["nodes"]))
        self.assertEqual({c["input"] for c in changes}, {"curve1.low", "curve1.high"})

    def test_an_empty_high_becomes_360_and_an_empty_low_stays(self):
        old = copy(OLD)
        del old["nodes"]["curve1"]["inputs"]["low"]
        del old["nodes"]["curve1"]["inputs"]["high"]
        new = rewritten(old)
        self.assertEqual(new["nodes"]["curve1"]["inputs"]["high"], 360)
        self.assertNotIn("low", new["nodes"]["curve1"]["inputs"])

    def test_a_number_phase_is_degrees(self):
        for turns, degrees in [(0.25, 90.0), (0, 0), (1, 360), (-0.25, -90.0), (1.25, 450.0)]:
            old = {"version": 3, "nodes": {"time1": {"kind": "time", "inputs": {"phase": turns}}}}
            self.assertEqual(rewritten(old)["nodes"]["time1"]["inputs"]["phase"], degrees)

    def test_a_chain_scales_every_number_that_makes_the_phase(self):
        # Spiral: the phase curve's low and high are curves over radius; a
        # value and a sum on the way scale too, a product scales one item.
        old = {"version": 3, "nodes": {
            "radius": {"kind": "space", "settings": {"kind": "radial"}},
            "inner": {"kind": "curve", "inputs": {"x": {"node": "radius"}}},
            "outer": {"kind": "curve", "inputs": {"x": {"node": "radius"}, "low": 1, "high": 2}},
            "v": {"kind": "value", "inputs": {"value": 0.5}},
            "sum": {"kind": "math", "settings": {"op": "+"}, "inputs": {"values": [{"node": "outer"}, {"node": "v"}]}},
            "prod": {"kind": "math", "settings": {"op": "*"}, "inputs": {"values": [{"node": "inner"}, 0.5]}},
            "lag": {"kind": "curve", "inputs": {"x": {"node": "radius"}, "low": {"node": "prod"}, "high": {"node": "sum"}}},
            "t": {"kind": "time", "inputs": {"every": 4, "phase": {"node": "lag"}}}}}
        new = rewritten(old)["nodes"]
        self.assertEqual(new["outer"]["inputs"], {"x": {"node": "radius"}, "low": 360, "high": 720})
        self.assertEqual(new["v"]["inputs"]["value"], 180.0)
        self.assertEqual(new["prod"]["inputs"]["values"], [{"node": "inner"}, 180.0])
        # The product scaled its number, so the curve under it stays.
        self.assertEqual(new["inner"], old["nodes"]["inner"])
        self.assertEqual(new["lag"]["inputs"], old["nodes"]["lag"]["inputs"])

    def test_a_curve_shared_by_two_phases_scales_once(self):
        old = copy(OLD)
        old["nodes"]["time2"] = {"kind": "time", "inputs": {"every": 16.0, "phase": {"node": "curve1"}}}
        new = rewritten(old)
        self.assertEqual(new["nodes"]["curve1"]["inputs"]["low"], 360.0)

    def test_a_node_that_also_feeds_something_else_gets_a_product(self):
        old = copy(OLD)
        old["nodes"]["color1"] = {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}
        notes = collections.Counter()
        new = m.rewrite(old, notes)
        self.assertEqual(new["nodes"]["curve1"], old["nodes"]["curve1"])
        self.assertEqual(new["nodes"]["time1"]["inputs"]["phase"], {"node": "math1"})
        self.assertEqual(new["nodes"]["math1"], {"kind": "math", "settings": {"op": "*"},
                                                 "inputs": {"values": [{"node": "curve1"}, 360]}})
        self.assertEqual(notes, {"shared: phase through a new product with 360": 1})

    def test_a_graph_with_no_phase_is_left_alone(self):
        old = copy(OLD)
        del old["nodes"]["time1"]["inputs"]["phase"]
        self.assertIs(rewritten(old), old)

    def test_drafts_keep_their_key_order(self):
        document = {"clips": {"c1": {"start": 1, "graph": OLD, "name": "x"}}, "z": 1}
        new = m.rewrite_document(document, collections.Counter())
        self.assertEqual(list(new["clips"]["c1"]), ["start", "graph", "name"])
        self.assertEqual(new["clips"]["c1"]["graph"]["nodes"]["curve1"]["inputs"]["low"], 360.0)

    def test_presets_keep_their_layout(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "presets.json"
            plain = {"name": "Wash", "graph": {"version": 3, "nodes": {"color1": {"kind": "color"}}}}
            phased = {"name": "Wave", "graph": OLD}
            path.write_text('{\n  "clips": [\n    ' + json.dumps(plain) + ',\n    '
                            + json.dumps(phased) + '\n  ]\n}\n')
            self.assertEqual(m.rewrite_presets(path), ["Wave"])
            lines = path.read_text().split("\n")
            self.assertEqual(lines[2], "    " + json.dumps(plain) + ",")
            data = json.loads(path.read_text())
            self.assertEqual(data["clips"][1]["graph"]["nodes"]["curve1"]["inputs"]["high"], 260.0)


if __name__ == "__main__":
    unittest.main()
