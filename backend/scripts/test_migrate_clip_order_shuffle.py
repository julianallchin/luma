#!/usr/bin/env python3
"""Tests for migrate_clip_order_shuffle.py: a graph in, the exact graph out.
With `LUMA_V3_PARITY` naming a version 3 clip_graph_parity binary, the new
graph passes the checker and plays.

    python3 backend/scripts/test_migrate_clip_order_shuffle.py
"""
import collections
import json
import os
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_order_shuffle as m  # noqa: E402

# "Aim · order · every 2" (Skanka), cut down: an order space read by a
# phase curve and a lean curve.
OLD = {"version": 3, "nodes": {
    "space1": {"kind": "space", "settings": {"kind": "order", "wrap": "no"}},
    "curve1": {"kind": "curve", "settings": {"kind": "number"},
               "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 0], [1, 1]]}}},
    "time1": {"kind": "time", "inputs": {"every": 2.0, "phase": {"node": "curve1"}}},
    "curve2": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {
        "x": {"node": "time1"}, "shape": {"points": [[0.0, 0.0], [1.0, 1.0]]},
        "low": -22.0, "high": 22.0}},
    "curve3": {"kind": "curve", "settings": {"kind": "vector"}, "inputs": {
        "x": {"node": "space1"}, "shape": {"points": [[0, 0], [1, 1]]},
        "low": [-0.42, 0.85, -0.31], "high": [0.42, 0.85, -0.31]}},
    "aim1": {"kind": "aim", "settings": {"base": "direction"},
             "inputs": {"direction": {"node": "curve3"}, "yaw": {"node": "curve2"}}}}}

SHUFFLED = {"version": 3, "nodes": {
    "time1": {"kind": "time", "inputs": {"every": 1}},
    "shuffle1": {"kind": "shuffle", "inputs": {"time": {"node": "time1"}}},
    "space1": {"kind": "space", "settings": {"kind": "order", "wrap": "no"},
               "inputs": {"heads": {"node": "shuffle1"}}},
    "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"},
                                           "shape": {"points": [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]}}},
    "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}}


class Rewrite(unittest.TestCase):
    def test_an_order_over_all_heads_gets_a_shuffle(self):
        notes = collections.Counter()
        new = m.rewrite(OLD, notes)
        expected = json.loads(json.dumps(OLD))
        expected["nodes"]["shuffle1"] = {"kind": "shuffle"}
        expected["nodes"]["space1"]["inputs"] = {"heads": {"node": "shuffle1"}}
        self.assertEqual(new, expected)
        self.assertEqual(list(new["nodes"])[:-1], list(OLD["nodes"]))
        self.assertEqual(notes, {"shuffle added": 1})
        self.assertEqual(m.rewrite(new, collections.Counter()), new)

    def test_a_shuffled_order_and_a_line_are_left_alone(self):
        self.assertIs(m.rewrite(SHUFFLED, collections.Counter()), SHUFFLED)
        line = json.loads(json.dumps(OLD))
        line["nodes"]["space1"]["settings"]["kind"] = "line"
        self.assertIs(m.rewrite(line, collections.Counter()), line)

    def test_a_taken_id_is_skipped_and_other_inputs_stay(self):
        old = json.loads(json.dumps(OLD))
        old["nodes"]["shuffle1"] = {"kind": "value", "inputs": {"value": 0.5}}
        old["nodes"]["space1"]["inputs"] = {"direction": [1, 0, 0]}
        new = m.rewrite(old, collections.Counter())
        self.assertEqual(new["nodes"]["shuffle2"], {"kind": "shuffle"})
        self.assertEqual(new["nodes"]["space1"]["inputs"],
                         {"heads": {"node": "shuffle2"}, "direction": [1, 0, 0]})

    def test_an_order_over_unshuffled_heads_is_counted_not_changed(self):
        old = json.loads(json.dumps(OLD))
        old["nodes"]["group1"] = {"kind": "group"}
        old["nodes"]["space1"]["inputs"] = {"heads": {"node": "group1"}}
        notes = collections.Counter()
        self.assertIs(m.rewrite(old, notes), old)
        self.assertEqual(notes, {"left alone: order over a heads wire with no shuffle": 1})

    def test_drafts_keep_their_key_order(self):
        document = {"clips": {"c1": {"start": 1, "graph": OLD, "name": "x"}}, "z": 1}
        new = m.rewrite_document(document, collections.Counter())
        self.assertEqual(list(new["clips"]["c1"]), ["start", "graph", "name"])
        self.assertIn("shuffle1", new["clips"]["c1"]["graph"]["nodes"])
        self.assertEqual(new["z"], 1)

    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY"), "needs a version 3 clip_graph_parity")
    def test_new_graph_passes_the_checker_and_plays(self):
        binary = os.environ["LUMA_V3_PARITY"]
        clip = {"name": "Aim · order · every 2", "start": 0.0, "duration": 16.0, "seed": 7,
                "blend_mode": "replace", "graph": m.rewrite(OLD, collections.Counter())}
        [check] = m.front.v3.v2.evaluate(binary, [clip], "--check")
        self.assertNotIn("error", check)
        for cells in m.front.v3.RIGS.values():
            [played] = m.front.v3.play(binary, [clip], cells)
            self.assertNotIn("error", played)


if __name__ == "__main__":
    unittest.main()
