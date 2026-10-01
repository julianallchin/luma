#!/usr/bin/env python3
"""Tests for migrate_clip_clocks.py: one per band form, a version 1 graph in,
the version 2 graph out with no node added. Every converted graph then goes through the Rust
checker (patterns/examples/clip_graph_parity.rs --check) in one batch, and
the Chase pair is played on both sides when `LUMA_V1_PARITY` names a
clip_graph_parity binary built from a version 1 checkout.

    python3 backend/scripts/test_migrate_clip_clocks.py
"""
import json
import os
import pathlib
import subprocess
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_clocks as m  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
CHECKED = []  # (test name, version 2 clip)
ON = {"points": [[0, 1], [1, 1]]}
COMET = {"points": [[0, 0], [0.95, 1], [1, 0]]}


def w(node):
    return {"node": node}


def clip(graph):
    return {"name": "c", "start": 0, "duration": 16, "seed": 7, "blend_mode": "replace",
            "graph": graph}


def band(offset, width, shape=ON, kind="line", wrap="no", extra=None):
    """A version 1 graph: a band on `space1` read by `curve2` into brightness."""
    nodes = {
        "clock1": {"kind": "clock", "inputs": {"every": 2}},
        "time1": {"kind": "time", "inputs": {"clock": w("clock1")}},
        "space1": {"kind": "space", "settings": {"kind": kind, "wrap": wrap},
                   "inputs": {"offset": offset, "width": width}},
        "curve2": {"kind": "curve", "settings": {"kind": "number"},
                   "inputs": {"x": w("space1"), "shape": shape}},
        "color1": {"kind": "color", "inputs": {"brightness": w("curve2")}},
        **(extra or {}),
    }
    if not any(isinstance(v, dict) for v in (offset, width)):
        del nodes["clock1"], nodes["time1"]  # a still band reads no time
    return {"version": 1, "nodes": nodes}


def offset_curve(shape, low, high):
    return {"curve1": {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": w("time1"), "shape": {"points": shape},
                                  "low": low, "high": high}}}


class Shapes(unittest.TestCase):
    def test_outside_adds_jumps_to_0_only_where_the_shape_is_not_0(self):
        self.assertEqual(m.outside(ON["points"]), {"points": [[0, 0], [0, 1], [1, 1], [1, 0]]})
        self.assertEqual(m.outside(COMET["points"]), COMET)
        self.assertEqual(m.outside([[0, 0, "ease-in"], [1, 1]]),
                         {"points": [[0, 0, "ease-in"], [1, 1], [1, 0]]})

    def test_place_pads_with_jumps_and_keeps_eases(self):
        self.assertEqual(m.place(ON["points"], 0.25, 0.5),
                         {"points": [[0, 0], [0.25, 0], [0.25, 1], [0.5, 1], [0.5, 0], [1, 0]]})
        self.assertEqual(m.place([[0, 0, "ease-in"], [1, 1]], 0, 0.5),
                         {"points": [[0, 0, "ease-in"], [0.5, 1], [0.5, 0], [1, 0]]})


class Convert(unittest.TestCase):
    def convert(self, graph):
        new, notes = m.convert(graph)
        CHECKED.append((self.id(), clip(new)))
        for node in new["nodes"].values():
            self.assertFalse({"offset", "width"} & set(node.get("inputs", {})))
        self.assertEqual(len(new["nodes"]), len(graph["nodes"]), "no node added")
        return new["nodes"], notes

    def test_no_band_drops_the_inputs(self):
        nodes, notes = self.convert(band(0, 1))
        self.assertEqual(notes, {"no band"})
        self.assertNotIn("inputs", nodes["space1"])

    def test_a_still_band_becomes_a_region_of_the_shape(self):
        nodes, notes = self.convert(band(0, 0.5, kind="order"))
        self.assertEqual(notes, {"still band: a region of the curve"})
        self.assertNotIn("inputs", nodes["space1"])
        self.assertEqual(nodes["curve2"]["inputs"]["shape"],
                         {"points": [[0, 0], [0, 1], [0.5, 1], [0.5, 0], [1, 0]]})

    def test_a_chase_shifts_the_space_by_the_same_curve(self):
        graph = band(w("curve1"), 0.2, COMET, extra=offset_curve([[0, 0], [1, 1]], -0.2, 1))
        nodes, notes = self.convert(graph)
        self.assertEqual(notes, {"band: shift and length"})
        self.assertEqual(nodes["space1"]["inputs"], {"shift": w("curve1"), "length": 0.2})
        self.assertEqual(nodes["curve1"], graph["nodes"]["curve1"])
        self.assertEqual(nodes["curve2"]["inputs"]["shape"], COMET)

    def test_an_eased_sweep_keeps_its_offset_curve_and_ease(self):
        eased = [[0, 1, "ease-in"], [0.4, 0, "hold"], [1, 0]]
        graph = band(w("curve1"), 0.3, extra=offset_curve(eased, -0.3, 1))
        nodes, _ = self.convert(graph)
        self.assertEqual(nodes["curve1"]["inputs"]["shape"], {"points": eased})
        self.assertEqual(nodes["space1"]["inputs"]["shift"], w("curve1"))
        self.assertEqual(nodes["curve2"]["inputs"]["shape"],
                         {"points": [[0, 0], [0, 1], [1, 1], [1, 0]]})

    def test_a_growing_band_moves_its_length(self):
        extra = {"curve1": {"kind": "curve", "settings": {"kind": "number"},
                            "inputs": {"x": w("time1"), "shape": {"points": [[0, 0], [1, 1]]}}}}
        nodes, _ = self.convert(band(0, w("curve1"), extra=extra))
        self.assertEqual(nodes["space1"]["inputs"], {"length": w("curve1")})

    def test_a_ring_keeps_its_wrap(self):
        graph = band(w("curve1"), 0.25, COMET, kind="angle", wrap="yes",
                     extra=offset_curve([[0, 0], [1, 1]], 0, 1))
        nodes, _ = self.convert(graph)
        self.assertEqual(nodes["space1"]["settings"]["wrap"], "yes")
        self.assertEqual(nodes["space1"]["inputs"], {"shift": w("curve1"), "length": 0.25})

    def test_a_color_curve_over_a_band_is_refused(self):
        graph = band(0.2, 0.5)
        graph["nodes"]["curve2"]["settings"]["kind"] = "color"
        with self.assertRaises(m.Refused):
            m.convert(graph)

    def test_version_2_passes_and_a_document_converts_every_graph(self):
        new, _ = m.convert(band(0, 1))
        self.assertEqual(m.convert(new)[0], new)
        doc = {"clips": {"a": clip(band(0, 0.5))}}
        self.assertEqual(m.convert_document(doc)["clips"]["a"]["graph"]["version"], 2)


def checker():
    return ["cargo", "+1.97.1", "run", "--quiet", "--release", "--manifest-path",
            str(REPO / "backend/Cargo.toml"), "-p", "luma-patterns", "--example",
            "clip_graph_parity", "--"]


class Checked(unittest.TestCase):
    def test_every_converted_graph_passes_the_checker(self):
        self.assertTrue(CHECKED, "run the Convert tests first")
        process = subprocess.run(checker() + ["--check"],
                                 input="".join(json.dumps(c) + "\n" for _, c in CHECKED),
                                 text=True, capture_output=True, check=True)
        lines = process.stdout.splitlines()
        self.assertEqual(len(lines), len(CHECKED))
        failures = [f"{name}: {json.loads(line)['error']}"
                    for (name, _), line in zip(CHECKED, lines) if "error" in json.loads(line)]
        self.assertEqual(failures, [])

    @unittest.skipUnless(os.environ.get("LUMA_V1_PARITY"), "needs a version 1 clip_graph_parity")
    def test_converted_bands_play_as_before(self):
        cases = [band(w("curve1"), 0.2, COMET, extra=offset_curve([[0, 0], [1, 1]], -0.2, 1)),
                 band(w("curve1"), 0.3, extra=offset_curve([[0, 1, "ease-in"], [1, 0]], -0.3, 1)),
                 band(0.2, 0.5, kind="order"),
                 band(w("curve1"), 0.25, COMET, kind="angle", wrap="yes",
                      extra=offset_curve([[0, 0], [1, 1]], 0, 1))]
        for graph in cases:
            old, new = clip(graph), clip(m.convert(graph)[0])
            request = lambda c: dict(clip=c, cells=m.CELLS, beats=m.beats(c))  # noqa: E731
            a = m.evaluate(os.environ["LUMA_V1_PARITY"], [request(old)])[0]
            b = subprocess.run(checker(), input=json.dumps(request(new)) + "\n", text=True,
                               capture_output=True, check=True).stdout
            kind, worst = m.compare(a["ok"], json.loads(b)["ok"])
            self.assertEqual(kind, "exact", worst)


if __name__ == "__main__":
    suite = unittest.TestSuite()
    loader = unittest.TestLoader()
    for case in (Shapes, Convert, Checked):
        suite.addTests(loader.loadTestsFromTestCase(case))
    sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
