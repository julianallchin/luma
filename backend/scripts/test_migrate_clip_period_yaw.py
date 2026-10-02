#!/usr/bin/env python3
"""Tests for migrate_clip_period_yaw.py: a graph in, the exact graph out.

    python3 backend/scripts/test_migrate_clip_period_yaw.py
"""
import collections
import json
import pathlib
import sqlite3
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_period_yaw as m  # noqa: E402

# Ballyhoo as it shipped: noise on `speed`, yaw from a curve whose low and
# high are value nodes that the pitch curve reads too.
BALLYHOO = {"version": 3, "nodes": {
    "speed": {"kind": "value", "inputs": {"value": 4.0}},
    "low": {"kind": "value", "inputs": {"value": -40.0}},
    "high": {"kind": "value", "inputs": {"value": 40.0}},
    "drift": {"kind": "noise", "inputs": {"speed": {"node": "speed"}, "scale": 0.02}},
    "curve1": {"kind": "curve", "inputs": {"x": {"node": "drift"},
                                           "low": {"node": "low"}, "high": {"node": "high"}}},
    "curve2": {"kind": "curve", "inputs": {"x": {"node": "drift"},
                                           "low": {"node": "low"}, "high": {"node": "high"}}},
    "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "curve1"}, "pitch": {"node": "curve2"}}}}}

FAN = {"version": 3, "nodes": {
    "place": {"kind": "space"},
    "curve1": {"kind": "curve", "inputs": {"x": {"node": "place"}, "low": -25.0, "high": 25}},
    "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "curve1"}}}}}


def rewritten(graph, notes=None):
    return m.rewrite(graph, collections.Counter() if notes is None else notes)


def copy(graph):
    return json.loads(json.dumps(graph))


class Rewrite(unittest.TestCase):
    def test_noise_speed_is_period_in_the_same_place(self):
        new = rewritten(BALLYHOO)["nodes"]["drift"]["inputs"]
        self.assertEqual(list(new), ["period", "scale"])
        self.assertEqual(new["period"], {"node": "speed"})

    def test_opposite_value_bounds_swap_their_wires(self):
        new = rewritten(BALLYHOO)
        expected = copy(BALLYHOO)
        expected["nodes"]["drift"]["inputs"] = {"period": {"node": "speed"}, "scale": 0.02}
        expected["nodes"]["curve1"]["inputs"].update(low={"node": "high"}, high={"node": "low"})
        self.assertEqual(new, expected)
        self.assertEqual(list(new["nodes"]["curve1"]["inputs"]), ["x", "low", "high"])

    def test_a_curve_negates_its_bounds(self):
        changes = []
        new = m.rewrite(FAN, collections.Counter(), changes)
        self.assertEqual(new["nodes"]["curve1"]["inputs"], {"x": {"node": "place"}, "low": 25.0, "high": -25})
        self.assertEqual({c["input"] for c in changes}, {"curve1.low", "curve1.high"})

    def test_an_empty_high_becomes_minus_one_and_an_empty_low_stays(self):
        old = copy(FAN)
        old["nodes"]["curve1"]["inputs"] = {"x": {"node": "place"}}
        new = rewritten(old)["nodes"]["curve1"]["inputs"]
        self.assertEqual(new, {"x": {"node": "place"}, "high": -1})

    def test_a_number_yaw_is_negated_and_zero_stays_zero(self):
        for yaw, expected in [(20, -20), (-12.5, 12.5), (0, 0), (0.0, 0.0)]:
            old = {"version": 3, "nodes": {"aim1": {"kind": "aim", "inputs": {"yaw": yaw}}}}
            got = rewritten(old)["nodes"]["aim1"]["inputs"]["yaw"]
            self.assertEqual((got, json.dumps(got)), (expected, json.dumps(expected)))

    def test_max_becomes_min_and_a_product_negates_one_item(self):
        old = {"version": 3, "nodes": {
            "t": {"kind": "time"},
            "a": {"kind": "curve", "inputs": {"x": {"node": "t"}, "low": 0, "high": 30}},
            "b": {"kind": "value", "inputs": {"value": 10}},
            "m": {"kind": "math", "settings": {"op": "max"}, "inputs": {"values": [{"node": "a"}, {"node": "b"}]}},
            "p": {"kind": "math", "settings": {"op": "*"}, "inputs": {"values": [{"node": "m"}, 0.5]}},
            "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "p"}}}}}
        new = rewritten(old)["nodes"]
        self.assertEqual(new["p"]["inputs"]["values"], [{"node": "m"}, -0.5])
        # The product took the sign, so max and its items stay.
        self.assertEqual(new["m"], old["nodes"]["m"])
        old["nodes"]["p"]["inputs"]["values"] = [{"node": "m"}, {"node": "t2"}]
        old["nodes"]["t2"] = {"kind": "value", "inputs": {"value": 2}}
        new = rewritten(old)["nodes"]
        self.assertEqual(new["m"]["settings"], {"op": "min"})
        self.assertEqual(new["a"]["inputs"], {"x": {"node": "t"}, "low": 0, "high": -30})
        self.assertEqual(new["b"]["inputs"], {"value": -10})
        self.assertEqual(new["t2"], old["nodes"]["t2"])

    def test_a_curve_that_also_feeds_pitch_gets_a_product(self):
        old = copy(FAN)
        old["nodes"]["aim1"]["inputs"]["pitch"] = {"node": "curve1"}
        notes = collections.Counter()
        new = rewritten(old, notes)
        self.assertEqual(new["nodes"]["curve1"], old["nodes"]["curve1"])
        self.assertEqual(new["nodes"]["aim1"]["inputs"], {"yaw": {"node": "math1"}, "pitch": {"node": "curve1"}})
        self.assertEqual(new["nodes"]["math1"], {"kind": "math", "settings": {"op": "*"},
                                                 "inputs": {"values": [{"node": "curve1"}, -1]}})
        self.assertEqual(notes, {"shared: yaw through a new product with -1": 1})

    def test_a_bound_from_a_node_it_cannot_negate_gets_a_product(self):
        old = copy(FAN)
        old["nodes"]["curve1"]["inputs"]["high"] = {"node": "place"}
        notes = collections.Counter()
        new = rewritten(old, notes)
        self.assertEqual(new["nodes"]["aim1"]["inputs"]["yaw"], {"node": "math1"})
        self.assertEqual(new["nodes"]["curve1"], old["nodes"]["curve1"])
        self.assertEqual(notes, {"not negatable in place: yaw through a new product with -1": 1})

    def test_an_empty_audio_band_gets_the_old_40_to_100_hz(self):
        old = {"version": 3, "nodes": {
            "a": {"kind": "audio"},
            "b": {"kind": "audio", "inputs": {"high_hz": 250}},
            "c": {"kind": "audio", "inputs": {"low_hz": 60, "high_hz": 300}}}}
        notes = collections.Counter()
        new = rewritten(old, notes)["nodes"]
        self.assertEqual(new["a"]["inputs"], {"low_hz": 40, "high_hz": 100})
        self.assertEqual(list(new["b"]["inputs"].items()), [("high_hz", 250), ("low_hz", 40)])
        self.assertEqual(new["c"], old["nodes"]["c"])
        self.assertEqual(notes, {"empty audio low_hz set to 40": 2, "empty audio high_hz set to 100": 1})
        full = {"version": 3, "nodes": {"c": old["nodes"]["c"]}}
        self.assertIs(rewritten(full), full)

    def test_a_graph_with_no_yaw_and_no_noise_is_left_alone(self):
        old = {"version": 3, "nodes": {"color1": {"kind": "color"}}}
        self.assertIs(rewritten(old), old)

    def test_drafts_keep_their_key_order(self):
        document = {"clips": {"c1": {"start": 1, "graph": FAN, "name": "x"}}, "z": 1}
        new = m.rewrite_document(document, collections.Counter())
        self.assertEqual(list(new["clips"]["c1"]), ["start", "graph", "name"])
        self.assertEqual(new["clips"]["c1"]["graph"]["nodes"]["curve1"]["inputs"]["low"], 25.0)


class Run(unittest.TestCase):
    def test_writes_guarded_updates_and_leaves_the_database_alone(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "luma.db"
            db = sqlite3.connect(path)
            db.executescript("""
                CREATE TABLE scores (id TEXT, name TEXT);
                CREATE TABLE clips (id TEXT, score_id TEXT, name TEXT, start REAL, duration REAL,
                                    seed INTEGER, blend_mode TEXT, graph_json TEXT);
                CREATE TABLE drafts (id TEXT, base_json TEXT, state_json TEXT);""")
            db.execute("INSERT INTO scores VALUES ('s1', 'Score')")
            for clip_id, graph in [("c1", FAN), ("c2", {"version": 3, "nodes": {"color1": {"kind": "color"}}})]:
                db.execute("INSERT INTO clips VALUES (?, 's1', ?, 0, 4, 1, 'offset', ?)",
                           (clip_id, clip_id, json.dumps(graph)))
            db.execute("INSERT INTO drafts VALUES ('d1', ?, ?)",
                       (json.dumps({"clips": {"c1": {"graph": BALLYHOO}}}), json.dumps({})))
            db.commit()
            db.close()
            before = path.read_bytes()
            report = m.run(path, pathlib.Path(tmp) / "out")
            self.assertEqual(path.read_bytes(), before)
            self.assertEqual((report["changed"], report["drafts_changed"]), (1, 1))
            clips = (pathlib.Path(tmp) / "out" / "clips.sql").read_text().split("\n")
            self.assertEqual(clips[0], "-- expected: 1")
            self.assertIn("WHERE id = 'c1' AND graph_json = ", clips[1])
            self.assertIn('"low":25.0', clips[1])


if __name__ == "__main__":
    unittest.main()
