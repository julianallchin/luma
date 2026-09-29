"""Clip graph builders, linking, source round trips and the clip edit calls.

The Rust checker owns type checking; these tests cover only what Python
writes and reads.
"""
import copy
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec import clip as clip_module  # noqa: E402
from luma_exec.clip import (BUILDERS, ClipError, Graph, aim, audio, clock, color, curve, group,  # noqa: E402
                            mirror, noise, preset, shuffle, space, split, strobe, time)
from luma_exec.score import GraphTrack  # noqa: E402

# The section 4 example, verbatim: the contract for Chase.
CHASE_JSON = {
    "version": 1,
    "nodes": {
        "clock1": {"kind": "clock", "inputs": {"every": 2}},
        "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}}},
        "curve1": {"kind": "curve", "settings": {"kind": "number"},
                   "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0, 0], [1, 1]]}, "low": -0.2, "high": 1}},
        "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                   "inputs": {"offset": {"node": "curve1"}, "width": 0.2}},
        "curve2": {"kind": "curve", "settings": {"kind": "number"},
                   "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}}},
        "color1": {"kind": "color", "inputs": {"color": [1, 1, 1], "brightness": {"node": "curve2"}}},
    },
}

SINE = [[0, 0.5, "sine-out"], [0.25, 1, "sine-in"], [0.5, 0.5, "sine-out"], [0.75, 0, "sine-in"], [1, 0.5]]
FIXTURE = {
    "curves": {
        "On": [[0, 1], [1, 1]], "Ramp up": [[0, 0], [1, 1]], "Ramp down": [[0, 1], [1, 0]],
        "Triangle": [[0, 0], [0.5, 1], [1, 0]],
        "Soft": [[0, 0, [0.4, 0, 0.6, 1]], [0.5, 1, [0.4, 0, 0.6, 1]], [1, 0]],
        "Comet": [[0, 0], [0.95, 1], [1, 0]], "Spike": [[0, 0], [0.15, 1], [1, 0]],
        "Drop": [[0, 1, "hold"], [0.5, 1], [1, 0]], "Swell": [[0, 0, "ease-in-out"], [0.5, 1, "ease-in-out"], [1, 0]],
        "Fade in": [[0, 0, "ease-in"], [1, 1]], "Fade out": [[0, 1, "ease-out"], [1, 0]],
        "Square": [[0, 1, "hold"], [0.5, 0], [1, 0]], "Sine": SINE,
        "Cosine": [[0, 1, "sine-in"], [0.25, 0.5, "sine-out"], [0.5, 0, "sine-in"], [0.75, 0.5, "sine-out"], [1, 1]],
        "Double sine": [[0, 0.5], [0.125, 1], [0.375, 0], [0.625, 1], [0.875, 0], [1, 0.5]],
        "Steps 4": [[0, 0, "hold"], [0.25, 1 / 3, "hold"], [0.5, 2 / 3, "hold"], [0.75, 1, "hold"], [1, 1]],
    },
    "gradients": {name: [{"t": 0, "color": [0, 0, 0]}, {"t": 1, "color": [1, 1, 1 - index / 10]}]
                  for index, name in enumerate(["Rainbow", "Warm", "Cool", "Fire", "Ocean", "Sunset", "B/W"])},
    "bands": {"Kick": [40, 100], "Bass": [20, 250], "Mids": [250, 4000], "Highs": [4000, 16000], "Full": [20, 16000]},
}
# The binding's shape: lists of named records, as presets.json ships them.
PRESETS = {
    "curves": [{"name": name, "curve": {"points": points}} for name, points in FIXTURE["curves"].items()],
    "gradients": [{"name": name, "gradient": {"stops": stops}} for name, stops in FIXTURE["gradients"].items()],
    "bands": [{"name": name, "low_hz": low, "high_hz": high} for name, (low, high) in FIXTURE["bands"].items()],
    "clips": [{"name": "Chase", "blend_mode": "replace", "graph": CHASE_JSON},
              {"name": "Sweep", "blend_mode": "offset", "graph": {"version": 1, "nodes": {
                  "time1": {"kind": "time"},
                  "curve1": {"kind": "curve", "settings": {"kind": "number"},
                             "inputs": {"x": {"node": "time1"}, "low": -45, "high": 45}},
                  "aim1": {"kind": "aim", "settings": {"base": "direction"}, "inputs": {"yaw": {"node": "curve1"}}}}}}],
}
SHIPPED = Path(__file__).resolve().parents[3] / "crates/patterns/src/presets.json"


def close(a, b):
    """Equal JSON, with numbers equal to 1e-6."""
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(close(a[key], b[key]) for key in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(close(x, y) for x, y in zip(a, b))
    if isinstance(a, (int, float)) and isinstance(b, (int, float)) and not isinstance(a, bool):
        return abs(a - b) <= 1e-6
    return a == b


def normal(graph):
    """Stored JSON with an absent inputs map written as empty."""
    return {"version": graph["version"], "nodes": {
        key: dict(node, inputs=node.get("inputs", {})) for key, node in graph["nodes"].items()}}

D = (0, 0.766, -0.643)

# Section 9, one entry per effect, as an agent would write it.
CATALOG = {
    "Wash": "color(color=(1,1,1))",
    "Pulse": 'color(brightness=curve(time(clock(every=1)), "Drop"))',
    "Breathe": 'color(brightness=curve(time(clock(every=4)), "Swell"))',
    "Fade": 'color(alpha=curve(time(), "Fade in"))',
    "Color fade": 'color(color=curve(time(), "Ramp up", gradient=[(0,"#b0400a"),(1,"#2449eb")]))',
    "Rainbow": 'color(color=curve(time(clock(every=4)), "Ramp up", gradient="Rainbow"))',
    "Gradient": 'color(color=curve(space(), "Ramp up", gradient="Sunset"))',
    "Stepped palette": 'color(color=curve(time(clock(every=4)), "Steps 4", gradient="Rainbow"))',
    "Two-color swap": 'color(color=curve(time(clock(every=2)), "Square", gradient=[(0,"#ff2a00"),(1,"#0040ff")]))',
    "Follows a band": 'color(brightness=curve(audio(40, 100), "Ramp up"))',
    "VU meter": 'color(brightness=curve(space(split(), direction=(0,0,1), offset=0, width=curve(audio(20,250), "Ramp up")), "On"))',
    "Random heads": 'k=clock(every=1); color(brightness=curve(space(shuffle(clock=k), kind="order", offset=0, width=0.5), "On"))',
    "Random bars": 'k=clock(every=1); color(brightness=curve(space(shuffle(group(), k), kind="order", offset=0, width=0.5), "On"))',
    "Sparkle": 'k=clock(every=0.125, duration=0.5); color(brightness=curve(space(shuffle(clock=k), kind="order", offset=0, width=0.3), "On"), alpha=curve(time(k), "Spike"))',
    "Build": 'color(brightness=curve(space(shuffle(), kind="order", offset=0, width=curve(time(), "Ramp up")), "On"))',
    "Clouds": 'n=noise(speed=8, scale=0.5); color(color=curve(n, "Ramp up", gradient="Ocean"), brightness=curve(n, "Ramp up", low=0.2, high=1))',
    "Sparkle rain": 'k=clock(every=0.25, duration=1); fall=curve(space(split(), direction=(0,0,-1), offset=curve(time(k), "Ramp up", low=-0.3, high=1), width=0.3), "Comet"); color(brightness=fall, alpha=curve(space(shuffle(group(), k), kind="order", offset=0, width=0.2), "On"))',
    "Chase": 'k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))',
    "Wave": 'k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-1, high=1), width=1), "Soft"))',
    "Bounce": 'k=clock(every=4); color(brightness=curve(space(offset=curve(time(k), "Triangle", low=0, high=0.8), width=0.2), "Soft"))',
    "Wipe": 'k=clock(every=4); color(brightness=curve(space(offset=0, width=curve(time(k), "Ramp up")), "On"))',
    "Stepped chase": 'k=clock(every=4); color(brightness=curve(space(offset=curve(time(k), "Steps 4", low=0, high=0.75), width=0.25), "On"))',
    "Many pills": 'k=clock(every=0.5, duration=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))',
    "Wrapping chase": 'k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up"), width=0.2, wrap=True), "On"))',
    "Colored pills": 'k=clock(every=0.5, duration=2); s=space(offset=curve(time(k), "Ramp up", low=-0.25, high=1), width=0.25); color(color=curve(time(k), "Ramp up", gradient="Rainbow"), brightness=curve(s, "Soft"))',
    "Speed-up chase": 't=time(); k=clock(every=curve(t, "Ramp down", low=0.25, high=2), duration=curve(t, "Ramp down", low=0.5, high=2)); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.4, high=1), width=curve(t, "Ramp down", low=0.1, high=0.4)), "Comet"))',
    "Alternating sides": 'k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Square", low=0, high=0.5), width=0.5), "On"))',
    "Diagonal slash": 'k=clock(every=2); color(brightness=curve(space(direction=(1,0,1), offset=curve(time(k), "Ramp up", low=-0.15, high=1), width=0.15), "On"))',
    "Ripple": 'k=clock(every=2); color(brightness=curve(space(kind="radial", offset=curve(time(k), "Ramp up", low=-0.4, high=1), width=0.4), "Soft"))',
    "Wrapping ripple": 'k=clock(every=2); color(brightness=curve(space(kind="radial", offset=curve(time(k), "Ramp up", low=-0.4, high=1), width=0.4, wrap=True), "Soft"))',
    "Spin": 'k=clock(every=2); color(brightness=curve(space(kind="angle", offset=curve(time(k), "Ramp up"), width=0.25), "Comet"))',
    "Grow": 'color(brightness=curve(space(kind="radial", offset=0, width=curve(time(), "Ramp up")), "On"))',
    "Turning line": 'k=clock(every=2, duration=4); color(brightness=curve(space(kind="angle", offset=curve(time(k), "Ramp up"), width=0.1), "On"))',
    "Spiral": 'k=clock(every=4); t=time(k, phase=curve(space(kind="radial"), "Ramp up")); color(brightness=curve(space(kind="angle", offset=curve(t, "Ramp up"), width=0.3), "Soft"))',
    "Mirror": 'm=mirror(); k=clock(every=2); color(brightness=curve(space(m, offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))',
    "Kaleidoscope": 'm=mirror(mirror(normal=(1,0,0)), normal=(0,0,1)); k=clock(every=4); color(brightness=curve(space(m, kind="angle", offset=curve(time(k), "Ramp up"), width=0.15), "Comet"))',
    "Position": "aim(direction=D)",
    "Fan": 'aim(direction=D, yaw=curve(space(), "Ramp up", low=-25, high=25))',
    "Converge": 'aim(base="point", point=(0, 3, 0))',
    "Follow": 'aim(base="point", point=curve(time(), "Ramp up", low=(-3,3,0), high=(3,3,0)))',
    "Bloom": 'aim(base="away", point=curve(time(), "Ramp up", low=(0,0,40), high=(0,0,7)))',
    "Sweep": 'aim(direction=D, yaw=curve(time(clock(every=8)), "Sine", low=-45, high=45))',
    "Nod wave": 'k=clock(every=4); aim(direction=D, pitch=curve(time(k, phase=curve(space(), "Ramp up", high=0.6)), "Sine", low=-25, high=25))',
    "Circle": 't=time(clock(every=4)); aim(direction=D, yaw=curve(t, "Cosine", low=-18, high=18), pitch=curve(t, "Sine", low=-18, high=18))',
    "Figure-8": 't=time(clock(every=4)); aim(direction=D, yaw=curve(t, "Sine", low=-25, high=25), pitch=curve(t, "Double sine", low=-12.5, high=12.5))',
    "Pinwheel": 't=time(clock(every=4), phase=curve(space(kind="angle"), "Ramp up")); aim(direction=D, yaw=curve(t, "Cosine", low=-20, high=20), pitch=curve(t, "Sine", low=-20, high=20))',
    "Scissor": 'm=mirror(); aim(heads=m, direction=D, yaw=curve(time(clock(every=4)), "Sine", low=-30, high=30))',
    "Ballyhoo": 'aim(direction=D, yaw=curve(noise(speed=4, scale=0.02), "Ramp up", low=-40, high=40), pitch=curve(noise(speed=4, scale=0.02), "Ramp up", low=-40, high=40))',
    "Tunnel": 'aim(base="point", point=(0, 25, 1.5))',
    "Up/down flip": 'aim(direction=D, pitch=curve(time(clock(every=2)), "Square", low=-30, high=30))',
    "Dissolve": 'color(brightness=curve(space(shuffle(), kind="order", offset=0, width=curve(time(), "Ramp down")), "On"))',
    "Comet": 'k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "Comet"))',
    "Wobble strobe": 'strobe(rate=curve(time(clock(every=0.25)), "Square"))',
    "Strobe": "strobe(rate=0.9)",
    "Ramp": 'strobe(rate=curve(time(), "Ramp up"))',
    "Strobe follows a band": 'strobe(rate=curve(audio(40, 100), "Ramp up", low=0.3, high=1))',
    "Kick chase": 'k=clock(every=1); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=curve(audio("Kick"), "Ramp up", low=0.05, high=0.4)), "On"))',
}


def run(code):
    """Exec code with the bare builders; return the value of the last statement."""
    namespace = {name: getattr(clip_module, name) for name in BUILDERS}
    namespace["D"] = D
    statements = [part.strip() for part in code.replace("\n", ";").split(";") if part.strip()]
    for statement in statements[:-1]:
        exec(statement, namespace)
    return eval(statements[-1], namespace)


class ClipBuilderTests(unittest.TestCase):
    def setUp(self):
        clip_module.install_presets(PRESETS)

    def test_chase_builds_the_section_4_json(self):
        k = clock(every=2)
        pos = curve(time(k), "Ramp up", low=-0.2, high=1)
        graph = color(color=(1, 1, 1), brightness=curve(space(offset=pos, width=0.2), "On"))
        self.assertEqual(graph.json(), CHASE_JSON)
        self.assertEqual(json.loads(json.dumps(graph.json())), CHASE_JSON)

    def test_reusing_a_variable_links_one_node(self):
        t = time(clock(every=4))
        graph = aim(direction=D, yaw=curve(t, "Cosine", low=-18, high=18), pitch=curve(t, "Sine", low=-18, high=18))
        kinds = [node.kind for node in graph.nodes.values()]
        self.assertEqual(kinds.count("time"), 1)
        self.assertEqual(kinds.count("clock"), 1)
        wires = {node.inputs["x"]["node"] for node in graph.nodes.values() if node.kind == "curve"}
        self.assertEqual(len(wires), 1)
        # Two calls with the same arguments are two nodes: two noise streams.
        two = run(CATALOG["Ballyhoo"])
        self.assertEqual([node.kind for node in two.nodes.values()].count("noise"), 2)

    def test_source_round_trips_every_catalog_effect(self):
        for name, code in CATALOG.items():
            with self.subTest(name):
                graph = run(code)
                again = run(graph.source())
                self.assertEqual(again.json(), graph.json())
                self.assertEqual(graph.source().splitlines()[-1].split("(")[0], graph.output.kind)

    def test_json_round_trips_through_from_json(self):
        for name, code in CATALOG.items():
            with self.subTest(name):
                graph = run(code)
                loaded = Graph.from_json(copy.deepcopy(graph.json()))
                self.assertEqual(loaded.json(), graph.json())
                self.assertEqual(run(loaded.source()).json(), graph.json())

    def test_loaded_ids_stay_when_new_nodes_wire_in(self):
        chase = Graph.from_json({"version": 1, "nodes": {
            "time4": {"kind": "time", "inputs": {}},
            "curve7": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time4"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve7"}}}}})
        graph = color(brightness=chase.node("curve7"), alpha=curve(time(), "Fade in"))
        self.assertEqual(sorted(graph.nodes), ["color1", "curve1", "curve7", "time1", "time4"])

    def test_values_are_plain(self):
        graph = color(color="#ff0000", brightness=curve(time(), [[0, 0], [1, 1, ]], low=0, high=0.5))
        node = graph.nodes["color1"]
        self.assertEqual(len(node.inputs["color"]), 3)
        self.assertLess(node.inputs["color"][1], 0.1)
        self.assertEqual(graph.nodes["curve1"].inputs["shape"], {"points": [[0, 0], [1, 1]]})
        self.assertEqual(graph.nodes["curve1"].settings["kind"], "number")
        vector = curve(time(), low=(0, 0, 1), high=(0, 1, 0))
        self.assertEqual(vector.settings["kind"], "vector")
        self.assertEqual(curve(time(), gradient="Fire").settings["kind"], "color")
        self.assertEqual(audio("kick").inputs, {"low_hz": 40, "high_hz": 100})
        self.assertEqual(space(kind="angle").settings["wrap"], "yes")
        self.assertEqual(space().settings, {"kind": "line", "wrap": "no"})
        with self.assertRaises(TypeError):
            color(fade=0.5)
        with self.assertRaisesRegex(ClipError, "unknown curve preset 'Nope'"):
            curve(time(), "Nope")

    def test_a_wrong_wire_is_left_to_the_checker(self):
        graph = color(brightness=time())
        self.assertEqual(graph.nodes["color1"].inputs["brightness"], {"node": "time1"})

    def test_definition_defaults_build(self):
        for kind, inputs in clip_module._INPUTS.items():
            with self.subTest(kind):
                built = getattr(clip_module, kind)(**{name: None for name in inputs})
                graph = built if isinstance(built, Graph) else color(brightness=built) if kind == "curve" else None
                if graph is not None:
                    self.assertEqual(graph.nodes[f"{kind}1"].kind, kind)

    def test_preset_inherits_its_name_and_is_a_copy(self):
        graph = preset("Chase")
        self.assertEqual(graph.name, "Chase")
        self.assertEqual(graph.json(), CHASE_JSON)
        self.assertEqual(preset("chase").name, "Chase")
        with self.assertRaisesRegex(ClipError, "unknown clip preset"):
            preset("Nothing")

    @unittest.skipUnless(SHIPPED.exists(), "bug: presets.json is missing from the patterns crate")
    def test_every_shipped_preset_round_trips_through_source(self):
        shipped = json.loads(SHIPPED.read_text())
        clip_module.install_presets(shipped)
        try:
            self.assertEqual(len(clip_module.Presets().clips), len(shipped["clips"]))
            for record in shipped["clips"]:
                with self.subTest(record["name"]):
                    graph = preset(record["name"])
                    self.assertEqual((graph.name, graph.blend), (record["name"], record["blend_mode"]))
                    self.assertEqual(graph.json(), normal(record["graph"]))
                    self.assertEqual(run(graph.source()).json(), normal(record["graph"]))
            for name, code in CATALOG.items():
                if name in clip_module._PRESETS["clips"]:
                    with self.subTest(f"catalog {name}"):
                        # presets.json rounds hex colors to 9 places.
                        self.assertTrue(close(run(code).json(), preset(name).json()), name)
        finally:
            clip_module.install_presets(PRESETS)


class ClipEditTests(unittest.TestCase):
    def setUp(self):
        clip_module.install_presets(PRESETS)
        self.calls = []
        self.refuse = None

    def track(self):
        def call(method, payload):
            self.calls.append((method, copy.deepcopy(payload)))
            if method == "track.clip_check":
                if self.refuse:
                    return {"ok": False, "error": f"clip {payload['clip']['name']} ({payload['id']}): {self.refuse}"}
                return {"ok": True}
            if method == "track.score_apply":
                return payload["candidate"]
            if method == "track.score_render":
                return self.render
            return {"ok": True}
        return GraphTrack({"id": "track", "title": "Test", "editable": True, "beat_origin_s": 1.5,
                           "document": {"clips": {}}},
                          nodes={"clock": {"kind": "clock", "inputs": {"every": {"type": "number", "default": None}}}},
                          features={"beats": [1., 1.5, 2., 3., 4.], "downbeats": [1.5, 5.]}, host_call=call)

    def test_add_clip_without_a_name_raises_before_the_host(self):
        edit = self.track().edit()
        with self.assertRaisesRegex(ClipError, "clip: expected a name; got none"):
            edit.add_clip(color(), beats=(0, 4))
        self.assertEqual(self.calls, [])
        self.assertEqual(edit.clips, ())

    def test_a_preset_clip_inherits_the_name(self):
        edit = self.track().edit()
        first = edit.add_clip(preset("Chase"), beats=(0, 4))
        second = edit.add_clip("Chase", beats=(4, 8), name="Other")
        self.assertEqual((first.name, second.name), ("Chase", "Other"))
        stored = edit.candidate["clips"][first.id]
        self.assertEqual(stored["graph"], CHASE_JSON)
        self.assertEqual(stored["name"], "Chase")
        self.assertEqual(first.graph, preset("Chase"))
        self.assertEqual(first.blend, "replace")
        sweep = edit.add_clip("Sweep", beats=(0, 4))
        self.assertEqual((sweep.name, sweep.blend), ("Sweep", "offset"))
        self.assertEqual(edit.add_clip("Sweep", beats=(0, 4), blend="replace").blend, "replace")

    def test_add_and_update_check_the_one_clip(self):
        edit = self.track().edit()
        clip = edit.add_clip(color(), name="Wash", beats=(0, 4), id="wash")
        self.assertEqual(self.calls[-1][0], "track.clip_check")
        self.assertEqual(self.calls[-1][1]["clip"]["name"], "Wash")
        self.refuse = "color1.brightness: expected a number 0–1 (share) or a number curve; got a coordinate wire from time1. Example: brightness=curve(time1, \"Ramp up\")"
        with self.assertRaises(ClipError) as caught:
            edit.update_clip(clip, graph=color(brightness=time()))
        self.assertTrue(str(caught.exception).startswith("clip Wash (wash): color1.brightness"))
        self.assertEqual(edit.candidate["clips"]["wash"]["graph"], color().json())
        self.refuse = None
        updated = edit.update_clip(clip, name="Glow", graph=color(alpha=0.5), blend="add")
        self.assertEqual((updated.name, updated.blend), ("Glow", "add"))
        self.assertEqual(updated.graph.output.inputs["alpha"], 0.5)

    def test_clips_read_back_with_source(self):
        track = self.track()
        edit = track.edit()
        edit.add_clip(run(CATALOG["Circle"]), name="Circle", beats=(0, 8), blend="offset")
        edit.apply()
        (clip,) = track.clips
        self.assertEqual(clip.name, "Circle")
        self.assertEqual(run(clip.graph.source()).json(), run(CATALOG["Circle"]).json())

    def test_window_output_splits_the_lighting_channels(self):
        import numpy as np
        edit = self.track().edit()
        values = np.zeros((2, 3, 12), dtype=np.float32)
        values[:, :, 0] = 0.5     # red, already darkened by the dimmer
        values[:, :, 3] = 0.5     # dimmer
        values[:, :, 6] = 0.25    # strobe
        values[:, :, 9] = 1.0     # aim v
        values[:, :, 11] = 0.75   # weight
        self.render = {"values": values, "lightIds": ["a", "b"], "timesS": [0, 0.5, 1],
                       "channels": ["r", "g", "b", "dimmer", "pan", "tilt", "strobe", "speed",
                                    "aim_u", "aim_v", "aim_z", "aim_weight"]}
        output = edit.window(beats=(0, 2)).output
        self.assertEqual(output.values.shape, (2, 3, 3))
        self.assertTrue(np.allclose(output.values[..., 0], 0.5))
        self.assertEqual(output.aim.values.shape, (2, 3, 3))
        self.assertTrue(np.allclose(output.aim.values[..., 1], 1.0))
        self.assertTrue(np.allclose(output.aim.weight, 0.75))
        self.assertTrue(np.allclose(output.strobe.values, 0.25))
        self.render = {"values": np.ones((2, 3, 3), dtype=np.float32)}
        rgb = edit.window(beats=(0, 2)).output
        self.assertEqual(rgb.values.shape, (2, 3, 3))
        with self.assertRaisesRegex(Exception, "RGB only"):
            rgb.aim

    def test_namespace_exposes_presets_and_builders(self):
        import tempfile
        from luma_exec.bindings import build_namespace
        namespace = build_namespace({"schema_version": 1, "revision": "r", "agent_kind": "track_copilot",
                                     "scope": {}, "root": {"presets": PRESETS}},
                                    Path(tempfile.mkdtemp(prefix="luma-clip-")))
        self.assertEqual(namespace.presets.clips["Chase"].json(), CHASE_JSON)
        self.assertEqual(namespace.presets.bands["Kick"], (40.0, 100.0))
        self.assertIs(namespace.clip.curve, curve)


if __name__ == "__main__":
    unittest.main()
