#!/usr/bin/env python3
"""Tests for migrate_clip_graphs.py: one per row of spec sections 8.2 and
8.3, fixed old inputs in, the expected graph out. Every expected graph then
goes through the Rust checker (patterns/examples/clip_graph_parity.rs
--check) in one batch.

    python3 backend/scripts/test_migrate_clip_graphs.py
"""
import json
import os
import pathlib
import subprocess
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_graphs as m  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
CHECKED = []  # (test name, clip) for the checker batch


def n(v):
    return {"type": "number", "value": v}


def p(v):
    return {"type": "proportion", "value": v}


def beats(v):
    return {"type": "beats", "value": v}


def time(points, **extra):
    return {"type": "time", "value": {"points": points, **extra}}


def ev(every, life=None):
    e = {"every": every if isinstance(every, dict) else beats(every)}
    if life is not None:
        e["life"] = life if isinstance(life, dict) else beats(life)
    return e


def axis(kind, **extra):
    return {"source": {"kind": kind, **{k: v for k, v in extra.items() if k == "direction"}},
            "per_group": False, "reverse": False,
            **{k: v for k, v in extra.items() if k != "direction"}}


def color_inputs(brightness=None, color=None, fade=None):
    return {"color": color or {"type": "color", "value": [1.0, 1.0, 1.0]},
            "brightness": brightness or p(1.0), "fade": fade or p(1.0)}


def aim_inputs(**over):
    inputs = {"base": {"type": "choice", "value": "direction"},
              "direction": {"type": "vector", "value": [0.0, 0.766, -0.643]},
              "point": {"type": "vector", "value": [0.0, 0.0, 0.0]},
              "lean": {"type": "vector", "value": [0, 0, 0]},
              "horizontal": n(0), "vertical": n(0),
              "axis": {"type": "mapping", "value": axis("order")}, "fade": p(1.0)}
    inputs.update(over)
    return inputs


def space(axis_value, curve, **extra):
    return {"type": "space", "value": {"axis": axis_value, "curve": {"points": curve}, **extra}}


def node(kind, inputs=None, settings=None):
    result = {"kind": kind}
    if settings:
        result["settings"] = settings
    result["inputs"] = inputs or {}
    return result


def w(node_id):
    return {"node": node_id}


class Convert(unittest.TestCase):
    maxDiff = None

    def convert(self, form, inputs, duration=16.0, blend=None):
        graph, out, notes = m.convert_inputs(form, inputs, duration)
        clip = {"name": m.name_for(graph, out), "start": 0.0, "duration": duration, "seed": 41,
                "selection": {"expression": "all"}, "z_index": 0,
                "blend_mode": blend or ("offset" if form == "aim@1" else "replace"), "graph": graph}
        CHECKED.append((self.id(), clip))
        return graph["nodes"], notes

    # -- 8.2 old top level --------------------------------------------
    def test_color_form(self):
        nodes, _ = self.convert("color@1", color_inputs(
            color={"type": "color", "value": [1.0, 0.0, 0.0]}, brightness=p(0.5), fade=p(0.25)))
        self.assertEqual(nodes, {"color1": node("color", {
            "color": [1.0, 0.0, 0.0], "brightness": 0.5, "alpha": 0.25})})

    def test_aim_form(self):
        mirror = {"normal": [1.0, 0.0, 0.0], "offset": 0.0}
        nodes, _ = self.convert("aim@1", aim_inputs(
            direction={"type": "vector", "value": [0.0, 0.5, -1.0]},
            horizontal=n(10), vertical=n(-5), fade=p(0.5),
            axis={"type": "mapping", "value": axis("u", mirror=mirror)}))
        self.assertEqual(nodes, {
            "mirror1": node("mirror", {"normal": [1.0, 0.0, 0.0]}),
            "aim1": node("aim", {"heads": w("mirror1"), "direction": [0.0, 0.5, -1.0],
                                 "yaw": 10.0, "pitch": -5.0, "alpha": 0.5},
                         {"base": "direction"})})

    def test_aim_point_base(self):
        nodes, _ = self.convert("aim@1", aim_inputs(
            base={"type": "choice", "value": "point"},
            point={"type": "vector", "value": [0.0, 3.0, 0.0]}))
        self.assertEqual(nodes, {"aim1": node("aim", {"point": [0.0, 3.0, 0.0]}, {"base": "point"})})

    def test_strobe_form(self):
        nodes, _ = self.convert("strobe.constant@1", {"rate": p(0.9), "fade": p(0.5)})
        self.assertEqual(nodes, {"strobe1": node("strobe", {"rate": 0.9, "alpha": 0.5})})

    def test_clip_fields_unchanged(self):
        old = {"graph": "color@1", "start": 4.0, "duration": 8.0, "seed": 7, "selection_seed": 9,
               "selection": {"expression": "front"}, "z_index": 3, "blend_mode": "add",
               "inputs": color_inputs()}
        new = m.convert_clip(old)
        for key in ("start", "duration", "seed", "selection_seed", "selection", "z_index", "blend_mode"):
            self.assertEqual(new[key], old[key])
        self.assertNotIn("inputs", new)
        self.assertEqual(new["name"], "Wash" if m.load_presets() else "Color · still")

    # -- 8.2 old values -----------------------------------------------
    def test_plain_numbers(self):
        for kind in m.SCALARS:
            nodes, _ = self.convert("strobe.constant@1", {"rate": {"type": kind, "value": 0.25},
                                                          "fade": p(1.0)})
            self.assertEqual(nodes["strobe1"]["inputs"], {"rate": 0.25})

    def test_time_numbers(self):
        nodes, _ = self.convert("color@1", color_inputs(
            brightness=time([[0.0, 0.2, "ease-out"], [0.5, 0.8], [1.0, 0.2]], events=ev(2),
                            gain=n(0.5))))
        self.assertEqual(nodes, {
            "clock1": node("clock", {"every": 2.0}),
            "time1": node("time", {"clock": w("clock1")}),
            "curve1": node("curve", {"x": w("time1"),
                                     "shape": {"points": [[0.0, 0.0, "ease-out"], [0.5, 1.0], [1.0, 0.0]]},
                                     "low": 0.1, "high": 0.4}, {"kind": "number"}),
            "color1": node("color", {"brightness": w("curve1")})})

    def test_constant_time_curve(self):
        nodes, _ = self.convert("color@1", color_inputs(
            brightness=time([[0.0, 0.6], [1.0, 0.6]], events=ev(1), gain=n(0.5))))
        self.assertEqual(nodes["curve1"]["inputs"]["low"], 0.3)
        self.assertEqual(nodes["curve1"]["inputs"]["high"], 0.3)

    def test_color_keyframes(self):
        nodes, notes = self.convert("color@1", color_inputs(color=time(
            [[0.0, [1.0, 0.0, 0.0], "hold"], [0.5, [0.0, 0.0, 1.0]], [1.0, [0.0, 1.0, 0.0]]],
            events=ev(4))))
        self.assertEqual(nodes["curve1"], node("curve", {
            "x": w("time1"),
            "shape": {"points": [[0.0, 0.0, "hold"], [0.5, 0.5], [1.0, 1.0]]},
            "gradient": {"stops": [{"t": 0.0, "color": [1.0, 0.0, 0.0]},
                                   {"t": 0.5, "color": [0.0, 0.0, 1.0]},
                                   {"t": 1.0, "color": [0.0, 1.0, 0.0]}]}}, {"kind": "color"}))
        self.assertIn(m.COLOR_KEYS, notes)

    def test_time_gradient(self):
        g = {"stops": [{"t": 0.0, "color": [1.0, 0.0, 0.0]}, {"t": 1.0, "color": [0.0, 0.0, 1.0]}]}
        nodes, _ = self.convert("color@1", color_inputs(color={"type": "time", "value": {
            "gradient": g, "curve": {"points": [[0, 0], [0.5, 1], [1, 0]]}, "events": ev(4)}}))
        self.assertEqual(nodes["curve1"], node("curve", {
            "x": w("time1"), "shape": {"points": [[0, 0], [0.5, 1], [1, 0]]}, "gradient": g},
            {"kind": "color"}))

    def test_phase_and_gain_sources(self):
        phase = space(axis("u"), [[0, 0], [1, 1]], gain=n(-0.5))
        gain = time([[0.0, 0.0], [1.0, 1.0]], events=ev(0))
        nodes, _ = self.convert("aim@1", aim_inputs(horizontal=time(
            [[0.0, -1.0], [1.0, 1.0]], events=ev(4), gain=gain, phase=phase)))
        # Phase −0.5 × x wraps into 0–1 by adding one turn.
        self.assertEqual(nodes["space1"], node("space", {"direction": [1, 0, 0]},
                                               {"kind": "line", "wrap": "no"}))
        self.assertEqual(nodes["curve1"], node("curve", {
            "x": w("space1"), "shape": {"points": [[0, 0], [1, 1]]}, "low": 1.0, "high": 0.5},
            {"kind": "number"}))
        self.assertEqual(nodes["time2"], node("time", {"clock": w("clock1"), "phase": w("curve1")}))
        # Gain → low and high each a curve over the gain's own coordinate.
        yaw = nodes["curve4"]["inputs"]
        self.assertEqual(yaw["x"], w("time2"))
        self.assertEqual(nodes[yaw["low"]["node"]]["inputs"], {
            "x": w("time1"), "shape": {"points": [[0.0, 0.0], [1.0, 1.0]]}, "high": -1.0})
        self.assertEqual(nodes[yaw["high"]["node"]]["inputs"], {
            "x": w("time1"), "shape": {"points": [[0.0, 0.0], [1.0, 1.0]]}})

    def test_same_as(self):
        nodes, _ = self.convert("aim@1", aim_inputs(
            horizontal=time([[0.0, -1.0], [1.0, 1.0]], events=ev(4), gain=n(20)),
            vertical=time([[0.0, 1.0], [1.0, -1.0]], events={"same_as": "horizontal"}, gain=n(20))))
        self.assertEqual([k for k, v in nodes.items() if v["kind"] == "clock"], ["clock1"])
        self.assertEqual([k for k, v in nodes.items() if v["kind"] == "time"], ["time1"])

    def test_space_stroke(self):
        brightness = space(axis("u", mirror={"normal": [1.0, 0.0, 0.0], "offset": 0.5}),
                           [[0, 1], [1, 1]], grain="clump2", boundary="wrap",
                           width=n(0.2), width_relative=False, gain=n(0.5),
                           offset=time([[0.0, 0.0], [1.0, 1.0]], events=ev(2, 2)))
        nodes, _ = self.convert("color@1", color_inputs(brightness=brightness))
        self.assertEqual(nodes["group1"], node("group", {"size": 2.0}))
        self.assertEqual(nodes["mirror1"], node("mirror", {"heads": w("group1"),
                                                           "normal": [1.0, 0.0, 0.0], "offset": 0.5}))
        # Wrap: no overrun; the centre-anchored offset moves back by w/2.
        self.assertEqual(nodes["curve1"]["inputs"]["low"], -0.1)
        self.assertEqual(nodes["curve1"]["inputs"]["high"], 0.9)
        self.assertEqual(nodes["space1"], node("space", {
            "heads": w("mirror1"), "direction": [1, 0, 0], "offset": w("curve1"), "width": 0.2},
            {"kind": "line", "wrap": "yes"}))
        self.assertEqual(nodes["curve2"], node("curve", {
            "x": w("space1"), "shape": {"points": [[0, 1], [1, 1]]}, "high": 0.5}, {"kind": "number"}))

    def test_axis_sources(self):
        cases = [("u", {}, "line", [1, 0, 0]), ("v", {}, "line", [0, 1, 0]),
                 ("z", {}, "line", [0, 0, 1]),
                 ("vector", {"direction": [1.0, 0.0, 1.0]}, "line", [1.0, 0.0, 1.0]),
                 ("major_axis", {}, "line", None), ("order", {}, "order", None),
                 ("radial", {"plane": {"kind": "auto"}}, "radial", None),
                 ("radial", {"plane": {"kind": "up_down"}}, "radial", [0, 0, 1]),
                 ("angle", {"plane": {"kind": "front_back"}}, "angle", [0, -1, 0]),
                 ("angle", {"plane": {"kind": "left_right"}}, "angle", [1, 0, 0]),
                 ("angle", {"plane": {"kind": "custom", "normal": [0.0, 1.0, 1.0]}}, "angle",
                  [0.0, 1.0, 1.0])]
        for kind, extra, new, direction in cases:
            with self.subTest(kind=kind, extra=extra):
                nodes, _ = self.convert("color@1", color_inputs(
                    brightness=space(axis(kind, **extra), [[0, 0], [1, 1]])))
                self.assertEqual(nodes["space1"]["settings"]["kind"], new)
                self.assertEqual(nodes["space1"]["inputs"].get("direction"), direction)
        nodes, _ = self.convert("color@1", color_inputs(
            brightness=space(axis("random"), [[0, 0], [1, 1]])))
        self.assertEqual(nodes["space1"]["inputs"]["heads"], w("shuffle1"))
        self.assertEqual(nodes["space1"]["settings"]["kind"], "order")

    def test_relative_width(self):
        # every 1, life 2: spacing 0.5; overrun (clip, gliding offset).
        brightness = space(axis("u"), [[0, 1], [1, 1]], width=n(0.5), width_relative=True,
                           boundary="clip", offset=time([[0.0, 0.0], [1.0, 1.0]], events=ev(1, 2)))
        nodes, _ = self.convert("color@1", color_inputs(brightness=brightness))
        gap = 0.25
        width = gap / (1 - gap)
        self.assertAlmostEqual(nodes["space1"]["inputs"]["width"], width)
        # Overrun offset: off × (1 + w) − w.
        self.assertAlmostEqual(nodes["curve1"]["inputs"]["low"], -width)
        self.assertAlmostEqual(nodes["curve1"]["inputs"].get("high", 1.0), 1.0)

    def test_offset_start_anchored(self):
        brightness = space(axis("u"), [[0, 1], [1, 1]], width=n(0.2), boundary="clip",
                           offset=time([[0.0, 0.0], [1.0, 1.0]], events=ev(2, 2)))
        nodes, _ = self.convert("color@1", color_inputs(brightness=brightness))
        self.assertAlmostEqual(nodes["curve1"]["inputs"]["low"], -0.2)
        self.assertAlmostEqual(nodes["curve1"]["inputs"].get("high", 1.0), 1.0)

    def test_shape_auto_reverse(self):
        comet = [[0, 0], [0.95, 1], [1, 0]]
        backward = space(axis("u"), comet, width=n(0.2), boundary="clip",
                         offset=time([[0.0, 1.0], [1.0, 0.0]], events=ev(2, 2)))
        nodes, notes = self.convert("color@1", color_inputs(brightness=backward))
        self.assertEqual(nodes["curve2"]["inputs"]["shape"]["points"],
                         [[0.0, 0], [0.050000000000000044, 1], [1.0, 0]])
        self.assertNotIn(m.ASYMMETRIC, notes)
        bounce = space(axis("u"), comet, width=n(0.2), boundary="clip",
                       offset=time([[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]], events=ev(2, 2)))
        nodes, notes = self.convert("color@1", color_inputs(brightness=bounce))
        self.assertEqual(nodes["curve2"]["inputs"]["shape"]["points"], comet)
        self.assertIn(m.ASYMMETRIC, notes)

    def test_random(self):
        brightness = {"type": "random", "value": {
            "events": ev(0.125, 0.5), "grain": "clump2", "coverage": p(0.3),
            "level": time([[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]])}}
        nodes, _ = self.convert("color@1", color_inputs(brightness=brightness))
        self.assertEqual(nodes["clock1"], node("clock", {"every": 0.125, "duration": 0.5}))
        self.assertEqual(nodes["group1"], node("group", {"size": 2.0}))
        self.assertEqual(nodes["shuffle1"], node("shuffle", {"heads": w("group1"), "clock": w("clock1")}))
        self.assertEqual(nodes["space1"], node("space", {"heads": w("shuffle1"), "offset": 0.0,
                                                         "width": 0.3}, {"kind": "order", "wrap": "no"}))
        # The level inherits the random clock.
        self.assertEqual(nodes["time1"], node("time", {"clock": w("clock1")}))
        self.assertEqual(nodes["curve2"], node("curve", {
            "x": w("space1"), "shape": {"points": m.ON}, "high": w("curve1")}, {"kind": "number"}))

    def test_noise(self):
        brightness = {"type": "noise", "value": {
            "speed": beats(8.0), "scale": p(0.5), "contrast": p(0.25), "range": [n(0.2), n(1.0)],
            "grain": "fixture", "independent": False}}
        nodes, notes = self.convert("color@1", color_inputs(brightness=brightness))
        self.assertEqual(nodes["group1"], node("group", {}))
        self.assertEqual(nodes["noise1"], node("noise", {"heads": w("group1"), "speed": 8.0,
                                                         "scale": 0.5, "contrast": 0.25}))
        self.assertEqual(nodes["curve1"]["inputs"], {"x": w("noise1"), "shape": {"points": m.RAMP_UP},
                                                     "low": 0.2})
        self.assertIn(m.NOISE_SPATIAL, notes)
        independent = {"type": "noise", "value": {"speed": beats(1.0), "range": [n(-40.0), n(40.0)],
                                                  "independent": True, "key": "1", "grain": "head"}}
        nodes, notes = self.convert("aim@1", aim_inputs(horizontal=independent))
        self.assertEqual(nodes["noise1"]["inputs"], {"speed": 1.0, "scale": 0.02})
        self.assertIn(m.NOISE_INDEPENDENT, notes)

    def test_audio(self):
        audio = {"type": "audio", "value": {"from_hz": 40.0, "to_hz": 100.0, "floor": 0.2,
                                            "gain": n(0.8)}}
        nodes, _ = self.convert("color@1", color_inputs(brightness=audio))
        self.assertEqual(nodes["audio1"], node("audio", {"low_hz": 40.0, "high_hz": 100.0}))
        self.assertEqual(nodes["curve1"]["inputs"], {"x": w("audio1"),
                                                     "shape": {"points": [[0, 0.2], [1, 1]]}, "high": 0.8})
        gated = {"type": "audio", "value": {"from_hz": 40.0, "to_hz": 100.0, "floor": 0.2,
                                            "threshold": 0.5}}
        nodes, _ = self.convert("strobe.constant@1", {"rate": gated, "fade": p(1.0)})
        points = nodes["curve1"]["inputs"]["shape"]["points"]
        self.assertEqual([points[0], points[2]], [[0, 0, "hold"], [1, 1]])
        self.assertEqual(points[1][0], 0.5)
        self.assertAlmostEqual(points[1][1], 0.6)

    def test_constant_lean(self):
        nodes, _ = self.convert("aim@1", aim_inputs(
            direction={"type": "vector", "value": [0.0, 1.0, 0.0]},
            lean={"type": "vector", "value": [0.0, 0.0, 90.0]}))
        direction = nodes["aim1"]["inputs"]["direction"]
        self.assertAlmostEqual(direction[1], 0.0)
        self.assertAlmostEqual(direction[2], 1.0)

    def test_fan_lean(self):
        lean = space(axis("u"), [[0, 0], [1, 1]], gain=n(40.0))
        nodes, notes = self.convert("aim@1", aim_inputs(lean=lean))
        self.assertEqual(nodes["curve1"]["inputs"], {"x": w("space1"), "shape": {"points": m.RAMP_UP},
                                                     "low": -20.0, "high": 20.0})
        self.assertEqual(nodes["aim1"]["inputs"]["yaw"], w("curve1"))
        self.assertIn(m.FAN, notes)

    def test_bloom_lean(self):
        lean = space(axis("radial", plane={"kind": "auto"}), [[0, 0], [1, 1]],
                     gain=time([[0.0, -90.0], [1.0, 90.0]], events=ev(0)))
        nodes, notes = self.convert("aim@1", aim_inputs(lean=lean))
        self.assertEqual(nodes["aim1"]["settings"], {"base": "away"})
        self.assertEqual(nodes["curve1"]["settings"], {"kind": "vector"})
        self.assertEqual(nodes["aim1"]["inputs"], {"point": w("curve1")})
        self.assertIn(m.BLOOM, notes)

    def test_fade(self):
        nodes, _ = self.convert("color@1", color_inputs(
            fade=time([[0.0, 0.0, "ease-out"], [0.125, 1.0], [1.0, 1.0]], events=ev(n(0)))))
        self.assertEqual(nodes["time1"], node("time"))
        self.assertEqual(nodes["color1"]["inputs"], {"alpha": w("curve1")})

    # -- 8.3 clocks ---------------------------------------------------
    def brightness_clock(self, **extra):
        nodes, _ = self.convert("color@1", color_inputs(
            brightness=time([[0.0, 0.0], [1.0, 1.0]], **extra)), duration=12.0)
        return nodes

    def test_clock_absent(self):
        nodes = self.brightness_clock()
        self.assertEqual(nodes["time1"], node("time"))
        self.assertNotIn("clock1", nodes)

    def test_clock_every_zero(self):
        nodes = self.brightness_clock(events=ev(0))
        self.assertNotIn("clock1", nodes)

    def test_clock_every_zero_with_life(self):
        nodes = self.brightness_clock(events=ev(0, 2))
        self.assertEqual(nodes["clock1"], node("clock", {"every": 24.0, "duration": 2.0}))

    def test_clock_every(self):
        nodes = self.brightness_clock(events=ev(0.5))
        self.assertEqual(nodes["clock1"], node("clock", {"every": 0.5}))

    def test_clock_every_and_life(self):
        nodes = self.brightness_clock(events=ev(0.5, 2))
        self.assertEqual(nodes["clock1"], node("clock", {"every": 0.5, "duration": 2.0}))

    def test_clock_life_zero_is_the_clip(self):
        nodes = self.brightness_clock(events=ev(1, 0))
        self.assertEqual(nodes["clock1"], node("clock", {"every": 1.0, "duration": 12.0}))

    def test_clock_sources(self):
        every = time([[0.0, 8.0, "hold"], [0.5, 4.0], [1.0, 4.0]], events=ev(n(0)))
        nodes = self.brightness_clock(events=ev(every, beats(2.0)))
        self.assertEqual(nodes["clock1"]["inputs"], {"every": w("curve1"), "duration": 2.0})
        self.assertEqual(nodes["curve1"]["inputs"], {
            "x": w("time1"), "shape": {"points": [[0.0, 1.0, "hold"], [0.5, 0.0], [1.0, 0.0]]},
            "low": 4.0, "high": 8.0})

    def test_nested_source_inherits(self):
        brightness = space(axis("u"), [[0, 1], [1, 1]], width=time([[0.0, 0.1], [1.0, 0.3]]),
                           boundary="wrap", offset=time([[0.0, 0.0], [1.0, 1.0]], events=ev(2)))
        nodes, _ = self.convert("color@1", color_inputs(brightness=brightness))
        self.assertEqual([k for k, v in nodes.items() if v["kind"] == "time"], ["time1"])
        self.assertEqual(nodes["time1"], node("time", {"clock": w("clock1")}))

    # -- 8.4 hard cases -----------------------------------------------
    def test_hard_cases(self):
        g = {"stops": [{"t": 0.0, "color": [1.0, 0.0, 0.0]}, {"t": 1.0, "color": [0.0, 0.0, 1.0]}]}
        cases = {
            "gain source on a gradient": ("color@1", color_inputs(color={"type": "space", "value": {
                "axis": axis("u"), "gradient": g, "gain": time([[0.0, 0.0], [1.0, 1.0]])}})),
            "numbers and colors": ("color@1", color_inputs(
                brightness=time([[0.0, 0.0], [1.0, [1.0, 1.0, 1.0]]]))),
        }
        for why, (form, inputs) in cases.items():
            with self.subTest(why=why), self.assertRaises(m.Unmappable):
                m.convert_inputs(form, inputs, 8.0)

    def test_vector_path_off_a_line(self):
        # Two runs joined by a step at x 0.5; a held stretch is not a run.
        nodes, notes = self.convert("aim@1", aim_inputs(direction=time(
            [[0.0, [0.0, 1.0, 0.0], "ease-out"], [0.25, [0.0, 1.0, 0.0]], [0.5, [1.0, 0.0, 0.0]],
             [1.0, [0.0, 0.0, -1.0]]])))
        self.assertIn(m.PATH, notes)
        outer = nodes[nodes["aim1"]["inputs"]["direction"]["node"]]["inputs"]
        self.assertEqual(outer["shape"]["points"], [[0.0, 0.0, "hold"], [0.5, 1.0], [1.0, 1.0]])
        first, second = nodes[outer["low"]["node"]]["inputs"], nodes[outer["high"]["node"]]["inputs"]
        self.assertEqual(first["shape"]["points"], [[0.0, 0.0], [0.25, 0.0], [0.5, 1.0], [1.0, 1.0]])
        self.assertEqual((first["low"], first["high"]), ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]))
        self.assertEqual(second["shape"]["points"], [[0.0, 0.0], [0.5, 0.0], [1.0, 1.0]])
        self.assertEqual((second["low"], second["high"]), ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]))

    def test_wide_wrapped_stroke(self):
        # Width 1.25 on a wrapped angle: the window starts half a turn before
        # the middle and the shape is read from 0.5 − 0.5/1.25 = 0.1 on.
        brightness = space(axis("angle", plane={"kind": "auto"}), [[0, 0], [0.5, 1], [1, 0]],
                           width=n(1.25), boundary="wrap",
                           offset=time([[0.0, 0.0], [1.0, 1.0]], events=ev(1)))
        nodes, _ = self.convert("color@1", color_inputs(brightness=brightness))
        self.assertAlmostEqual(nodes["curve1"]["inputs"]["low"], -0.5)
        points = nodes["curve2"]["inputs"]["shape"]["points"]
        self.assertEqual([p[0] for p in points], [0.0, 0.4, 0.8, 1.0])
        for got, want in zip([p[1] for p in points], [0.2, 1.0, 0.2, 0.2]):
            self.assertAlmostEqual(got, want)

    def test_phase_wider_than_a_turn(self):
        phase = space(axis("u"), [[0, 0], [1, 1]], gain=n(-2.0))
        nodes, notes = self.convert("aim@1", aim_inputs(horizontal=time(
            [[0.0, -1.0], [1.0, 1.0]], events=ev(4), gain=n(20), phase=phase)))
        self.assertIn(m.PHASE_WRAP, notes)
        points = nodes["curve1"]["inputs"]["shape"]["points"]
        # −2x mod 1: from 1 down to 0 at x 0.5, then again from 1 down to 0.
        self.assertEqual((points[0], points[-1]), ([0, 1.0], [1, 0.0]))
        self.assertEqual(points[2], [0.5, 1.0])

    def test_document_mode(self):
        old = {"clips": {"a": {"graph": "strobe.constant@1", "start": 0.0, "duration": 4.0, "seed": 1,
                               "selection": {"expression": "all"}, "z_index": 0, "blend_mode": "replace",
                               "inputs": {"rate": p(0.9), "fade": p(1.0)}}}}
        new = m.convert_document(old)
        self.assertEqual(new["clips"]["a"]["graph"], {"version": 1, "nodes": {
            "strobe1": node("strobe", {"rate": 0.9})}})
        self.assertEqual(m.convert_document(new), new)  # idempotent


def checker():
    binary = os.environ.get("CLIP_GRAPH_PARITY")
    if binary:
        return [binary, "--check"]
    return ["cargo", "+1.97.1", "run", "--quiet", "--release", "--manifest-path",
            str(REPO / "backend/Cargo.toml"), "-p", "luma-patterns", "--example",
            "clip_graph_parity", "--", "--check"]


class Checker(unittest.TestCase):
    """Runs after Convert: every expected graph passes the Rust checker."""

    def test_expected_graphs_pass_the_checker(self):
        self.assertTrue(CHECKED, "run the Convert tests first")
        process = subprocess.run(checker(), input="\n".join(json.dumps(c) for _, c in CHECKED) + "\n",
                                 text=True, capture_output=True, check=True)
        failures = [f"{name}: {json.loads(line)['error']}"
                    for (name, _), line in zip(CHECKED, process.stdout.splitlines())
                    if "error" in json.loads(line)]
        self.assertEqual(len(process.stdout.splitlines()), len(CHECKED))
        self.assertEqual(failures, [])


if __name__ == "__main__":
    suite = unittest.TestSuite()
    loader = unittest.TestLoader()
    suite.addTests(loader.loadTestsFromTestCase(Convert))
    suite.addTests(loader.loadTestsFromTestCase(Checker))
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
