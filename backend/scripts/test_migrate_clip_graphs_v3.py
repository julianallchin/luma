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
        new, why = m.convert(old, **kw)
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
                 color1=color(brightness=w("curve1"), alpha=w("curve2"))),
            {"shuffle: an existing time"})

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

    # A delay with no clock was a share of the clip. It becomes the space
    # shifted by a curve over time(), so the clip still stretches it.
    ORDER = {"kind": "space", "settings": {"kind": "order", "wrap": "no"},
             "inputs": {"heads": w("shuffle1")}}

    def over_the_clip(self, delay_shape, step, expected_shift, expected_step):
        old = graph(2, shuffle1={"kind": "shuffle"}, space1=self.ORDER,
                    curve1=curve("space1", {"points": delay_shape}),
                    time1={"kind": "time", "inputs": {"delay": w("curve1")}},
                    curve2=curve("time1", {"points": step}),
                    color1=color(brightness=w("curve2")))
        expected = dict(shuffle1={"kind": "shuffle"},
                        curve2=curve("space2", {"points": expected_step}),
                        color1=color(brightness=w("curve2")),
                        time2={"kind": "time"},
                        curve3=curve("time2", low=expected_shift[0], high=expected_shift[1]),
                        space2={**self.ORDER, "inputs": {"heads": w("shuffle1"),
                                                         "shift": w("curve3"), "scale": 1}})
        return self.convert(old, expected, {"delay over the clip: a space shifted by time()"})

    def test_a_falling_delay_with_no_clock_becomes_a_shifted_space(self):
        # Dissolve: delay 1 − rank; off when progress passes 1 − rank.
        self.over_the_clip([[0, 1], [1, 0]], [[0, 1], [0, 0], [1, 0]], (1, 0),
                           [[0, 1], [0, 0], [1, 0]])

    def test_a_rising_delay_with_no_clock_reverses_each_curve_over_the_time(self):
        # Build: delay = rank; on once progress passes it. The space reads
        # 1 − τ, so the step and its eases are mirrored.
        self.over_the_clip([[0, 0], [1, 1]], [[0, 0], [0, 1], [1, 1]], (-1, 0),
                           [[0, 1], [1, 1], [1, 0]])
        self.assertEqual(m.reversed_shape({"points": [[0, 0, "ease-in"], [0.25, 1, "hold"],
                                                      [0.5, 0.5, [0.1, 0.2, 0.3, 0.4]], [1, 1]]}),
                         {"points": [[0, 1, [0.7, 0.6, 0.9, 0.8]], [0.5, 0.5],
                                     [0.5, 1], [0.75, 1, "ease-out"], [1, 0]]})

    def test_a_delay_with_no_clock_that_is_not_a_straight_line_is_refused(self):
        for delay, why in ((0.25, "one number"),
                           (w("curve9"), "not a straight line")):
            old = graph(2, time1={"kind": "time", "inputs": {"delay": delay}},
                        space9={"kind": "space"},
                        curve9=curve("space9", {"points": [[0, 0], [0.5, 1], [1, 0]]}),
                        curve1=curve("time1"), color1=color(brightness=w("curve1")))
            with self.assertRaisesRegex(m.Refused, why):
                m.convert(old)

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
        old, expected = self.delay({"every": 4}, 0, 0)
        old["nodes"]["time1"]["inputs"]["length"] = 1
        self.convert(old, expected)
        old["nodes"]["time1"]["inputs"]["length"] = 0.5
        with self.assertRaisesRegex(m.Refused, "time length"):
            m.convert(old)

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
            m.convert(old)

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
        new, _ = m.convert(graph(2, **old))
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
                 strobe1={"kind": "strobe", "inputs": {"rate": w("math1"), "alpha": 0.25}},
                 math1={"kind": "math", "settings": {"op": "*"},
                        "inputs": {"values": [w("curve1"), 0.5, w("curve2")]}}))

    # ---- alpha ----

    # Alpha is opacity in version 3 and stays alpha: alone a clip looks as
    # it did, so these play the same in the parity check.

    def test_alpha_stays_alpha(self):
        self.convert(graph(1, color1=color(brightness=0.5, alpha=0.4)),
                     dict(color1=color(brightness=0.5, alpha=0.4)))
        fade = dict(time2={"kind": "time"}, curve2=curve("time2", {"points": [[0, 1], [1, 0]]}),
                    color1=color(brightness=0.5, alpha=w("curve2")))
        self.convert(graph(1, **fade), fade)

    def test_alpha_1_stays(self):
        self.convert(graph(1, color1=color(alpha=1)), dict(color1=color(alpha=1)))

    def test_an_alpha_list_becomes_a_math_node_in_alpha(self):
        self.convert(
            graph(2, time1={"kind": "time"}, curve1=curve("time1"),
                  color1=color(brightness=0.5, alpha=[w("curve1"), 0.5])),
            dict(time1={"kind": "time"}, curve1=curve("time1"),
                 color1=color(brightness=0.5, alpha=w("math1")),
                 math1={"kind": "math", "settings": {"op": "*"},
                        "inputs": {"values": [w("curve1"), 0.5]}}),
            {"list: math node"})

    def test_strobe_and_aim_alpha_stay(self):
        strobe = {"kind": "strobe", "inputs": {"rate": 0.8, "alpha": 0.5}}
        self.convert(graph(1, strobe1=strobe), dict(strobe1=strobe))
        aim = {"kind": "aim", "settings": {"base": "direction"},
               "inputs": {"direction": [0, 1, -1], "alpha": 0.5}}
        self.convert(graph(1, aim1=aim), dict(aim1=aim))

    # ---- wrapped scales (decision 50) ----

    def test_a_wrapped_scale_moves_into_the_curve_points(self):
        pill = {"points": [[0, 0], [0, 1], [1, 1], [1, 0]]}
        self.convert(
            graph(2, space1={"kind": "space", "settings": {"kind": "line", "wrap": "yes"},
                             "inputs": {"shift": 0.3, "length": 0.25}},
                  curve1=curve("space1", pill), color1=color(brightness=w("curve1"))),
            dict(space1={"kind": "space", "settings": {"kind": "line", "wrap": "yes"},
                         "inputs": {"shift": 0.3}},
                 curve1=curve("space1", {"points": [[0, 0], [0, 1], [0.25, 1], [0.25, 0],
                                                    [1, 0]]}),
                 color1=color(brightness=w("curve1"))),
            {"wrapped scale: into the curve points"})

    def test_a_wrapped_scale_above_1_cuts_the_curve_at_1(self):
        # x reads 0..0.8 of the old curve: a point at 0.8 lands on 1, and a
        # linear segment over 0.8 is cut at its value there.
        for points, expected in (
                ([[0, 0], [0.4, 1, "ease-in"], [0.8, 0], [1, 0.5]],
                 [[0, 0], [0.5, 1, "ease-in"], [1, 0]]),
                ([[0, 0], [0.4, 1], [1, 0]], [[0, 0], [0.5, 1], [1, 0.333333333333]])):
            with self.subTest(points):
                self.convert(
                    graph(2, space1={"kind": "space", "settings": {"kind": "angle"},
                                     "inputs": {"length": 1.25}},
                          curve1=curve("space1", {"points": points}),
                          color1=color(brightness=w("curve1"))),
                    dict(space1={"kind": "space", "settings": {"kind": "angle"}, "inputs": {}},
                         curve1=curve("space1", {"points": expected}),
                         color1=color(brightness=w("curve1"))))
        with self.assertRaisesRegex(m.Refused, "eased segment"):
            m.scaled_points([[0, 0], [0.5, 1, "ease-in"], [1, 0]], 1.25)

    # ---- whole graphs ----

    def test_version_3_passes_and_a_document_converts_each_clip(self):
        new, _ = m.convert(graph(1, color1=color(alpha=0.5)))
        self.assertEqual(m.convert(new)[0], new)
        timed = graph(2, clock1={"kind": "clock", "inputs": {"every": 2}},
                      time1={"kind": "time", "inputs": {"clock": w("clock1"), "delay": 0.5}},
                      curve1=curve("time1"), color1=color(brightness=w("curve1")))
        doc = {"clips": {"a": {**clip(timed), "duration": 6}, "b": clip(new)}}
        out = m.convert_document(doc)
        self.assertEqual(out["clips"]["a"]["graph"]["nodes"]["time1"]["inputs"],
                         {"every": 2, "delay": 1})
        self.assertEqual(out["clips"]["a"]["duration"], 6)
        self.assertEqual(out["clips"]["b"]["graph"], new)


class Audit(unittest.TestCase):
    """The 2026-09-30 audit steps (spec section 0)."""

    def test_lighten_becomes_max_and_value_becomes_replace_with_alpha(self):
        wash = graph(1, color1=color(brightness=0.5))
        new, blend, _ = m.convert_clip(wash, "lighten")
        self.assertEqual(blend, "max")
        self.assertEqual(new["nodes"]["color1"]["inputs"], {"color": [1, 0.5, 0.25],
                                                            "brightness": 0.5})
        new, blend, notes = m.convert_clip(wash, "value")
        self.assertEqual(blend, "replace")
        self.assertEqual(new["nodes"]["color1"]["inputs"]["alpha"], 0.5)
        self.assertIn("blend: value → replace", notes)
        self.assertEqual(m.convert_clip(wash, "screen")[1], "screen")

    def test_a_falling_jump_moves_a_hair_right_so_a_tie_reads_as_before(self):
        region = {"points": [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]}
        old = graph(2, space1={"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
                    curve1=curve("space1", region), color1=color(brightness=w("curve1")))
        new, _, _ = m.convert_clip(old, "replace")
        self.assertEqual(new["nodes"]["curve1"]["inputs"]["shape"]["points"],
                         [[0, 1], [0.5 + m.TIE, 1], [0.5 + m.TIE, 0], [1, 0]])

    def test_a_pill_end_at_1_grows_its_space_a_hair(self):
        pill = {"points": [[0, 0], [0, 1], [0.5, 1], [0.5, 2 / 3], [1, 1], [1, 0]]}
        old = graph(2, space1={"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                               "inputs": {"length": 0.25}},
                    curve1=curve("space1", pill), color1=color(brightness=w("curve1")))
        new, _, _ = m.convert_clip(old, "replace")
        self.assertAlmostEqual(new["nodes"]["space1"]["inputs"]["scale"], 0.25 * (1 + m.TIE))
        points = new["nodes"]["curve1"]["inputs"]["shape"]["points"]
        # The rising jump at 0 stays; the falling one inside moved right.
        self.assertEqual(points[:2], [[0, 0], [0, 1]])
        self.assertEqual(points[-2:], [[1, 1], [1, 0]])

    def test_a_phase_of_0_goes(self):
        old = graph(2, time1={"kind": "time", "inputs": {"phase": 0}},
                    curve1=curve("time1"), color1=color(brightness=w("curve1")))
        new, _ = m.convert(old)
        self.assertNotIn("phase", new["nodes"]["time1"].get("inputs", {}))

    # A row of four heads along U from u 0 to 3, and one more at u 9: the
    # box's middle is u 4.5, the centroid u 3.
    ROW = [dict(id=f"f{u}:0", group="all", world=[u, 0, 0], uvz=[u, 0, 0])
           for u in (0, 1, 2, 3, 9)]

    def coordinate(self, nodes, node_id):
        """d / max from the space's `at`, as version 3 reads radial."""
        at = nodes[node_id].get("inputs", {}).get("at", [0.5, 0.5, 0.5])
        centre = 0 + at[0] * 9
        far = max(abs(c["uvz"][0] - centre) for c in self.ROW)
        return {c["id"]: abs(c["uvz"][0] - centre) / far for c in self.ROW}

    def test_a_mirrored_line_is_fitted_to_the_folded_span_on_the_rig(self):
        old = graph(2, mirror1={"kind": "mirror", "inputs": {"normal": [1, 0, 0]}},
                    space1={"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                            "inputs": {"heads": w("mirror1"), "direction": [1, 0, 0],
                                       "shift": 0.2, "length": 0.5}},
                    curve1=curve("space1"),
                    color1=color(heads=w("mirror1"), brightness=w("curve1")))
        # Version 3 reads the folded heads at 0.1 to 0.4 of the plane's
        # span; version 2 read them 0 to 1.
        a = {c["id"]: v for c, v in zip(self.ROW, (0.1, 0.3, 0.4, 0.2, 0.25))}
        new, notes = m.convert(old, cells=self.ROW, coordinate=lambda n, i: a)
        inputs = new["nodes"]["space1"]["inputs"]
        self.assertIn("rig: mirrored line fitted to the folded span", notes)
        for v in a.values():
            before = ((v - 0.1) / 0.3 - 0.2) / 0.5
            self.assertAlmostEqual((v - inputs["shift"]) / inputs["scale"], before)

    def test_a_radial_space_takes_the_old_centroid_and_its_nearest_share(self):
        old = graph(2, space1={"kind": "space", "settings": {"kind": "radial", "wrap": "no"},
                               "inputs": {"length": 0.5}},
                    curve1=curve("space1"), color1=color(brightness=w("curve1")))
        new, notes = m.convert(old, cells=self.ROW, coordinate=self.coordinate)
        inputs = new["nodes"]["space1"]["inputs"]
        self.assertAlmostEqual(inputs["at"][0], 3 / 9)
        self.assertEqual(inputs["at"][1:], [0.5, 0.5])
        # Distances from u 3: 3, 2, 1, 0, 6. The nearest is on the centre
        # (m 0), so shift and scale stay.
        self.assertEqual(inputs["scale"], 0.5)
        self.assertNotIn("shift", inputs)
        # Without the head at u 3 the nearest is 1 of 6: m = 1/6, so the
        # old (d − 1) / 5 is (d/6 − 1/6) / (5/6).
        row = [c for c in self.ROW if c["uvz"][0] != 3]
        centroid = sum(c["uvz"][0] for c in row) / len(row)

        def coordinate(nodes, node_id):
            at = nodes[node_id]["inputs"]["at"]
            centre = at[0] * 9
            far = max(abs(c["uvz"][0] - centre) for c in row)
            return {c["id"]: abs(c["uvz"][0] - centre) / far for c in row}
        new, _ = m.convert(old, cells=row, coordinate=coordinate)
        inputs = new["nodes"]["space1"]["inputs"]
        self.assertAlmostEqual(inputs["at"][0], centroid / 9)
        d = [abs(c["uvz"][0] - centroid) for c in row]
        nearest = min(d) / max(d)
        self.assertAlmostEqual(inputs["shift"], nearest)
        self.assertAlmostEqual(inputs["scale"], 0.5 * (1 - nearest))

    def test_an_angle_space_takes_the_old_centroid(self):
        old = graph(2, space1={"kind": "space", "settings": {"kind": "angle", "wrap": "yes"}},
                    curve1=curve("space1"), color1=color(brightness=w("curve1")))
        new, _ = m.convert(old, cells=self.ROW, coordinate=self.coordinate)
        self.assertEqual(set(new["nodes"]["space1"]["inputs"]), {"at"})

    def test_a_tilted_best_fit_is_written_and_a_stage_axis_is_not(self):
        tilted = [dict(id=f"t{i}:0", group="all", world=[i * 0.8, 0, i * 0.6],
                       uvz=[i * 0.8, 0, i * 0.6]) for i in range(5)]
        old = graph(2, space1={"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
                    curve1=curve("space1"), color1=color(brightness=w("curve1")))
        new, _ = m.convert(old, cells=tilted, coordinate=None)
        direction = new["nodes"]["space1"]["inputs"]["direction"]
        self.assertAlmostEqual(direction[0], 0.8)
        self.assertAlmostEqual(direction[2], 0.6)
        new, _ = m.convert(old, cells=self.ROW, coordinate=None)
        self.assertNotIn("inputs", new["nodes"]["space1"])


class Checked(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY"), "needs a version 3 clip_graph_parity")
    def test_every_converted_graph_passes_the_checker(self):
        self.assertTrue(CHECKED, "run the Convert tests first")
        lines = m.v2.evaluate(os.environ["LUMA_V3_PARITY"], [c for _, c in CHECKED], "--check")
        self.assertEqual(len(lines), len(CHECKED))
        failures = [f"{name}: {line['error']}"
                    for (name, _), line in zip(CHECKED, lines) if "error" in line]
        self.assertEqual(failures, [])

    @unittest.skipUnless(os.environ.get("LUMA_V3_PARITY"), "needs a version 3 clip_graph_parity")
    def test_a_delay_over_the_clip_stretches_with_the_clip_and_plays_as_the_preset(self):
        # Version 2 Build and Dissolve: a delay of rank (or 1 − rank) turns
        # with no clock. Converted, each plays as the shipped preset and the
        # same at the same share of a 16- and a 32-beat clip.
        shipped = {c["name"]: c["graph"] for c in json.loads(
            (pathlib.Path(__file__).resolve().parents[1]
             / "crates/patterns/src/presets.json").read_text())["clips"]}
        for name, delay, step in (("Build", [[0, 0], [1, 1]], [[0, 0], [0, 1], [1, 1]]),
                                  ("Dissolve", [[0, 1], [1, 0]], [[0, 1], [0, 0], [1, 0]])):
            old = graph(2, shuffle1={"kind": "shuffle"}, space1=Convert.ORDER,
                        curve1=curve("space1", {"points": delay}),
                        time1={"kind": "time", "inputs": {"delay": w("curve1")}},
                        curve2=curve("time1", {"points": step}),
                        color1=color(color=[1, 1, 1], brightness=w("curve2")))
            new, _ = m.convert(old)
            requests = [dict(clip={**clip(g), "duration": length}, cells=m.BARS,
                             beats=[length * k / 64 for k in range(65)])
                        for g in (new, shipped[name]) for length in (16, 32)]
            played = m.v2.evaluate(os.environ["LUMA_V3_PARITY"], requests)
            ours, ours_long, preset, preset_long = (m.lights(p["ok"]) for p in played)
            with self.subTest(name):
                self.assertEqual(ours, ours_long)
                self.assertEqual(preset, preset_long)
                self.assertEqual(ours, preset)
                self.assertTrue(0 < sum(ours) < len(ours), "some heads lit, not all")

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
    for case in (Convert, Audit, Checked):
        suite.addTests(loader.loadTestsFromTestCase(case))
    sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
