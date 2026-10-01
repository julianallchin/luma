#!/usr/bin/env python3
"""Tests for migrate_clip_front.py: a graph in, the exact graph out. With
`LUMA_V3_PARITY` naming a version 3 clip_graph_parity binary, every
rewritten graph passes the checker and plays the same as before on both
stand-in rigs (every number within EXACT).

    python3 backend/scripts/test_migrate_clip_front.py
"""
import os
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_front as m  # noqa: E402

PLAYED = []  # (test name, old clip, new clip)
SWEEP = {"points": [[0.0, 0.0], [0.2, 1.0, "hold"], [1.0, 1.0]]}
PILL = {"points": [[0, 0], [0, 1], [1, 1], [1, 0]]}
STEP_UP = {"points": [[0, 0], [0, 1], [1, 1]]}
FADE = {"points": [[0.0, 1.0, "hold"], [0.2, 1.0, "sine-out"], [1.0, 0.0]]}


def w(node):
    return {"node": node}


def curve(x, shape, **inputs):
    return {"kind": "curve", "settings": {"kind": "number"},
            "inputs": {"x": w(x), "shape": shape, **inputs}}


def space(direction, shift, scale=None):
    inputs = {"direction": direction, "shift": shift}
    if scale is not None:
        inputs["scale"] = scale
    return {"kind": "space", "settings": {"kind": "line", "wrap": "no"}, "inputs": inputs}


def graph(**nodes):
    return {"version": 3, "nodes": nodes}


def clip(g):
    return {"name": "c", "start": 0, "duration": 8, "seed": 7, "blend_mode": "replace", "graph": g}


TIME = {"kind": "time", "inputs": {"every": 2.0, "duration": 2.0}}


def times(*items):
    return {"kind": "math", "settings": {"op": "*"}, "inputs": {"values": list(items)}}


def color(brightness):
    return {"kind": "color", "inputs": {"color": [1, 0.5, 0.25], "brightness": brightness}}


def slash(direction):
    return graph(time1=TIME,
                 curve1=curve("time1", SWEEP, low=-2.0, high=-0.97),
                 curve4=curve("time1", FADE),
                 space1=space(direction, w("curve1"), 2.000000002),
                 curve5=curve("space1", PILL),
                 math1=times(w("curve4"), w("curve5")),
                 color1=color(w("math1")))


class Rewrite(unittest.TestCase):
    def rewrite(self, old, expected):
        new, notes = m.rewrite(old)
        self.assertEqual(new, expected)
        again, more = m.rewrite(new)
        self.assertEqual(again, new, "idempotent")
        self.assertEqual(sum(more.values()), 0)
        PLAYED.append((self.id(), clip(old), clip(new)))
        return notes

    def test_slash_becomes_a_front(self):
        notes = self.rewrite(slash([-0.8192, 0.0, 0.5736]), graph(
            time1=TIME,
            curve1=curve("time1", SWEEP, low=1.0, high=-0.03),
            curve4=curve("time1", FADE),
            space1=space([0.8192, 0, -0.5736], w("curve1")),
            curve5=curve("space1", STEP_UP),
            math1=times(w("curve4"), w("curve5")),
            color1=color(w("math1"))))
        self.assertEqual(notes["fronts rewritten"], 1)
        self.assertEqual(notes["front 0 → 1.03"], 1)

    def test_backslash_becomes_a_front(self):
        self.rewrite(slash([0.8192, 0.0, 0.5736]), graph(
            time1=TIME,
            curve1=curve("time1", SWEEP, low=1.0, high=-0.03),
            curve4=curve("time1", FADE),
            space1=space([-0.8192, 0, -0.5736], w("curve1")),
            curve5=curve("space1", STEP_UP),
            math1=times(w("curve4"), w("curve5")),
            color1=color(w("math1"))))

    def test_a_window_whose_back_enters_the_rig_is_left_alone(self):
        # Scale 2, shift −1 → 0.5: the window's back edge sweeps the rig too.
        old = graph(time1=TIME,
                    curve1=curve("time1", SWEEP, low=-1.0, high=0.5),
                    space1=space([1, 0, 0], w("curve1"), 2.0),
                    curve5=curve("space1", PILL),
                    color1=color(w("curve5")))
        new, notes = m.rewrite(old)
        self.assertEqual(new, old)
        self.assertEqual(notes["left alone: window's back edge enters the rig"], 1)

    def test_a_front_that_holds_on_the_rig_is_left_alone(self):
        # The front stops at 0.5: a head there is lit before (a < F) and
        # not after (a ≤ F) only on a tie, so it is not provable.
        old = graph(time1=TIME,
                    curve1=curve("time1", SWEEP, low=-2.0, high=-1.5),
                    space1=space([1, 0, 0], w("curve1"), 2.0),
                    curve5=curve("space1", PILL),
                    color1=color(w("curve5")))
        new, notes = m.rewrite(old)
        self.assertEqual(new, old)
        self.assertEqual(sum(v for k, v in notes.items() if k.startswith("left alone")), 1)

    def test_a_shared_shift_curve_is_left_alone(self):
        old = slash([-0.8192, 0.0, 0.5736])
        old["nodes"]["curve6"] = curve("curve1", PILL)
        new, notes = m.rewrite(old)
        self.assertEqual(new, old)
        self.assertEqual(notes["left alone: shift curve read elsewhere"], 1)

    def test_drafts_rewrite_every_version_3_graph(self):
        old = slash([-0.8192, 0.0, 0.5736])
        document = {"tracks": [{"clips": [clip(old), {"graph": {"version": 2, "nodes": {}}}]}]}
        notes = m.collections.Counter()
        new = m.rewrite_document(document, notes)
        self.assertEqual(new["tracks"][0]["clips"][0]["graph"], m.rewrite(old)[0])
        self.assertEqual(new["tracks"][0]["clips"][1], document["tracks"][0]["clips"][1])
        self.assertEqual(notes["fronts rewritten"], 1)


class Played(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY"), "needs a version 3 clip_graph_parity")
    def test_rewritten_graphs_pass_the_checker_and_play_the_same(self):
        self.assertTrue(PLAYED, "run the Rewrite tests first")
        binary = os.environ["LUMA_V3_PARITY"]
        checks = m.v3.v2.evaluate(binary, [b for _, _, b in PLAYED], "--check")
        self.assertEqual([f"{n}: {c['error']}" for (n, _, _), c in zip(PLAYED, checks)
                          if "error" in c], [])
        for rig in m.v3.RIGS.values():
            olds = m.v3.play(binary, [a for _, a, _ in PLAYED], rig)
            news = m.v3.play(binary, [b for _, _, b in PLAYED], rig)
            for (name, _, _), x, y in zip(PLAYED, olds, news):
                with self.subTest(name):
                    self.assertIn("ok", x)
                    lit = m.v3.lights(x["ok"])
                    self.assertTrue(any(lit), "something lit")
                    self.assertLessEqual(m.v3.difference(x["ok"], y["ok"])[0], m.EXACT)


if __name__ == "__main__":
    suite = unittest.TestSuite()
    loader = unittest.TestLoader()
    for case in (Rewrite, Played):
        suite.addTests(loader.loadTestsFromTestCase(case))
    sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
