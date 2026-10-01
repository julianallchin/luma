#!/usr/bin/env python3
"""Tests for migrate_clip_math.py: a graph in, the exact graph out. With
`LUMA_V3_PARITY` naming a version 3 clip_graph_parity binary, every
rewritten graph passes the checker and plays bit for bit as before on both
stand-in rigs.

    python3 backend/scripts/test_migrate_clip_math.py
"""
import os
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_math as m  # noqa: E402

PLAYED = []  # (test name, old clip, new clip)
RAMP = {"points": [[0, 0], [1, 1]]}
PILL = {"points": [[0, 0], [0, 1], [1, 1], [1, 0]]}
FADE = {"points": [[0.0, 1.0, "hold"], [0.2, 1.0, "sine-out"], [1.0, 0.0]]}


def w(node):
    return {"node": node}


def curve(x, shape=RAMP, kind="number", **inputs):
    return {"kind": "curve", "settings": {"kind": kind},
            "inputs": {"x": w(x), "shape": shape, **inputs}}


def graph(**nodes):
    return {"version": 3, "nodes": nodes}


def clip(g):
    return {"name": "c", "start": 0, "duration": 8, "seed": 7, "blend_mode": "replace", "graph": g}


TIME = {"kind": "time", "inputs": {"every": 2.0}}
LINE = {"kind": "space", "settings": {"kind": "line", "wrap": "no"}}
DIAGONAL = {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
            "inputs": {"direction": [0.5736, 0.0, 0.8192], "scale": 0.6}}


def times(*items):
    return {"kind": "math", "settings": {"op": "*"}, "inputs": {"values": list(items)}}


def color(brightness):
    return {"kind": "color", "inputs": {"color": [1, 0.5, 0.25], "brightness": brightness}}


class Rewrite(unittest.TestCase):
    def rewrite(self, old, expected):
        new, notes = m.rewrite(old)
        self.assertEqual(new, expected)
        again, more = m.rewrite(new)
        self.assertEqual(again, new, "idempotent")
        self.assertEqual(sum(more.values()), 0)
        PLAYED.append((self.id(), clip(old), clip(new)))
        return notes

    def test_slash_becomes_one_product_of_three(self):
        old = graph(time1=TIME, space1=LINE, space2=DIAGONAL,
                    curve4=curve("time1", FADE),
                    curve5=curve("space1", PILL, low=0.0, high=w("curve4")),
                    curve6=curve("space2", {"points": [[0, 0], [0.12, 1], [0.88, 1], [1, 0]]},
                                 low=0.0, high=w("curve5")),
                    color1=color(w("curve6")))
        notes = self.rewrite(old, graph(
            time1=TIME, space1=LINE, space2=DIAGONAL,
            curve4=curve("time1", FADE),
            curve5=curve("space1", PILL),
            curve6=curve("space2", {"points": [[0, 0], [0.12, 1], [0.88, 1], [1, 0]]}),
            color1=color(w("math1")),
            math1=times(w("curve4"), w("curve5"), w("curve6"))))
        self.assertEqual(notes["product of 3"], 1)
        self.assertEqual(notes["curves rewritten"], 2)

    def test_a_chain_of_two_with_empty_low(self):
        old = graph(time1=TIME, space1=LINE,
                    curve1=curve("time1", {"points": [[0, 1], [1, 0]]}, low=0.007),
                    curve2=curve("space1", PILL, high=w("curve1")),
                    color1=color(w("curve2")))
        self.rewrite(old, graph(
            time1=TIME, space1=LINE,
            curve1=curve("time1", {"points": [[0, 1], [1, 0]]}, low=0.007),
            curve2=curve("space1", PILL),
            color1=color(w("math1")),
            math1=times(w("curve1"), w("curve2"))))

    def test_a_non_zero_low_is_not_rewritten(self):
        old = graph(time1=TIME, space1=LINE,
                    curve1=curve("time1"),
                    curve2=curve("space1", PILL, low=0.2, high=w("curve1")),
                    curve3=curve("space1", PILL, low=w("curve1"), high=w("curve1")),
                    curve4=curve("time1", kind="vector", low=[0, 0, 0], high=w("curve5")),
                    curve5=curve("time1", kind="vector", low=[0, 0, 0], high=[1, 1, 1]),
                    color1=color(w("curve2")))
        new, notes = m.rewrite(old)
        self.assertEqual(new, old)
        self.assertEqual(sum(notes.values()), 0)

    def test_a_chain_flattens_with_a_math_node_on_its_high(self):
        # curve3.high = math(curve1, curve2): one product, the math node's id.
        old = graph(time1=TIME, space1=LINE,
                    curve1=curve("time1"), curve2=curve("space1"),
                    product=times(w("curve1"), w("curve2"), 0.5),
                    curve3=curve("space1", PILL, high=w("product")),
                    curve4=curve("time1", PILL, low=0, high=w("curve3")),
                    color1=color(w("curve4")))
        notes = self.rewrite(old, graph(
            time1=TIME, space1=LINE,
            curve1=curve("time1"), curve2=curve("space1"),
            curve3=curve("space1", PILL),
            curve4=curve("time1", PILL),
            color1=color(w("product")),
            product=times(w("curve1"), w("curve2"), 0.5, w("curve3"), w("curve4"))))
        self.assertEqual(notes["nodes removed"], 1)
        self.assertEqual(notes["product of 5"], 1)

    def test_a_product_reader_takes_the_items_first(self):
        old = graph(time1=TIME, space1=LINE,
                    curve1=curve("time1"), curve2=curve("space1", PILL, high=w("curve1")),
                    curve3=curve("space1"),
                    math1=times(w("curve3"), w("curve2")),
                    color1=color(w("math1")))
        self.rewrite(old, graph(
            time1=TIME, space1=LINE,
            curve1=curve("time1"), curve2=curve("space1", PILL), curve3=curve("space1"),
            math1=times(w("curve1"), w("curve2"), w("curve3")),
            color1=color(w("math1"))))

    def test_a_shared_link_stays_a_factor(self):
        # curve2 is read twice: it becomes its own product, read by both.
        old = graph(time1=TIME, space1=LINE,
                    curve1=curve("time1"),
                    curve2=curve("space1", PILL, high=w("curve1")),
                    curve3=curve("time1", PILL, high=w("curve2")),
                    space2={"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                            "inputs": {"shift": w("curve2")}},
                    curve4=curve("space2", PILL, high=w("curve3")),
                    color1=color(w("curve4")))
        self.rewrite(old, graph(
            time1=TIME, space1=LINE,
            curve1=curve("time1"),
            curve2=curve("space1", PILL),
            curve3=curve("time1", PILL),
            space2={"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                    "inputs": {"shift": w("math1")}},
            curve4=curve("space2", PILL),
            color1=color(w("math2")),
            math1=times(w("curve1"), w("curve2")),
            math2=times(w("math1"), w("curve3"), w("curve4"))))

    def test_drafts_rewrite_every_version_3_graph(self):
        old = graph(time1=TIME, space1=LINE, curve1=curve("time1"),
                    curve2=curve("space1", PILL, high=w("curve1")), color1=color(w("curve2")))
        document = {"tracks": [{"clips": [clip(old), {"graph": {"version": 2, "nodes": {}}}]}]}
        notes = m.collections.Counter()
        new = m.rewrite_document(document, notes)
        self.assertEqual(new["tracks"][0]["clips"][0]["graph"], m.rewrite(old)[0])
        self.assertEqual(new["tracks"][0]["clips"][1], document["tracks"][0]["clips"][1])
        self.assertEqual(notes["curves rewritten"], 1)


class Played(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY"), "needs a version 3 clip_graph_parity")
    def test_rewritten_graphs_pass_the_checker_and_play_bit_for_bit(self):
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
                    self.assertEqual(lit, m.v3.lights(y["ok"]))


if __name__ == "__main__":
    suite = unittest.TestSuite()
    loader = unittest.TestLoader()
    for case in (Rewrite, Played):
        suite.addTests(loader.loadTestsFromTestCase(case))
    sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
