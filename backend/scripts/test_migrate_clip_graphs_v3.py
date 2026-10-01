#!/usr/bin/env python3
"""Tests for migrate_clip_graphs_v3.py: one per mapping rule, a version 1 or
2 graph in, the exact version 3 graph out. With `LUMA_V3_PARITY` naming a
version 3 clip_graph_parity binary, every converted graph also goes through
the checker; with `LUMA_V1_PARITY` as well, the version 1 cases play the
same before and after on both stand-in rigs.

    python3 backend/scripts/test_migrate_clip_graphs_v3.py
"""
import json
import os
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_graphs_v3 as m  # noqa: E402

CHECKED = []  # (test name, version 3 clip)
PLAYED = []  # (test name, version 1 clip, version 3 clip)
RAMP = {"points": [[0, 0], [1, 1]]}
DURATION = 16


def w(node):
    return {"node": node}


def clip(graph):
    return {"name": "c", "start": 0, "duration": DURATION, "seed": 7,
            "blend_mode": "replace", "graph": graph}


def curve(x, shape=RAMP, kind="number", **inputs):
    return {"kind": "curve", "settings": {"kind": kind},
            "inputs": {"x": w(x), "shape": shape, **inputs}}


def color(**inputs):
    return {"kind": "color", "inputs": {"color": [1, 0.5, 0.25], **inputs}}


def graph(version, **nodes):
    return {"version": version, "nodes": nodes}


class Convert(unittest.TestCase):
    def convert(self, old, expected, notes=None, **kw):
        new, why = m.convert(old, DURATION, **kw)
        self.assertEqual(new, graph(3, **expected))
        if notes is not None:
            self.assertLessEqual(notes, why)
        CHECKED.append((self.id(), clip(new)))
        if old["version"] == 1:
            PLAYED.append((self.id(), clip(old), clip(new)))
        return why

    # ---- clocks ----

    def test_a_shared_clock_becomes_equal_time_inputs(self):
        self.convert(
            graph(1, clock1={"kind": "clock", "inputs": {"every": 2, "duration": 1}},
                  time1={"kind": "time", "inputs": {"clock": w("clock1")}},
                  time2={"kind": "time", "inputs": {"clock": w("clock1"), "phase": w("curve3")}},
                  space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"}},
                  curve3=curve("space1"),
                  curve1=curve("time1"), curve2=curve("time2", kind="color",
                                                      gradient={"stops": [
                                                          {"t": 0, "color": [1, 0, 0]},
                                                          {"t": 1, "color": [0, 0, 1]}]}),
                  color1=color(color=w("curve2"), brightness=w("curve1"))),
            dict(time1={"kind": "time", "inputs": {"every": 2, "duration": 1}},
                 time2={"kind": "time", "inputs": {"every": 2, "duration": 1,
                                                   "phase": w("curve3")}},
                 space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"}},
                 curve3=curve("space1"),
                 curve1=curve("time1"), curve2=curve("time2", kind="color",
                                                     gradient={"stops": [
                                                         {"t": 0, "color": [1, 0, 0]},
                                                         {"t": 1, "color": [0, 0, 1]}]}),
                 color1=color(color=w("curve2"), brightness=w("curve1"))),
            {"clock: into time inputs"})

    def test_a_wire_into_a_clock_becomes_the_same_wire_into_the_time(self):
        self.convert(
            graph(1, time0={"kind": "time"},
                  curve0=curve("time0", low=4, high=1),
                  clock1={"kind": "clock", "inputs": {"every": w("curve0")}},
                  time1={"kind": "time", "inputs": {"clock": w("clock1")}},
                  curve1=curve("time1", {"points": [[0, 1], [1, 0]]}),
                  color1=color(brightness=w("curve1"))),
            dict(time0={"kind": "time"},
                 curve0=curve("time0", low=4, high=1),
                 time1={"kind": "time", "inputs": {"every": w("curve0")}},
                 curve1=curve("time1", {"points": [[0, 1], [1, 0]]}),
                 color1=color(brightness=w("curve1"))))

    def test_a_shuffle_reads_an_existing_time_of_its_clock(self):
        self.convert(
            graph(1, clock1={"kind": "clock", "inputs": {"every": 1}},
                  time1={"kind": "time", "inputs": {"clock": w("clock1")}},
                  shuffle1={"kind": "shuffle", "inputs": {"clock": w("clock1")}},
                  space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                          "inputs": {"heads": w("shuffle1")}},
                  curve1=curve("space1", {"points": [[0, 1], [0.25, 1], [0.3, 0], [1, 0]]}),
                  curve2=curve("time1", {"points": [[0, 1], [1, 0]]}),
                  color1=color(brightness=w("curve1"), alpha=w("curve2"))),
            dict(time1={"kind": "time", "inputs": {"every": 1}},
                 shuffle1={"kind": "shuffle", "inputs": {"time": w("time1")}},
                 space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                         "inputs": {"heads": w("shuffle1")}},
                 curve1=curve("space1", {"points": [[0, 1], [0.25, 1], [0.3, 0], [1, 0]]}),
                 curve2=curve("time1", {"points": [[0, 1], [1, 0]]}),
                 color1=color(brightness=w("math1")),
                 math1={"kind": "math", "settings": {"op": "*"},
                        "inputs": {"values": [w("curve1"), w("curve2")]}}),
            {"shuffle: an existing time", "math node added"})

    def test_a_shuffle_gets_a_new_time_when_every_time_of_its_clock_has_a_phase(self):
        self.convert(
            graph(1, clock1={"kind": "clock", "inputs": {"every": 0.5}},
                  time1={"kind": "time", "inputs": {"clock": w("clock1"), "phase": w("curve3")}},
                  shuffle1={"kind": "shuffle", "inputs": {"clock": w("clock1")}},
                  space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                          "inputs": {"heads": w("shuffle1")}},
                  curve3=curve("space1", low=0, high=0.5),
                  curve1=curve("time1", {"points": [[0, 1], [1, 0]]}),
                  color1=color(brightness=w("curve1"))),
            dict(time1={"kind": "time", "inputs": {"every": 0.5, "phase": w("curve3")}},
                 shuffle1={"kind": "shuffle", "inputs": {"time": w("time2")}},
                 space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                         "inputs": {"heads": w("shuffle1")}},
                 curve3=curve("space1", low=0, high=0.5),
                 curve1=curve("time1", {"points": [[0, 1], [1, 0]]}),
                 color1=color(brightness=w("curve1")),
                 time2={"kind": "time", "inputs": {"every": 0.5}}),
            {"shuffle: a new time"})

    # ---- delay and length ----

    def delay(self, clock_inputs, delay, expected_delay, extra=None, expected_extra=None):
        nodes = dict(time1={"kind": "time", "inputs": {"delay": delay}},
                     curve1=curve("time1", {"points": [[0, 0], [0, 1], [1, 0]]}),
                     color1=color(brightness=w("curve1")), **(extra or {}))
        expected = dict(nodes, time1={"kind": "time", "inputs": {"delay": expected_delay}},
                        **(expected_extra or {}))
        if clock_inputs is not None:
            nodes["clock1"] = {"kind": "clock", "inputs": clock_inputs}
            nodes["time1"]["inputs"]["clock"] = w("clock1")
            expected["time1"]["inputs"] = {**clock_inputs, "delay": expected_delay}
        return graph(2, **nodes), expected

    def test_a_delay_in_turns_becomes_beats_of_the_clock_duration(self):
        self.convert(*self.delay({"every": 4, "duration": 2}, 0.25, 0.5), {"delay: turns to beats"})

    def test_a_delay_with_no_clock_duration_takes_every(self):
        self.convert(*self.delay({"every": 4}, 0.25, 1))

    def test_a_delay_with_no_clock_takes_the_clip_duration(self):
        self.convert(*self.delay(None, 0.25, 4))

    def test_a_delay_curve_with_one_reader_scales_its_low_and_high(self):
        space = {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                 "inputs": {"direction": [1, 0, 0]}}
        old, expected = self.delay({"every": 4}, w("curve2"), w("curve2"),
                                   {"space1": space, "curve2": curve("space1", low=0, high=0.5)},
                                   {"curve2": curve("space1", low=0, high=2)})
        self.convert(old, expected, {"curve low/high scaled"})

    def test_a_delay_times_a_wired_duration_is_a_math_node(self):
        old, expected = self.delay(
            {"every": 4, "duration": w("curve0")}, 0.25, w("math1"),
            {"time0": {"kind": "time"}, "curve0": curve("time0", low=1, high=2)},
            {"time0": {"kind": "time"}, "curve0": curve("time0", low=1, high=2),
             "math1": {"kind": "math", "settings": {"op": "*"},
                       "inputs": {"values": [0.25, w("curve0")]}}})
        self.convert(old, expected, {"math node added"})

    def test_a_time_length_other_than_1_is_refused_and_1_is_dropped(self):
        old, expected = self.delay(None, 0, 0)
        old["nodes"]["time1"]["inputs"]["length"] = 1
        self.convert(old, expected)
        old["nodes"]["time1"]["inputs"]["length"] = 0.5
        with self.assertRaisesRegex(m.Refused, "time length"):
            m.convert(old, DURATION)

    # ---- space and mirror ----

    def test_space_length_becomes_scale(self):
        self.convert(
            graph(2, space1={"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                             "inputs": {"direction": [0, 0, 1], "shift": 0.2, "length": 0.3}},
                  curve1=curve("space1"), color1=color(brightness=w("curve1"))),
            dict(space1={"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                         "inputs": {"direction": [0, 0, 1], "shift": 0.2, "scale": 0.3}},
                 curve1=curve("space1"), color1=color(brightness=w("curve1"))))

    def test_a_mirror_offset_other_than_0_is_refused_and_0_is_dropped(self):
        old = graph(2, mirror1={"kind": "mirror", "inputs": {"normal": [0, 0, 1], "offset": 0}},
                    space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                            "inputs": {"heads": w("mirror1")}},
                    curve1=curve("space1"), color1=color(brightness=w("curve1")))
        self.convert(old, dict(old["nodes"], mirror1={"kind": "mirror",
                                                      "inputs": {"normal": [0, 0, 1]}}))
        old["nodes"]["mirror1"]["inputs"]["offset"] = 0.5
        with self.assertRaisesRegex(m.Refused, "mirror offset"):
            m.convert(old, DURATION)

    def mirrored(self, normal, direction, space_inputs, extra=None):
        mirror = {"kind": "mirror", "inputs": {"normal": normal} if normal else {}}
        if not mirror["inputs"]:
            del mirror["inputs"]
        inputs = {"heads": w("mirror1"), **space_inputs}
        if direction:
            inputs["direction"] = direction
        return dict(mirror1=mirror,
                    space1={"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                            "inputs": inputs},
                    curve1=curve("space1", {"points": [[0, 0], [0, 1], [1, 1], [1, 0]]}),
                    color1=color(brightness=w("curve1")), **(extra or {}))

    def test_a_line_after_a_parallel_mirror_halves_shift_and_scale(self):
        why = self.convert(graph(2, **self.mirrored([1, 0, 0], [-1, 0, 0],
                                                    {"shift": 0.5, "length": 0.25})),
                           self.mirrored([1, 0, 0], [-1, 0, 0], {"shift": 0.25, "scale": 0.125}))
        self.assertIn("mirrored line: halved (parallel)", why)

    def test_a_mirrored_line_with_no_scale_gets_scale_one_half(self):
        self.convert(graph(2, **self.mirrored([0, 0, 1], [0, 0, 2], {})),
                     self.mirrored([0, 0, 1], [0, 0, 2], {"scale": 0.5}))

    def test_a_mirrored_line_halves_a_wired_shift_by_its_curve(self):
        """Version 1: a chase band over a mirror (8.7 gives shift and length)."""
        extra = {"clock1": {"kind": "clock", "inputs": {"every": 4, "duration": 2}},
                 "time1": {"kind": "time", "inputs": {"clock": w("clock1")}},
                 "curve2": curve("time1", low=-0.5, high=1)}
        old = graph(1, **self.mirrored([1, 0, 0], [1, 0, 0],
                                       {"offset": w("curve2"), "width": 0.5}, extra))
        old["nodes"]["curve1"]["inputs"]["shape"] = {"points": [[0, 0], [0.5, 1], [1, 0]]}
        expected = self.mirrored(
            [1, 0, 0], [1, 0, 0], {"shift": w("curve2"), "scale": 0.25},
            {"time1": {"kind": "time", "inputs": {"every": 4, "duration": 2}},
             "curve2": curve("time1", low=-0.25, high=0.5)})
        expected["curve1"]["inputs"]["shape"] = {"points": [[0, 0], [0.5, 1], [1, 0]]}
        self.convert(old, expected)

    def test_a_mirrored_line_halves_a_shared_shift_with_a_math_node(self):
        extra = {"time1": {"kind": "time"}, "curve2": curve("time1"),
                 "curve3": curve("time1", {"points": [[0, 0], [1, 1]]})}
        old = self.mirrored([1, 0, 0], [1, 0, 0], {"shift": w("curve2")}, extra)
        old["color1"]["inputs"]["brightness"] = w("curve2")
        old["curve3"] = curve("space1")
        old["color1"]["inputs"]["alpha"] = w("curve3")
        new, _ = m.convert(graph(2, **old), DURATION)
        nodes = new["nodes"]
        self.assertEqual(nodes["space1"]["inputs"]["shift"], w("math1"))
        self.assertEqual(nodes["math1"]["inputs"]["values"], [w("curve2"), 0.5])
        self.assertEqual(nodes["curve2"], curve("time1"))

    def test_a_line_across_the_mirror_normal_is_unchanged(self):
        self.convert(graph(2, **self.mirrored([0, 0, 1], [1, 0, 0], {"shift": 0.5})),
                     self.mirrored([0, 0, 1], [1, 0, 0], {"shift": 0.5}))

    def test_a_best_fit_normal_halves_or_keeps_by_choice(self):
        old = graph(2, **self.mirrored(None, [1, 0, 0], {}))
        why = self.convert(old, self.mirrored(None, [1, 0, 0], {"scale": 0.5}), best_fit="halve")
        self.assertIn("mirrored line: halved (best fit)", why)
        self.convert(old, self.mirrored(None, [1, 0, 0], {}), best_fit="keep")

    # ---- lists ----

    def test_a_list_becomes_one_math_node_and_a_list_of_numbers_its_product(self):
        self.convert(
            graph(2, time1={"kind": "time"}, curve1=curve("time1"),
                  space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"}},
                  curve2=curve("space1"),
                  strobe1={"kind": "strobe", "inputs": {"rate": [w("curve1"), 0.5, w("curve2")],
                                                        "alpha": [0.5, 0.5]}}),
            dict(time1={"kind": "time"}, curve1=curve("time1"),
                 space1={"kind": "space", "settings": {"kind": "order", "wrap": "no"}},
                 curve2=curve("space1"),
                 strobe1={"kind": "strobe", "inputs": {"rate": w("math1")}},
                 math1={"kind": "math", "settings": {"op": "*"},
                        "inputs": {"values": [w("curve1"), 0.5, w("curve2"), 0.25]}}))

    # ---- alpha ----

    def alpha(self, old, expected, notes=None, version=1):
        base = dict(time1={"kind": "time"}, time2={"kind": "time"}, curve1=curve("time1"),
                    curve2=curve("time2", {"points": [[0, 1], [1, 0]]}))
        old = {**base, **old}
        expected = {**base, **expected}
        for nodes in (old, expected):  # drop the base nodes nothing reads
            for k in ("curve1", "curve2", "time1", "time2"):
                if not any(json.dumps(w(k)) in json.dumps(n.get("inputs", {}))
                           for n in nodes.values()):
                    del nodes[k]
        return self.convert(graph(version, **old), expected, notes)

    def test_alpha_number_times_brightness_number(self):
        self.alpha({"color1": color(brightness=0.5, alpha=0.5)},
                   {"color1": color(brightness=0.25)})

    def test_alpha_number_with_empty_brightness_becomes_brightness(self):
        self.alpha({"color1": color(alpha=0.4)}, {"color1": color(brightness=0.4)})

    def test_alpha_1_is_dropped(self):
        self.alpha({"color1": color(brightness=w("curve1"), alpha=1)},
                   {"color1": color(brightness=w("curve1"))})

    def test_alpha_number_scales_a_bare_brightness_curve(self):
        self.alpha({"color1": color(brightness=w("curve1"), alpha=0.5)},
                   {"color1": color(brightness=w("curve1")),
                    "curve1": curve("time1", high=0.5)},
                   {"curve low/high scaled"})

    def test_alpha_number_scales_a_brightness_curve_low_and_high(self):
        self.alpha({"color1": color(brightness=w("curve1"), alpha=0.5),
                    "curve1": curve("time1", low=0.2, high=0.8)},
                   {"color1": color(brightness=w("curve1")),
                    "curve1": curve("time1", low=0.1, high=0.4)})

    def test_alpha_number_with_a_shared_brightness_curve_adds_a_math_node(self):
        hue = curve("space1", kind="color", gradient={"stops": [
            {"t": 0, "color": [1, 0, 0]}, {"t": 1, "color": [0, 0, 1]}]})
        space = {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                 "inputs": {"direction": [1, 0, 0], "shift": w("curve1"), "length": 0.5}}
        self.alpha({"color1": color(color=w("curve3"), brightness=w("curve1"), alpha=0.5),
                    "space1": space, "curve3": hue},
                   {"color1": color(color=w("curve3"), brightness=w("math1")),
                    "space1": {**space, "inputs": {"direction": [1, 0, 0], "shift": w("curve1"),
                                                   "scale": 0.5}},
                    "curve3": hue,
                    "math1": {"kind": "math", "settings": {"op": "*"},
                              "inputs": {"values": [w("curve1"), 0.5]}}},
                   {"math node added"}, version=2)

    def test_alpha_wire_with_empty_brightness_becomes_brightness(self):
        self.alpha({"color1": color(alpha=w("curve2"))},
                   {"color1": color(brightness=w("curve2"))})

    def test_alpha_wire_times_a_brightness_number_scales_the_alpha_curve(self):
        self.alpha({"color1": color(brightness=0.5, alpha=w("curve2"))},
                   {"color1": color(brightness=w("curve2")),
                    "curve2": curve("time2", {"points": [[0, 1], [1, 0]]}, high=0.5)})

    def test_alpha_wire_times_a_brightness_wire_is_one_math_node(self):
        self.alpha({"color1": color(brightness=w("curve1"), alpha=w("curve2"))},
                   {"color1": color(brightness=w("math1")),
                    "math1": {"kind": "math", "settings": {"op": "*"},
                              "inputs": {"values": [w("curve1"), w("curve2")]}}},
                   {"math node added"})

    def test_alpha_joins_a_brightness_list(self):
        self.alpha({"color1": color(brightness=[w("curve1"), 0.5], alpha=w("curve2"))},
                   {"color1": color(brightness=w("math1")),
                    "math1": {"kind": "math", "settings": {"op": "*"},
                              "inputs": {"values": [w("curve1"), 0.5, w("curve2")]}}},
                   {"joined a math node"}, version=2)

    def test_alpha_and_brightness_of_different_clocks_are_refused(self):
        old = graph(1, clock1={"kind": "clock", "inputs": {"every": 1}},
                    clock2={"kind": "clock", "inputs": {"every": 3}},
                    time1={"kind": "time", "inputs": {"clock": w("clock1")}},
                    time2={"kind": "time", "inputs": {"clock": w("clock2")}},
                    curve1=curve("time1"), curve2=curve("time2"),
                    color1=color(brightness=w("curve1"), alpha=w("curve2")))
        with self.assertRaisesRegex(m.Refused, "different clocks"):
            m.convert(old, DURATION)
        # Two clocks with equal inputs are one clock in version 3.
        old["nodes"]["clock2"]["inputs"]["every"] = 1
        new, _ = m.convert(old, DURATION)
        self.assertEqual(new["nodes"]["color1"]["inputs"]["brightness"], w("math1"))
        CHECKED.append((self.id(), clip(new)))

    def test_strobe_rate_takes_alpha_and_an_empty_rate_is_one_half(self):
        self.alpha({"strobe1": {"kind": "strobe", "inputs": {"rate": 0.8, "alpha": 0.5}}},
                   {"strobe1": {"kind": "strobe", "inputs": {"rate": 0.4}}})
        self.alpha({"strobe1": {"kind": "strobe", "inputs": {"alpha": w("curve2")}}},
                   {"strobe1": {"kind": "strobe", "inputs": {"rate": w("curve2")}},
                    "curve2": curve("time2", {"points": [[0, 1], [1, 0]]}, high=0.5)})

    def test_aim_alpha_is_unchanged(self):
        aim = {"kind": "aim", "settings": {"base": "direction"},
               "inputs": {"direction": [0, 1, -1], "alpha": w("curve2")}}
        self.alpha({"aim1": aim}, {"aim1": aim})

    # ---- whole graphs ----

    def test_version_3_passes_and_a_document_uses_each_clip_duration(self):
        new, _ = m.convert(graph(1, color1=color(alpha=0.5)))
        self.assertEqual(m.convert(new)[0], new)
        timed = graph(2, time1={"kind": "time", "inputs": {"delay": 0.5}},
                      curve1=curve("time1"), color1=color(brightness=w("curve1")))
        doc = {"clips": {"a": {**clip(timed), "duration": 6}, "b": clip(new)}}
        out = m.convert_document(doc)
        self.assertEqual(out["clips"]["a"]["graph"]["nodes"]["time1"]["inputs"], {"delay": 3})
        self.assertEqual(out["clips"]["a"]["duration"], 6)
        self.assertEqual(out["clips"]["b"]["graph"], new)


class Checked(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY"), "needs a version 3 clip_graph_parity")
    def test_every_converted_graph_passes_the_checker(self):
        self.assertTrue(CHECKED, "run the Convert tests first")
        lines = m.v2.evaluate(os.environ["LUMA_V3_PARITY"], [c for _, c in CHECKED], "--check")
        self.assertEqual(len(lines), len(CHECKED))
        failures = [f"{name}: {line['error']}"
                    for (name, _), line in zip(CHECKED, lines) if "error" in line]
        self.assertEqual(failures, [])

    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY") and os.environ.get("LUMA_V1_PARITY"),
                         "needs version 1 and version 3 clip_graph_parity binaries")
    def test_converted_version_1_graphs_play_as_before(self):
        self.assertTrue(PLAYED, "run the Convert tests first")
        for rig in m.RIGS.values():
            olds = m.play(os.environ["LUMA_V1_PARITY"], [a for _, a, _ in PLAYED], rig)
            news = m.play(os.environ["LUMA_V3_PARITY"], [b for _, _, b in PLAYED], rig)
            for (name, _, _), x, y in zip(PLAYED, olds, news):
                with self.subTest(name):
                    self.assertEqual(m.difference(x["ok"], y["ok"])[0] <= m.EXACT, True,
                                     m.difference(x["ok"], y["ok"]))


if __name__ == "__main__":
    suite = unittest.TestSuite()
    loader = unittest.TestLoader()
    for case in (Convert, Checked):
        suite.addTests(loader.loadTestsFromTestCase(case))
    sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
