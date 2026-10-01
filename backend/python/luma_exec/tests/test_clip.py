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

PILL = [[0, 0], [0, 1], [1, 1], [1, 0]]

# The Chase preset: a pill slides along the heads; each node's id is the
# variable it was assigned to.
CHASE_JSON = {
    "version": 2,
    "nodes": {
        "k": {"kind": "clock", "inputs": {"every": 2}},
        "t": {"kind": "time", "inputs": {"clock": {"node": "k"}}},
        "move": {"kind": "curve", "settings": {"kind": "number"},
                 "inputs": {"x": {"node": "t"}, "shape": {"points": [[0, 0], [1, 1]]},
                            "low": -0.2, "high": 1}},
        "place": {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                  "inputs": {"shift": {"node": "move"}, "length": 0.2}},
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
              {"name": "Sweep", "blend_mode": "offset", "graph": {"version": 2, "nodes": {
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
    "Wash": "color(color=(1, 1, 1))",
    "Pulse": 'beat = clock(every=1); t = time(beat); drop = curve(t, "Drop"); color(brightness=drop)',
    "Breathe": 'k = clock(every=4); t = time(k); swell = curve(t, "Swell"); color(brightness=swell)',
    "Fade": 't = time(); fade = curve(t, "Fade in"); color(alpha=fade)',
    "Color fade": 't = time(); hue = curve(t, "Ramp up", gradient=[(0, "#b0400a"), (1, "#2449eb")]); color(color=hue)',
    "Rainbow": 'k = clock(every=4); t = time(k); hue = curve(t, "Ramp up", gradient="Rainbow"); color(color=hue)',
    "Gradient": 'place = space(); hue = curve(place, "Ramp up", gradient="Sunset"); color(color=hue)',
    "Stepped palette": 'k = clock(every=4); t = time(k); hue = curve(t, "Steps 4", gradient="Rainbow"); color(color=hue)',
    "Two-color swap": 'k = clock(every=2); t = time(k); hue = curve(t, "Square", gradient=[(0, "#ff2a00"), (1, "#0040ff")]); color(color=hue)',
    "Follows a band": 'kick = audio(40, 100); level = curve(kick, "Ramp up"); color(brightness=level)',
    "VU meter": 'bass = audio(20, 250); level = curve(bass, "Ramp up"); bars = split(); height = space(bars, direction=(0, 0, 1), shift=level); meter = curve(height, "Step down"); color(brightness=meter)',
    "Random heads": 'k = clock(every=1); order = shuffle(clock=k); rank = space(order, kind="order"); half = curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)',
    "Random bars": 'k = clock(every=1); bars = group(); order = shuffle(bars, clock=k); rank = space(order, kind="order"); half = curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)',
    "Sparkle": 'k = clock(every=0.125, duration=0.5); order = shuffle(clock=k); rank = space(order, kind="order"); pick = curve(rank, [[0, 1], [0.3, 1], [0.3, 0], [1, 0]]); t = time(k); spike = curve(t, "Spike"); color(brightness=[pick, spike])',
    "Build": 'order = shuffle(); rank = space(order, kind="order"); start = curve(rank, "Ramp up"); t = time(delay=start); on = curve(t, "Step up"); color(brightness=on)',
    "Dissolve": 'order = shuffle(); rank = space(order, kind="order"); stop = curve(rank, "Ramp down"); t = time(delay=stop); off = curve(t, "Step down"); color(brightness=off)',
    "Clouds": 'cloud = noise(speed=8, scale=0.5); hue = curve(cloud, "Ramp up", gradient="Ocean"); level = curve(cloud, "Ramp up", low=0.2, high=1); color(color=hue, brightness=level)',
    "Sparkle rain": 'k = clock(every=0.25, duration=1); bars = group(); order = shuffle(bars, clock=k); columns = split(); t = time(k); fall = curve(t, "Ramp up", low=-0.3, high=1); drop = space(columns, direction=(0, 0, -1), shift=fall, length=0.3); streak = curve(drop, "Comet"); rank = space(order, kind="order"); pick = curve(rank, [[0, 0], [0, 1], [0.2, 1], [0.2, 0], [1, 0]]); color(brightness=streak, alpha=pick)',
    "Chase": 'k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)',
    "Wave": 'k = clock(every=2); place = space(); lag = curve(place, "Ramp down", low=0.5, high=1); t = time(k, phase=lag); swell = curve(t, [[0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]); color(brightness=swell)',
    "Bounce": 'k = clock(every=4); t = time(k); move = curve(t, "Triangle", low=0, high=0.8); place = space(shift=move, length=0.2); pill = curve(place, "Soft"); color(brightness=pill)',
    "Comet": 'k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); tail = curve(place, "Comet"); color(brightness=tail)',
    "Wipe": 'k = clock(every=4); place = space(); start = curve(place, "Ramp up"); t = time(k, delay=start); on = curve(t, "Step up"); color(brightness=on)',
    "Stepped chase": 'k = clock(every=4); t = time(k); move = curve(t, "Steps 4", low=0, high=0.75); place = space(shift=move, length=0.25); block = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=block)',
    "Many pills": 'k = clock(every=0.5, duration=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)',
    "Wrapping chase": 'k = clock(every=2); ring = space(wrap=True); lag = curve(ring, "Ramp down"); t = time(k, phase=lag); pill = curve(t, [[0, 0], [0.8, 0], [0.8, 1], [1, 1]]); color(brightness=pill)',
    "Colored pills": 'k = clock(every=0.5, duration=2); t = time(k); move = curve(t, "Ramp up", low=-0.25, high=1); place = space(shift=move, length=0.25); age = time(k); hue = curve(age, "Ramp up", gradient="Rainbow"); pill = curve(place, "Soft"); color(color=hue, brightness=pill)',
    "Speed-up chase": 't = time(); every = curve(t, "Ramp down", low=0.25, high=2); life = curve(t, "Ramp down", low=0.5, high=2); k = clock(every=every, duration=life); age = time(k); move = curve(age, "Ramp up", low=-0.4, high=1); size = curve(t, "Ramp down", low=0.1, high=0.4); place = space(shift=move, length=size); tail = curve(place, "Comet"); color(brightness=tail)',
    "Alternating sides": "k = clock(every=2); place = space(); lag = curve(place, [[0, 0.5], [0.5, 0.5], [0.5, 0], [1, 0]]); t = time(k, phase=lag); half = curve(t, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)",
    "Diagonal slash": 'k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.15, high=1); place = space(direction=(1, 0, 1), shift=move, length=0.15); line = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=line)',
    "Ripple": 'k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.4, high=1); radius = space(shift=move, length=0.4, kind="radial"); ring = curve(radius, "Soft"); color(brightness=ring)',
    "Wrapping ripple": 'k = clock(every=2); radius = space(kind="radial", wrap=True); lag = curve(radius, "Ramp down", low=-0.4, high=0.6); t = time(k, length=0.7143, phase=lag); ring = curve(t, [[0, 0], [0.6, 0, [0.4, 0, 0.6, 1]], [0.8, 1, [0.4, 0, 0.6, 1]], [1, 0]]); color(brightness=ring)',
    "Spin": 'k = clock(every=2); turn = space(kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.75, 0], [0.7625, 1], [1, 0]]); color(brightness=arm)',
    "Grow": 'radius = space(kind="radial"); start = curve(radius, "Ramp up"); t = time(delay=start); on = curve(t, "Step up"); color(brightness=on)',
    "Turning line": 'k = clock(every=2, duration=4); turn = space(kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.9, 0], [0.9, 1], [1, 1]]); color(brightness=arm)',
    "Spiral": 'k = clock(every=4); radius = space(kind="radial"); inner = curve(radius, "Ramp up"); outer = curve(radius, "Ramp up", low=1, high=2); turn = space(kind="angle"); lag = curve(turn, "Ramp down", low=inner, high=outer); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.7, 0, [0.4, 0, 0.6, 1]], [0.85, 1, [0.4, 0, 0.6, 1]], [1, 0]]); color(brightness=arm)',
    "Mirror": 'k = clock(every=2); halves = mirror(); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(halves, shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)',
    "Kaleidoscope": 'k = clock(every=4); sides = mirror(normal=(1, 0, 0)); quarters = mirror(sides, normal=(0, 0, 1)); turn = space(quarters, kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.85, 0], [0.8575, 1], [1, 0]]); color(brightness=arm)',
    "Slash": 'k=clock(every=2); cut=curve(time(k, delay=curve(space(direction=(-0.82,0,0.57)), "Ramp up", low=0, high=0.2)), "Step up"); d=space(direction=(0.57,0,0.82)); v=[[0,1],[0.68,0],[1,0.47]]; bloom=curve(time(k, delay=curve(d, v, low=0, high=0.9), length=curve(d, v, low=0.05, high=0.4)), "Ramp up"); fade=curve(time(k), [[0,1,"hold"],[0.2,1,"sine-out"],[1,0]]); color(brightness=[cut, bloom, fade])',
    "Position": "aim(direction=D)",
    "Fan": 'place = space(); spread = curve(place, "Ramp up", low=-25, high=25); aim(direction=D, yaw=spread)',
    "Converge": 'aim(point=(0, 3, 0), base="point")',
    "Follow": 't = time(); target = curve(t, "Ramp up", low=(-3, 3, 0), high=(3, 3, 0)); aim(point=target, base="point")',
    "Bloom": 't = time(); source = curve(t, "Ramp up", low=(0, 0, 40), high=(0, 0, 7)); aim(point=source, base="away")',
    "Sweep": 'k = clock(every=8); t = time(k); swing = curve(t, "Sine", low=-45, high=45); aim(direction=D, yaw=swing)',
    "Nod wave": 'k = clock(every=4); place = space(); lag = curve(place, "Ramp up", high=0.6); t = time(k, phase=lag); nod = curve(t, "Sine", low=-25, high=25); aim(direction=D, pitch=nod)',
    "Circle": 'k = clock(every=4); t = time(k); across = curve(t, "Cosine", low=-18, high=18); up = curve(t, "Sine", low=-18, high=18); aim(direction=D, yaw=across, pitch=up)',
    "Figure-8": 'k = clock(every=4); t = time(k); across = curve(t, "Sine", low=-25, high=25); up = curve(t, "Double sine", low=-12.5, high=12.5); aim(direction=D, yaw=across, pitch=up)',
    "Pinwheel": 'k = clock(every=4); turn = space(kind="angle"); lag = curve(turn, "Ramp up"); t = time(k, phase=lag); across = curve(t, "Cosine", low=-20, high=20); up = curve(t, "Sine", low=-20, high=20); aim(direction=D, yaw=across, pitch=up)',
    "Scissor": 'k = clock(every=4); halves = mirror(); t = time(k); swing = curve(t, "Sine", low=-30, high=30); aim(heads=halves, direction=D, yaw=swing)',
    "Ballyhoo": 'drift = noise(speed=4, scale=0.02); across = curve(drift, "Ramp up", low=-40, high=40); wander = noise(speed=4, scale=0.02); up = curve(wander, "Ramp up", low=-40, high=40); aim(direction=D, yaw=across, pitch=up)',
    "Tunnel": 'aim(point=(0, 25, 1.5), base="point")',
    "Up/down flip": 'k = clock(every=2); t = time(k); flip = curve(t, "Square", low=-30, high=30); aim(direction=D, pitch=flip)',
    "Wobble strobe": 'strobe(rate=curve(time(clock(every=0.25)), "Square"))',
    "Strobe": "strobe(rate=0.9)",
    "Ramp": 't = time(); rise = curve(t, "Ramp up"); strobe(rate=rise)',
    "Strobe follows a band": 'kick = audio(40, 100); rate = curve(kick, "Ramp up", low=0.3, high=1); strobe(rate=rate)',
    "Kick chase": 'k=clock(every=1); move=curve(time(k), "Ramp up", low=-0.2, high=1); pill=curve(space(shift=move, length=0.2), [[0,0],[0,1],[1,1],[1,0]]); color(brightness=[pill, curve(audio("Kick"), "Ramp up", low=0.2, high=1)])',
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

    def test_chase_builds_the_preset_json(self):
        k = clock(every=2)
        t = time(k)
        move = curve(t, "Ramp up", low=-0.2, high=1)
        place = space(shift=move, length=0.2)
        pill = curve(place, PILL)
        graph = color(brightness=pill)
        self.assertEqual(graph.json(), CHASE_JSON)
        self.assertEqual(json.loads(json.dumps(graph.json())), CHASE_JSON)

    def test_variable_names_are_node_ids_and_round_trip(self):
        k = clock(every=2)
        bloom_far = curve(time(k), "Ramp up")
        graph = color(brightness=[bloom_far, curve(time(k), "Spike")])
        # Named nodes keep their variable; the rest are numbered per kind.
        self.assertEqual(sorted(graph.nodes), ["bloom_far", "color1", "curve1", "k", "time1", "time2"])
        self.assertIn("bloom_far = curve(x=time1, shape='Ramp up')", graph.source())
        self.assertEqual(run(graph.source()).json(), graph.json())
        # Renaming is assigning to another variable.
        cut = bloom_far
        renamed = color(brightness=cut)
        self.assertIn("bloom_far", renamed.nodes, "the first variable bound wins")
        self.assertIn("cut", run("k = clock(every=2); cut = curve(time(k)); color(brightness=cut)").nodes)
        # A name that cannot be an id falls back to kind<n>.
        self.assertFalse(clip_module._usable("time"))
        self.assertFalse(clip_module._usable("lambda"))
        self.assertFalse(clip_module._usable("x" * 33))
        self.assertTrue(clip_module._usable("_fade2"))

    def test_space_writes_only_a_changed_shift_and_length(self):
        self.assertEqual(dict(space(shift=0, length=1).inputs), {})
        move = curve(time(), "Ramp up")
        place = space(direction=(1, 0, 0), shift=move, length=0.25, wrap=True)
        graph = color(brightness=curve(place, "Triangle"))
        self.assertIn("place = space(direction=(1, 0, 0), shift=move, length=0.25, wrap=True)", graph.source())
        self.assertEqual(run(graph.source()).json(), graph.json())

    def test_a_list_on_a_0_1_input_is_stored_as_a_list(self):
        k = clock(every=2)
        cut, fade = curve(time(k), "Step up"), curve(time(k), "Ramp down")
        graph = color(color=(1, 0.5, 0), brightness=[cut, fade, 0.5], alpha=(0.5, 0.5, 0.5))
        output = graph.nodes["color1"].inputs
        self.assertEqual(output["brightness"], [{"node": "cut"}, {"node": "fade"}, 0.5])
        # A 3-tuple is a color on color, and a list on alpha.
        self.assertEqual((output["color"], output["alpha"]), ([1, 0.5, 0], [0.5, 0.5, 0.5]))
        self.assertIn("brightness=[cut, fade, 0.5]", graph.source())
        self.assertEqual(run(graph.source()).json(), graph.json())
        self.assertEqual(Graph.from_json(graph.json()).json(), graph.json())
        rate = strobe(rate=[curve(noise(), "Ramp up"), 0.8])
        self.assertEqual(rate.nodes["strobe1"].inputs["rate"], [{"node": "curve1"}, 0.8])
        with self.assertRaisesRegex(ClipError, "graph: expected every node"):
            Graph.from_json({"version": 2, "nodes": {
                "time1": {"kind": "time"}, "color1": {"kind": "color", "inputs": {"brightness": [0.5, 0.5]}}}})

    def test_time_writes_only_changed_delay_length_and_phase(self):
        self.assertEqual(dict(time().inputs), {})
        self.assertEqual(dict(time(delay=0, length=1, phase=0).inputs), {})
        d = curve(space(), "Ramp up")
        t = time(clock(every=4), delay=d, length=0.5, phase=-0.25)
        self.assertEqual((t.inputs["delay"], t.inputs["length"], t.inputs["phase"]), (d, 0.5, -0.25))
        graph = color(brightness=curve(t, "Step up"))
        self.assertIn("t = time(clock=clock1, delay=d, length=0.5, phase=-0.25)", graph.source())
        self.assertEqual(run(graph.source()).json(), graph.json())
        self.assertEqual(graph.json()["version"], 2)

    def test_a_jump_shape_passes_through(self):
        jump = [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]
        graph = color(brightness=curve(space(), jump))
        self.assertEqual(graph.nodes["curve1"].inputs["shape"], {"points": jump})
        self.assertIn("'Step up'", color(brightness=curve(time(), [[0, 0], [0, 1], [1, 1]])).source())

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
        chase = Graph.from_json({"version": 2, "nodes": {
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
