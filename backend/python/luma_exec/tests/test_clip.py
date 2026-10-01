"""Clip graph builders, math, linking, source round trips and the clip edit calls.

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
from luma_exec.clip import (BUILDERS, ClipError, Graph, aim, audio, color, curve, group,  # noqa: E402
                            mirror, noise, preset, shuffle, space, split, strobe, time)
from luma_exec.clip import max as clip_max, min as clip_min  # noqa: E402
from luma_exec.score import GraphTrack  # noqa: E402

PILL = [[0, 0], [0, 1], [1, 1], [1, 0]]

# The Chase preset: a pill slides along the heads; each node's id is the
# variable it was assigned to.
CHASE_JSON = {
    "version": 3,
    "nodes": {
        "t": {"kind": "time", "inputs": {"every": 2}},
        "move": {"kind": "curve", "settings": {"kind": "number"},
                 "inputs": {"x": {"node": "t"}, "shape": {"points": [[0, 0], [1, 1]]},
                            "low": -0.2, "high": 1}},
        "place": {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                  "inputs": {"shift": {"node": "move"}, "scale": 0.2}},
        "pill": {"kind": "curve", "settings": {"kind": "number"},
                 "inputs": {"x": {"node": "place"}, "shape": {"points": PILL}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "pill"}}},
    },
}

SINE = [[0, 0.5, "sine-out"], [0.25, 1, "sine-in"], [0.5, 0.5, "sine-out"], [0.75, 0, "sine-in"], [1, 0.5]]
FIXTURE = {
    "curves": {
        "On": [[0, 1], [1, 1]], "Ramp up": [[0, 0], [1, 1]], "Ramp down": [[0, 1], [1, 0]],
        "Step up": [[0, 0], [0, 1], [1, 1]], "Step down": [[0, 1], [0, 0], [1, 0]],
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
              {"name": "Sweep", "blend_mode": "offset", "graph": {"version": 3, "nodes": {
                  "time1": {"kind": "time"},
                  "curve1": {"kind": "curve", "settings": {"kind": "number"},
                             "inputs": {"x": {"node": "time1"}, "low": -45, "high": 45}},
                  "aim1": {"kind": "aim", "settings": {"base": "direction"}, "inputs": {"yaw": {"node": "curve1"}}}}}}],
}
SHIPPED = Path(__file__).resolve().parents[3] / "crates/patterns/src/presets.json"


def shipped_version():
    try:
        clips = json.loads(SHIPPED.read_text())["clips"]
        return {record["graph"]["version"] for record in clips}
    except (OSError, KeyError, ValueError):
        return set()


# The two graphs Julian approved (2026-09-30), exactly as source() writes them.
EXAMPLE_SHIFT = """clip = time()
k = time(every=curve(clip, 'Ramp up', low=1, high=0.5), duration=curve(clip, 'Ramp up', low=2, high=4))
x = space(direction=(1, 0, 0), shift=curve(k, 'Ramp up'), wrap=True)
color(brightness=curve(x, [[0, 0], [0, 1], [0.25, 1], [0.25, 0], [1, 0]]))"""
EXAMPLE_SLASH = """t = time(every=2)
diag = space(direction=(-0.82, 0, 0.57), shift=curve(t, [[0, 0], [0.2, 1], [1, 1]]))
cut = curve(diag, [[0, 1], [0, 0]])
line = mirror(normal=(0.57, 0, 0.82), at=0.68)
dist = space(heads=line, direction=(0.57, 0, 0.82), scale=curve(t, 'Ramp up', low=0.04, high=0.74))
bloom = curve(dist, [[0, 1], [0.76, 1], [1, 0]])
fade = curve(t, [[0, 1, 'hold'], [0.2, 1, 'sine-out'], [1, 0]])
heat = curve(t, gradient=[(0, (1, 1, 1)), (0.2, (1, 1, 1)), (0.5, (1, 0, 0.01)), (1, (1, 0, 0.01))])
color(color=heat, brightness=cut * bloom * fade)"""


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

# The effect catalog (effect-catalog skill): every shipped preset as source()
# writes it, then a few graphs that are not presets.
CATALOG = {
    'Wash': 'color(color=(1, 1, 1))',
    'Pulse': "t = time(every=1); color(brightness=curve(t, 'Drop'))",
    'Breathe': "t = time(every=4); color(brightness=curve(t, 'Swell'))",
    'Fade': "t = time(); color(alpha=curve(t, 'Fade in'))",
    'Color fade': "t = time(); color(color=curve(t, 'Ramp up', gradient=[(0, '#b0400a'), (1, '#2449eb')]))",
    'Rainbow': "t = time(every=4); color(color=curve(t, 'Ramp up', gradient='Rainbow'))",
    'Gradient': "place = space(); color(color=curve(place, 'Ramp up', gradient='Sunset'))",
    'Stepped palette': "t = time(every=4); color(color=curve(t, 'Steps 4', gradient='Rainbow'))",
    'Two-color swap': "t = time(every=2); color(color=curve(t, 'Square', gradient=[(0, '#ff2a00'), (1, '#0040ff')]))",
    'Follows a band': "kick = audio(low_hz=40, high_hz=100); color(brightness=curve(kick, 'Ramp up'))",
    'VU meter': "bars = split(); bass = audio(low_hz=20, high_hz=250); height = space(heads=bars, direction=(0, 0, 1), shift=curve(bass, 'Ramp up')); color(brightness=curve(height, 'Step down'))",
    'Random heads': "k = time(every=1); order = shuffle(time=k); rank = space(heads=order, kind='order'); color(brightness=curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]))",
    'Random bars': "bars = group(); k = time(every=1); order = shuffle(heads=bars, time=k); rank = space(heads=order, kind='order'); color(brightness=curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]))",
    'Sparkle': "k = time(every=0.125, duration=0.5); order = shuffle(time=k); rank = space(heads=order, kind='order'); color(brightness=curve(rank, [[0, 1], [0.3, 1], [0.3, 0], [1, 0]]) * curve(k, 'Spike'))",
    'Build': "clip = time(); order = shuffle(); rank = space(heads=order, shift=curve(clip, 'Ramp up'), kind='order'); color(brightness=curve(rank, 'Step down'))",
    'Dissolve': "clip = time(); order = shuffle(); rank = space(heads=order, shift=curve(clip, 'Ramp down'), kind='order'); color(brightness=curve(rank, 'Step down'))",
    'Clouds': "cloud = noise(speed=8, scale=0.5); color(color=curve(cloud, 'Ramp up', gradient='Ocean'), brightness=curve(cloud, 'Ramp up', low=0.2, high=1))",
    'Sparkle rain': "bars = group(); columns = split(); k = time(every=0.25, duration=1); order = shuffle(heads=bars, time=k); drop = space(heads=columns, direction=(0, 0, -1), shift=curve(k, 'Ramp up', low=-0.3, high=1), scale=0.3); rank = space(heads=order, kind='order'); color(brightness=curve(drop, 'Comet') * curve(rank, [[0, 0], [0, 1], [0.2, 1], [0.2, 0], [1, 0]]))",
    'Chase': "t = time(every=2); place = space(shift=curve(t, 'Ramp up', low=-0.2, high=1), scale=0.2); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))",
    'Wave': "place = space(); t = time(every=2, phase=curve(place, 'Ramp down', low=0.5, high=1)); color(brightness=curve(t, [[0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]))",
    'Bounce': "t = time(every=4); place = space(shift=curve(t, 'Triangle', low=0, high=0.8), scale=0.2); color(brightness=curve(place, 'Soft'))",
    'Comet': "t = time(every=2); place = space(shift=curve(t, 'Ramp up', low=-0.2, high=1), scale=0.2); color(brightness=curve(place, 'Comet'))",
    'Wipe': "place = space(); t = time(every=4, delay=curve(place, 'Ramp up', low=0, high=4)); color(brightness=curve(t, 'Step up'))",
    'Stepped chase': "t = time(every=4); place = space(shift=curve(t, 'Steps 4', low=0, high=0.75), scale=0.25); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))",
    'Many pills': "t = time(every=0.5, duration=2); place = space(shift=curve(t, 'Ramp up', low=-0.2, high=1), scale=0.2); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))",
    'Wrapping chase': "ring = space(wrap=True); t = time(every=2, phase=curve(ring, 'Ramp down')); color(brightness=curve(t, [[0, 0], [0.8, 0], [0.8, 1], [1, 1]]))",
    'Colored pills': "t = time(every=0.5, duration=2); place = space(shift=curve(t, 'Ramp up', low=-0.25, high=1), scale=0.25); color(color=curve(t, 'Ramp up', gradient='Rainbow'), brightness=curve(place, 'Soft'))",
    'Speed-up chase': "clip = time(); k = time(every=curve(clip, 'Ramp down', low=0.25, high=2), duration=curve(clip, 'Ramp down', low=0.5, high=2)); place = space(shift=curve(k, 'Ramp up', low=-0.4, high=1), scale=curve(clip, 'Ramp down', low=0.1, high=0.4)); color(brightness=curve(place, 'Comet'))",
    'Alternating sides': 'place = space(); t = time(every=2, phase=curve(place, [[0, 0.5], [0.5, 0.5], [0.5, 0], [1, 0]])); color(brightness=curve(t, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]))',
    'Diagonal slash': "t = time(every=2); place = space(direction=(1, 0, 1), shift=curve(t, 'Ramp up', low=-0.15, high=1), scale=0.15); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))",
    'Ripple': "t = time(every=2); radius = space(shift=curve(t, 'Ramp up', low=-0.4, high=1), scale=0.4, kind='radial'); color(brightness=curve(radius, 'Soft'))",
    'Wrapping ripple': "t = time(every=2); radius = space(shift=curve(t, 'Ramp up', low=-0.4, high=1), kind='radial', wrap=True); color(brightness=curve(radius, [[0, 0, [0.4, 0, 0.6, 1]], [0.2, 1, [0.4, 0, 0.6, 1]], [0.4, 0], [1, 0]]))",
    'Zoom out': "clip = time(); x = space(direction=(1, 0, 0), shift=curve(clip, [[0, 0, 'ease-out'], [1, 1]]), scale=curve(clip, 'Ramp up', low=0.5, high=0.125), wrap=True); color(brightness=curve(x, [[0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]))",
    'Spin': "turn = space(kind='angle'); t = time(every=2, phase=curve(turn, 'Ramp down')); color(brightness=curve(t, [[0, 0], [0.75, 0], [0.7625, 1], [1, 0]]))",
    'Grow': "clip = time(); radius = space(shift=curve(clip, 'Ramp up'), kind='radial'); color(brightness=curve(radius, 'Step down'))",
    'Turning line': "turn = space(kind='angle'); t = time(every=2, duration=4, phase=curve(turn, 'Ramp down')); color(brightness=curve(t, [[0, 0], [0.9, 0], [0.9, 1], [1, 1]]))",
    'Spiral': "radius = space(kind='radial'); turn = space(kind='angle'); t = time(every=4, phase=curve(turn, 'Ramp down', low=curve(radius, 'Ramp up'), high=curve(radius, 'Ramp up', low=1, high=2))); color(brightness=curve(t, [[0, 0], [0.7, 0, [0.4, 0, 0.6, 1]], [0.85, 1, [0.4, 0, 0.6, 1]], [1, 0]]))",
    'Mirror': "halves = mirror(); t = time(every=2); place = space(heads=halves, shift=curve(t, 'Ramp up', low=-0.1, high=0.5), scale=0.1); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))",
    'Kaleidoscope': "sides = mirror(normal=(1, 0, 0)); quarters = mirror(heads=sides, normal=(0, 0, 1)); turn = space(heads=quarters, kind='angle'); t = time(every=4, phase=curve(turn, 'Ramp down')); color(brightness=curve(t, [[0, 0], [0.85, 0], [0.8575, 1], [1, 0]]))",
    'Position': 'aim(direction=D)',
    'Fan': "place = space(); aim(direction=D, yaw=curve(place, 'Ramp up', low=-25, high=25))",
    'Converge': "aim(point=(0, 3, 0), base='point')",
    'Follow': "t = time(); aim(point=curve(t, 'Ramp up', low=(-3, 3, 0), high=(3, 3, 0)), base='point')",
    'Bloom': "t = time(); aim(point=curve(t, 'Ramp up', low=(0, 0, 40), high=(0, 0, 7)), base='away')",
    'Tunnel': "aim(point=(0, 25, 1.5), base='point')",
    'Sweep': "t = time(every=8); aim(direction=D, yaw=curve(t, 'Sine', low=-45, high=45))",
    'Nod wave': "place = space(); t = time(every=4, phase=curve(place, 'Ramp up', high=0.6)); aim(direction=D, pitch=curve(t, 'Sine', low=-25, high=25))",
    'Circle': "t = time(every=4); aim(direction=D, yaw=curve(t, 'Cosine', low=-18, high=18), pitch=curve(t, 'Sine', low=-18, high=18))",
    'Figure-8': "t = time(every=4); aim(direction=D, yaw=curve(t, 'Sine', low=-25, high=25), pitch=curve(t, 'Double sine', low=-12.5, high=12.5))",
    'Pinwheel': "turn = space(kind='angle'); t = time(every=4, phase=curve(turn, 'Ramp up')); aim(direction=D, yaw=curve(t, 'Cosine', low=-20, high=20), pitch=curve(t, 'Sine', low=-20, high=20))",
    'Scissor': "halves = mirror(); t = time(every=4); aim(heads=halves, direction=D, yaw=curve(t, 'Sine', low=-30, high=30))",
    'Up/down flip': "t = time(every=2); aim(direction=D, pitch=curve(t, 'Square', low=-30, high=30))",
    'Ballyhoo': "drift = noise(speed=4, scale=0.02); wander = noise(speed=4, scale=0.02); aim(direction=D, yaw=curve(drift, 'Ramp up', low=-40, high=40), pitch=curve(wander, 'Ramp up', low=-40, high=40))",
    'Strobe': 'strobe(rate=0.9)',
    'Ramp': "t = time(); strobe(rate=curve(t, 'Ramp up'))",
    'Strobe follows a band': "kick = audio(low_hz=40, high_hz=100); strobe(rate=curve(kick, 'Ramp up', low=0.3, high=1))",
    # Not shipped presets.
    "Slash": EXAMPLE_SLASH,
    "Shifted chase": EXAMPLE_SHIFT,
    "Wobble strobe": "strobe(rate=curve(time(every=0.25), 'Square'))",
    "Kick chase": "t = time(every=1); place = space(shift=curve(t, 'Ramp up', low=-0.2, high=1), scale=0.2); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]) * curve(audio('Kick'), 'Ramp up', low=0.2, high=1))",
    "Named steps": "t = time(every=2); cut = curve(t, 'Step down'); fade = curve(t, 'Fade out'); glow = cut * fade; color(brightness=max(glow, 0.1), alpha=1 - curve(t, 'Ramp up'))",
}


def run(code):
    """Exec code with the bare builders; return the value of the last statement."""
    namespace = {name: getattr(clip_module, name) for name in BUILDERS}
    namespace["D"] = D
    statements = [part.strip() for part in code.replace("\n", ";").split(";") if part.strip()]
    for statement in statements[:-1]:
        exec(statement, namespace)
    return eval(statements[-1], namespace)


def maths(graph):
    return {key: node for key, node in graph.nodes.items() if node.kind == "math"}


class ClipBuilderTests(unittest.TestCase):
    def setUp(self):
        clip_module.install_presets(PRESETS)

    def test_chase_builds_the_preset_json(self):
        t = time(every=2)
        move = curve(t, "Ramp up", low=-0.2, high=1)
        place = space(shift=move, scale=0.2)
        pill = curve(place, PILL)
        graph = color(brightness=pill)
        self.assertEqual(graph.json(), CHASE_JSON)
        self.assertEqual(json.loads(json.dumps(graph.json())), CHASE_JSON)

    def test_builders_write_version_3_inputs(self):
        self.assertEqual(dict(time().inputs), {})
        lag = curve(space(), "Ramp up")
        t = time(every=2, duration=4, delay=-0.5, phase=lag)
        self.assertEqual(dict(t.inputs), {"every": 2, "duration": 4, "delay": -0.5, "phase": lag})
        order = shuffle(group(), time=t)
        place = space(order, direction=(1, 0, 0), shift=0.25, scale=0.5, kind="order")
        line = mirror(normal=(0, 0, 1), at=0.68)
        graph = color(brightness=curve(place, "Ramp up") * curve(space(line), "Ramp down"))
        nodes = graph.json()["nodes"]
        self.assertEqual(graph.json()["version"], 3)
        self.assertEqual(nodes["order"]["inputs"], {"heads": {"node": "group1"}, "time": {"node": "t"}})
        self.assertEqual(nodes["place"]["inputs"]["scale"], 0.5)
        self.assertNotIn("length", nodes["place"]["inputs"])
        self.assertEqual(nodes["line"]["inputs"], {"normal": [0, 0, 1], "at": 0.68})
        self.assertEqual(nodes["math1"], {"kind": "math", "settings": {"op": "*"},
                                          "inputs": {"values": [{"node": "curve1"}, {"node": "curve2"}]}})
        self.assertEqual(run(graph.source()).json(), graph.json())

    def test_variable_names_are_node_ids_and_round_trip(self):
        t = time(every=2)
        bloom_far = curve(t, "Ramp up")
        graph = color(brightness=bloom_far * curve(t, "Spike"))
        # Named nodes keep their variable; the rest are numbered per kind.
        self.assertEqual(sorted(graph.nodes), ["bloom_far", "color1", "curve1", "math1", "t"])
        self.assertIn("bloom_far = curve(t, 'Ramp up')", graph.source())
        self.assertEqual(run(graph.source()).json(), graph.json())
        # Renaming is assigning to another variable.
        cut = bloom_far
        renamed = color(brightness=cut)
        self.assertIn("bloom_far", renamed.nodes, "the first variable bound wins")
        self.assertIn("cut", run("t = time(every=2); cut = curve(t); color(brightness=cut)").nodes)
        # A name that cannot be an id falls back to kind<n>; clock is free now.
        for name in ("time", "lambda", "x" * 33, "math", "max", "min"):
            self.assertFalse(clip_module._usable(name), name)
        self.assertTrue(clip_module._usable("_fade2"))
        self.assertTrue(clip_module._usable("clock"))

    def test_a_chain_of_one_operator_is_one_math_node(self):
        t = time(every=2)
        a, b, c = curve(t, "Ramp up"), curve(t, "Ramp down"), curve(t, "Spike")
        graph = color(brightness=a * b * c)
        (math,) = maths(graph).values()
        self.assertEqual(math.inputs["values"], [{"node": "a"}, {"node": "b"}, {"node": "c"}])
        self.assertIn("color(brightness=a * b * c)", graph.source())
        # Brackets on the right merge too: the order of the items stays.
        self.assertEqual(color(brightness=a * (b * c)).json(), graph.json())
        # A named step is its own node and keeps its name.
        ab = a * b
        named = color(brightness=ab * c)
        self.assertEqual(sorted(maths(named)), ["ab", "math1"])
        self.assertEqual(named.nodes["math1"].inputs["values"], [{"node": "ab"}, {"node": "c"}])
        self.assertIn("ab = a * b\n", named.source())
        # A step that two inputs read is one shared node.
        shared = a * b
        both = color(brightness=shared * c, alpha=shared * 0.5)
        self.assertEqual(len(maths(both)), 3)
        # max and min chain the same way; another operator does not merge.
        self.assertEqual(len(maths(color(brightness=clip_max(clip_max(a, b), c)))), 1)
        self.assertEqual(len(maths(color(brightness=a * b + c))), 2)
        for code in ("a * b * c", "a * (b + c)", "a - b - c", "a - (b - c)", "a - (b + c)", "a + b - c",
                     "0.5 * a", "a * 0.5", "1 - a", "a * -0.5", "max(a, b, c)", "min(a, 0.2)",
                     "max(a * b, c) * 0.5", "sum([a, b, c])"):
            with self.subTest(code):
                graph = run(f"t = time(every=2); a = curve(t, 'Ramp up'); b = curve(t, 'Ramp down'); "
                            f"c = curve(t, 'Spike'); color(brightness={code})")
                self.assertEqual(run(graph.source()).json(), graph.json())
                if code != "sum([a, b, c])":
                    self.assertTrue(graph.source().endswith(f"color(brightness={code})"), graph.source())

    def test_math_items_and_builtins(self):
        a, b = curve(time(), "Ramp up"), curve(space(), "Ramp up")
        self.assertEqual(color(brightness=0.5 * a).nodes["math1"].inputs["values"], [0.5, {"node": "a"}])
        self.assertEqual(color(brightness=a * 0.5).nodes["math1"].inputs["values"], [{"node": "a"}, 0.5])
        minus = color(brightness=1 - a).nodes["math1"]
        self.assertEqual((minus.settings["op"], minus.inputs["values"]), ("-", [1, {"node": "a"}]))
        self.assertEqual([len(node.inputs["values"]) for node in maths(color(brightness=a - b - 0.1)).values()], [2, 2])
        self.assertEqual(color(brightness=clip_max([a, b])).nodes["math1"].settings, {"op": "max"})
        # On plain numbers max and min stay Python's own.
        self.assertEqual((clip_max(1, 3, 2), clip_min([4, 2]), clip_max([], default=7)), (3, 2, 7))
        self.assertEqual(clip_max("ab", key=str.upper), "b")

    def test_coordinates_and_lists_are_not_values(self):
        for code in ("time() * 2", "2 * time()", "curve(time()) + space()", "max(curve(time()), noise())"):
            with self.subTest(code), self.assertRaisesRegex(ClipError, r"curve\(\.\.\.\)"):
                run(code)
        with self.assertRaisesRegex(ClipError, "is not a value"):
            curve(time()) * mirror()
        with self.assertRaisesRegex(ClipError, r"a \* b"):
            color(brightness=[curve(time()), curve(space())])
        with self.assertRaisesRegex(ClipError, r"a \* b"):
            color(brightness=[0.5, 0.5])
        with self.assertRaisesRegex(ClipError, r"a \* b"):
            strobe(rate=[0.5, curve(time())])
        with self.assertRaises(TypeError):
            curve(time()) * "x"

    def test_the_approved_examples_write_back_exactly(self):
        for code in (EXAMPLE_SHIFT, EXAMPLE_SLASH):
            with self.subTest(code.splitlines()[0]):
                graph = run(code)
                self.assertEqual(graph.source(), code)
                self.assertEqual(run(graph.source()).json(), graph.json())
                loaded = Graph.from_json(json.loads(json.dumps(graph.json())))
                self.assertEqual(run(loaded.source()).json(), graph.json())
        # Inline curves are numbered as they are read; the three named curves
        # and the one product keep their own lines.
        slash = run(EXAMPLE_SLASH)
        self.assertEqual(sorted(key for key in slash.nodes if slash.nodes[key].kind in ("curve", "math")),
                         ["bloom", "curve1", "curve2", "cut", "fade", "heat", "math1"])
        self.assertEqual(Graph.from_json(run(EXAMPLE_SHIFT).json()).source(), EXAMPLE_SHIFT)

    def test_source_writes_shared_and_named_curves_on_lines(self):
        t = time(every=2)
        shared = curve(t, "Ramp up")
        graph = aim(direction=D, yaw=shared, pitch=shared)
        lines = graph.source().splitlines()
        self.assertEqual(lines[-1], "aim(direction=(0, 0.766, -0.643), yaw=shared, pitch=shared)")
        unnamed = run("t = time(every=2); aim(yaw=curve(t, 'Sine'), pitch=curve(t, 'Sine'))")
        self.assertEqual(len(unnamed.source().splitlines()), 2)
        twice = Graph.from_json({"version": 3, "nodes": {
            "time1": {"kind": "time", "inputs": {}},
            "curve1": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}}},
            "aim1": {"kind": "aim", "settings": {"base": "direction"},
                     "inputs": {"yaw": {"node": "curve1"}, "pitch": {"node": "curve1"}}}}})
        self.assertIn("curve1 = curve(time1)", twice.source())
        self.assertEqual(run(twice.source()).json(), twice.json())

    def test_stored_numbering_survives_source(self):
        # Numbers that are not creation order, and a stored chain of one
        # operator: source() keeps every id and every node.
        stored = {"version": 3, "nodes": {
            "time1": {"kind": "time", "inputs": {"every": 2}},
            "curve1": {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0, 0], [1, 1]]}}},
            "curve5": {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0, 1], [1, 0]]}}},
            "curve2": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}}},
            "math2": {"kind": "math", "settings": {"op": "*"},
                      "inputs": {"values": [{"node": "curve1"}, {"node": "curve2"}]}},
            "math1": {"kind": "math", "settings": {"op": "*"},
                      "inputs": {"values": [{"node": "curve5"}, {"node": "math2"}]}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "math1"}}}}}
        graph = Graph.from_json(stored)
        self.assertEqual(graph.json(), normal(stored))
        self.assertEqual(run(graph.source()).json(), graph.json())

    def test_a_jump_shape_passes_through(self):
        jump = [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]
        graph = color(brightness=curve(space(), jump))
        self.assertEqual(graph.nodes["curve1"].inputs["shape"], {"points": jump})
        self.assertIn("'Step up'", color(brightness=curve(time(), [[0, 0], [0, 1], [1, 1]])).source())

    def test_reusing_a_variable_links_one_node(self):
        t = time(every=4)
        graph = aim(direction=D, yaw=curve(t, "Cosine", low=-18, high=18), pitch=curve(t, "Sine", low=-18, high=18))
        kinds = [node.kind for node in graph.nodes.values()]
        self.assertEqual(kinds.count("time"), 1)
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
                self.assertEqual(again.source(), graph.source())
                self.assertEqual(graph.source().splitlines()[-1].split("(")[0], graph.output.kind)

    def test_json_round_trips_through_from_json(self):
        for name, code in CATALOG.items():
            with self.subTest(name):
                graph = run(code)
                loaded = Graph.from_json(copy.deepcopy(graph.json()))
                self.assertEqual(loaded.json(), graph.json())
                self.assertEqual(run(loaded.source()).json(), graph.json())

    def test_loaded_ids_stay_when_new_nodes_wire_in(self):
        chase = Graph.from_json({"version": 3, "nodes": {
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
        with self.assertRaises(TypeError):
            time(clock=None)
        with self.assertRaisesRegex(ClipError, "unknown curve preset 'Nope'"):
            curve(time(), "Nope")

    def test_a_wrong_wire_is_left_to_the_checker(self):
        graph = color(brightness=time())
        self.assertEqual(graph.nodes["color1"].inputs["brightness"], {"node": "time1"})

    def test_definition_defaults_build(self):
        for kind, inputs in clip_module._INPUTS.items():
            if kind == "math":
                continue  # operators, not a builder
            with self.subTest(kind):
                node = getattr(clip_module, kind)(**{name: None for name in inputs})
                graph = node if isinstance(node, Graph) else color(brightness=node) if kind == "curve" else None
                if graph is not None:
                    self.assertIn(kind, [record.kind for record in graph.nodes.values()])

    def test_preset_inherits_its_name_and_is_a_copy(self):
        graph = preset("Chase")
        self.assertEqual(graph.name, "Chase")
        self.assertEqual(graph.json(), CHASE_JSON)
        self.assertEqual(preset("chase").name, "Chase")
        with self.assertRaisesRegex(ClipError, "unknown clip preset"):
            preset("Nothing")

    @unittest.skipUnless(shipped_version() == {3}, "presets.json is not version 3 yet (the patterns crate ships it)")
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
                          nodes={"time": {"kind": "time", "inputs": {"every": {"type": "number", "default": None}}}},
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
        edit.add_clip(run(EXAMPLE_SLASH), name="Slash", beats=(8, 16))
        edit.apply()
        circle, slash = sorted(track.clips, key=lambda clip: clip.start)
        self.assertEqual(circle.name, "Circle")
        self.assertEqual(run(circle.graph.source()).json(), run(CATALOG["Circle"]).json())
        self.assertEqual(run(slash.graph.source()).json(), run(EXAMPLE_SLASH).json())

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
        self.assertIs(namespace.clip.max, clip_max)


if __name__ == "__main__":
    unittest.main()
