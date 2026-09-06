#!/usr/bin/env python3
"""Manual EBF graph recipes and migration verification against recorded output.

This tool only calls local Rust binaries. It never starts a model turn. The
original library remains in its independent backup; generated score sources
and comparisons are written to the nominated migration directory for review.
"""
from __future__ import annotations

import argparse
import bisect
import copy
from dataclasses import dataclass
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sqlite3
from typing import Any

VENUE = "99a8a8e9-2bc4-4fac-82d3-33b4cb9e6a4f"
OWNER = "b54e7669-fd0e-40fe-a1d8-f9286f49c7ba"
LINEAR_CHASES = {"major_axis_chase", "x_chase_bounce", "y_chase", "y_chase_bounce", "z_chase", "z_chase_bounce"}


@dataclass(frozen=True)
class Port:
    node: str
    key: str


@dataclass(frozen=True)
class Input:
    key: str
    default: Any
    label: str


def typed(kind, value):
    return {"type": kind, "value": value}


class Recipe:
    """A small spelling of the real graph schema, validated by the Rust host."""

    def __init__(self, library, name):
        self.library = library
        self.name = name
        self.nodes = {}
        self.inputs = {}

    def node(self, name, definition, **values):
        if name in self.nodes:
            raise ValueError(f"duplicate node {name}")
        spec = self.library[definition]
        bindings = {}
        for key, value in values.items():
            port = spec["inputs"][key]
            if isinstance(value, Port):
                bindings[key] = {"source": "connection", "node": value.node, "output": value.key}
            elif isinstance(value, Input):
                exposed = copy.deepcopy(port)
                exposed.update(name=value.label, default=typed(port["value_type"], value.default))
                old = self.inputs.setdefault(value.key, exposed)
                if old["value_type"] != exposed["value_type"]:
                    raise ValueError(f"conflicting input {value.key}")
                if exposed["rate"] == "fixed":
                    old["rate"] = "fixed"
                bindings[key] = {"source": "input", "input": value.key}
            else:
                bindings[key] = {"source": "value", "value": typed(port["value_type"], value)}
        self.nodes[name] = {"definition": definition, "inputs": bindings}
        outputs = spec["outputs"]
        return Port(name, next(iter(outputs))) if len(outputs) == 1 else name

    def number(self, name, value):
        return self.node(name, "core/number_field", value=value)

    def field(self, name, value, kind="number"):
        return self.node(name, f"core/{kind}_field", value=value)

    def math(self, name, operation, a, b):
        return self.node(name, f"core/{operation}", a=a, b=b)

    def appearance(self, mask):
        return self.node("output", "appearance", mask=mask, color=Input("color", [1., 1., 1.], "Color"))

    def finish(self, output):
        spec = self.library[self.nodes[output.node]["definition"]]["outputs"][output.key]
        return {"name": self.name, "inputs": self.inputs, "outputs": {"lighting": copy.deepcopy(spec)},
                "body": {"kind": "graph", "body": {"nodes": self.nodes,
                    "outputs": {"lighting": {"source": "connection", "node": output.node, "output": output.key}}}}}


def mapping(kind, reverse=False):
    source = {"kind": kind}
    if kind == "circle":
        source["origin"] = 0.
    return {"source": source, "reverse": reverse, "per_group": False}


def color(value):
    # The legacy evaluator used c=3: alpha never reached apply_color.
    if isinstance(value, str):
        if value.startswith("#"):
            return [int(value[i:i+2], 16) / 255 for i in (1, 3, 5)]
        value = json.loads(value)
    return [value[key] / 255 for key in ("r", "g", "b")]


def curve(x, amount):
    x = max(0., min(1., x))
    if abs(amount) < .001:
        return x
    power = 1 + abs(amount) * 5
    return x ** power if amount > 0 else 1 - (1 - x) ** power


def sampled_envelope(function, anchors=()):
    """Keep actual stage boundaries, then refine only where curvature needs it."""
    points = sorted(set([0., 1., *(float(x) for x in anchors if 0 < x < 1)]))
    values = {x: max(0., min(1., function(x))) for x in points}
    while len(points) < 256:
        worst = (0., None)
        for a, b in zip(points, points[1:]):
            for weight in (.25, .5, .75):
                x = a + (b-a) * weight
                error = abs(function(x) - (values[a] + (values[b]-values[a]) * weight))
                if error > worst[0]:
                    worst = error, x
        if worst[0] <= .0002:
            break
        x = worst[1]
        values[x] = max(0., min(1., function(x)))
        bisect.insort(points, x)
    else:
        raise ValueError("envelope approximation exceeds the canonical knot budget")
    return {"points": [[x, values[x]] for x in points]}


def envelope(params):
    weights = [min(1., max(0., params.get(k, 0.))) for k in ("attack", "decay", "sustain", "release")]
    total = sum(weights)
    if not total:
        return {"points": [[0., 0.], [1., 0.]]}
    attack, decay, sustain, release = [x / total for x in weights]
    level = params.get("sustain_level", 0.)
    def sample(t):
        if t < attack:
            return curve(t / attack, params.get("attack_curve", 0.))
        if t < attack + decay:
            return level + (1-level) * curve(1-(t-attack)/decay, params.get("decay_curve", 0.))
        if t < attack + decay + sustain:
            return level
        if release:
            return level * max(0., 1-(t-attack-decay-sustain)/release)
        # The clock gates the stroke's end. Preserve an attack's limiting peak.
        return level if sustain or decay else 1.
    return sampled_envelope(sample, [attack, attack+decay, attack+decay+sustain])


def spatial_profile(record):
    params = next(n["params"] for n in record["graph"]["nodes"] if n["typeId"] == "falloff")
    # Width and shape remain independently editable. Discard the old sampled
    # Invert extrema: the intended stroke has a lit center and dark edges.
    shape = sampled_envelope(lambda x: 1 - curve(abs(2*x-1), params["curve"]), [.5])
    width = (1 if record["name"].startswith("circle_pill") else 2) / max(params["width"], 1e-6)
    return width, shape


def beat_at(grid, seconds):
    beats = grid["beats"]
    def raw(t):
        i = min(max(0, bisect.bisect_right(beats, t)-1), len(beats)-2)
        return i + (t-beats[i])/(beats[i+1]-beats[i])
    origin = grid["downbeats"][0] if grid["downbeats"] else grid["downbeatOffset"]
    return raw(seconds) - raw(origin)


class Host:
    """Direct dispatch RPC. Only explicit local read/validate/preview/import commands."""
    ALLOWED = {"get_pattern_node_library", "preview_composable_pattern", "score_dsl_export", "score_dsl_validate", "score_dsl_import", "preview_score_clip", "get_fixture_facings"}

    def __init__(self, binary, config, log, *, fixture=True):
        self.log = open(log, "w")
        command = [str(binary), "--config-dir", str(config)]
        if fixture:
            command += ["--fixture-principal", OWNER]
        self.process = subprocess.Popen(command,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, text=True)
        self.serial = 0

    def call(self, command, **args):
        if command not in self.ALLOWED:
            raise ValueError(f"command outside migration scope: {command}")
        self.serial += 1
        self.process.stdin.write(json.dumps({"id": self.serial, "cmd": command, "args": args}) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError("local host exited; inspect the migration host log")
        result = json.loads(line)
        if result["id"] != self.serial:
            raise RuntimeError("unexpected local response identity")
        if "err" in result:
            raise RuntimeError(result["err"])
        return result["ok"]

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=30)
        self.log.close()


def stock(library, definition, ports):
    g = Recipe(library, "")
    arguments = {key: Input(key, copy.deepcopy(library[definition]["inputs"][key]["default"]["value"]),
                            library[definition]["inputs"][key]["name"]) for key in ports}
    output = g.node("effect", definition, **arguments)
    return g.finish(output)


def make_recipe(library, name, record):
    """Translate known authored ideas explicitly; never transplant the old graph."""
    if name == "solid_color":
        return stock(library, "wash", ["color"])
    if name == "soild_strobe":
        return stock(library, "write_strobe", ["value"])
    if name == "gradient":
        return stock(library, "gradient", ["gradient"])
    if name == "chord_color":
        return stock(library, "harmony_color", [])
    if name == "rainbow":
        return stock(library, "rainbow", ["repeat"])
    if name == "random_dimmer_mask":
        return stock(library, "random_heads", ["repeat", "delay", "grid_aligned", "count", "color"])
    if name == "smooth_dimmer_noise":
        return stock(library, "noise_wash", ["period", "scale"])
    if name == "kick_intensity":
        return stock(library, "drum_pulse", ["duration", "shape", "color"])

    g = Recipe(library, {
        "intensity_spikes": "Pulse", "bass_strobe": "Bass strobe",
        "250_500hz_bass_pulse": "Bass pulse", "left_right_alternating_mask": "Alternating sides",
        "two_color_alternating_wash": "Alternating colors", "circle_pill": "Circle chase",
        "circle_pill_step": "Stepped circle chase", "outward_circle_pulse": "Outward pulse",
    }.get(name, "Chase"))
    repeat = Input("repeat", 1., "Repeat (beats)")
    delay = Input("delay", 0., "Phase delay (beats)")
    if name == "intensity_spikes":
        light = g.node("pulse", "pulse", repeat=repeat, travel=repeat, delay=delay, grid_aligned=True,
                       shape=Input("shape", {"points": [[0., 1.], [1., 0.]]}, "Fade shape"),
                       color=Input("color", [1., 1., 1.], "Color"))
        return g.finish(light)

    if name in {"bass_strobe", "250_500hz_bass_pulse"}:
        bass = name != "bass_strobe"
        energy = g.node("energy", "band_energy", source=Input("source", "bass" if bass else "mix", "Audio source"),
                        low_hz=250. if bass else 20., high_hz=500. if bass else 60.)
        energy = g.field("energy_field", energy)
        energy = g.node("sensitivity", "remap_field", value=energy,
                        low=Input("quiet", 0., "Quiet level"), high=Input("full", 1., "Full level"))
        mask = g.node("coverage", "core/clamp_coverage", value=energy)
        values = g.node("level", "core/mask_values", mask=mask)
        threshold = g.math("gate", "greater", values, g.number("threshold", .04 if bass else .36))
        if bass:
            mask = g.node("gated", "multiply_mask", a=mask, b=threshold)
        light = g.appearance(mask)
        if not bass:
            gated = g.node("rate", "scale_mask", mask=threshold, amount=Input("rate", .9, "Strobe rate"))
            strobe = g.node("strobe", "core/strobe_output", mask=gated)
            light = g.node("combined", "add_lighting", a=light, b=strobe)
        return g.finish(light)

    if name in {"circle_pill", "circle_pill_step", "two_color_alternating_wash"}:
        clock = g.node("clock", "rhythm", repeat=repeat, grid_aligned=False)
        mapped = g.node("mapping", "mapped_position", mapping=Input("mapping", mapping("circle"), "Mapping"))
        if name == "two_color_alternating_wash":
            ranks = g.node("order", "core/rank", value=mapped)
            cycle = g.field("cycle", Port(clock, "cycle"))
            position = g.math("advance", "add", ranks, cycle)
            position = g.math("pairs", "divide", position, g.number("two", 2.))
            position = g.node("wrap", "core/fraction", value=position)
            gate = g.math("alternate", "greater", position, g.number("threshold", .25))
            position = g.node("values", "core/mask_values", mask=gate)
            default_gradient = {"stops": [{"t": 0., "color": [0., 0., 0.]}, {"t": 1., "color": [1., 1., 1.]}]}
            color_field = g.node("gradient", "sample_field_gradient", position=position,
                                 gradient=Input("gradient", default_gradient, "Colors"))
            return g.finish(g.node("output", "core/color_output", color=color_field))
        elapsed = g.field("elapsed", Port(clock, "elapsed"), "beats")
        period = g.field("period", repeat, "beats")
        phase = g.math("phase", "divide", elapsed, period)
        if name == "circle_pill_step":
            four = g.number("steps", 4.)
            phase = g.math("scale_steps", "multiply", phase, four)
            phase = g.node("step", "core/floor", value=phase)
            phase = g.math("stepped", "divide", phase, four)
        else:
            mapped = g.math("copies", "multiply", mapped, g.number("count", Input("count", 1., "Strokes")))
        delta = g.math("offset", "subtract", mapped, phase)
        half = g.number("half", .5)
        delta = g.math("center", "add", delta, half)
        delta = g.node("wrap", "core/fraction", value=delta)
        delta = g.math("nearest", "subtract", delta, half)
    elif name == "left_right_alternating_mask":
        clock = g.node("clock", "rhythm", repeat=repeat, delay=delay, grid_aligned=True)
        elapsed = g.field("elapsed", Port(clock, "elapsed"), "beats")
        phase = g.math("phase", "divide", elapsed, g.field("period", repeat, "beats"))
        half = g.number("half", .5-1e-10)
        beat_side = g.math("beat_side", "greater", phase, half)
        position = g.node("mapping", "mapped_position", mapping=mapping("u"))
        fixture_side = g.math("fixture_side", "greater", position, half)
        a = g.node("side", "core/mask_values", mask=fixture_side)
        b = g.node("beat", "core/mask_values", mask=beat_side)
        delta = g.math("offset", "subtract", a, b)
        delta = g.node("alternate", "core/absolute", value=delta)
        return g.finish(g.appearance(g.node("mask", "core/clamp_coverage", value=delta)))
    else:
        allowed = LINEAR_CHASES | {"outward_circle_pulse"}
        if name not in allowed:
            raise ValueError(f"no reviewed manual recipe for {name}")
        clock = g.node("clock", "rhythm", repeat=repeat, delay=delay, grid_aligned=True)
        motion = g.node("motion", "motion", elapsed=Port(clock, "elapsed"), repeat=repeat,
                        travel=Input("travel", 1., "Travel time (beats)"),
                        end=Input("end", 1., "End position"), path=Input("path", {"points": [[0., 0.], [1., 1.]]}, "Travel curve"))
        position = g.field("position", Port(motion, "position"), "position")
        if name == "outward_circle_pulse":
            mapped = g.node("radius", "radial_distance")
            mapped = g.node("mapping", "normalize_field", value=mapped)
        else:
            axis = "u" if name.startswith("x_") else "z" if name.startswith("z_") else "v"
            mapped = g.node("mapping", "mapped_position", mapping=Input("mapping", mapping(axis, reverse=axis == "v"), "Mapping"))
        delta = g.math("offset", "subtract", mapped, position)
    mask = g.node("shape", "profile_mask", offset=delta, width=Input("width", .5, "Stroke width"),
                  shape=Input("shape", {"points": [[0., 1.], [1., 0.]]}, "Stroke shape"))
    if name in LINEAR_CHASES:
        mask = g.node("rest", "scale_mask", mask=mask, amount=Port(motion, "active"))
    return g.finish(g.appearance(mask))


def pulse_settings(record):
    args, grid = record["args"], record["beat_grid"]
    subdivision = args.get("subdivision", args.get("subdivison", 1.))
    if record["name"] == "left_right_alternating_mask":
        subdivision /= 2
    repeat = 1. if abs(subdivision) < .001 else abs(1 / subdivision)
    phase = beat_at(grid, grid["beats"][0])
    if abs(subdivision) < 1:
        phase += repeat * .5
    phase += args.get("offset", args.get("beat_offset", 0.))
    params = next((n["params"] for n in record["graph"]["nodes"] if n["typeId"] in {"beat_envelope", "adsr"}), {})
    if params.get("anticipate", False):
        total = sum(min(1., max(0., params.get(k, 0.))) for k in ("attack", "decay", "sustain", "release"))
        phase -= repeat * params.get("attack", 0.) / total if total else 0.
    return repeat, phase % repeat


def clip_inputs(record):
    name, args = record["name"], record["args"]
    values = {}
    if "color" in args and name != "soild_strobe":
        values["color"] = color(args["color"])
    if name in {"gradient", "two_color_alternating_wash"}:
        values["gradient"] = {"stops": [{"t": s["t"], "color": color(s["color"])} for s in args["gradient"]["stops"]]}
    if name == "soild_strobe":
        return {"value": args["rate"]}
    if name == "chord_color":
        return {}
    if name == "smooth_dimmer_noise":
        return {"period": 2., "scale": 2.}
    if name == "kick_intensity":
        params = next(n["params"] for n in record["graph"]["nodes"] if n["typeId"] == "adsr")
        # The legacy ADSR used a fixed 120 BPM internally: length .5 meant .25s.
        seconds = params["length_beats"] / 2
        values.update(duration=beat_at(record["beat_grid"], record["start"]+seconds)-beat_at(record["beat_grid"], record["start"]),
                      shape=envelope(params))
        return values
    if name in {"bass_strobe", "250_500hz_bass_pulse"}:
        calibration = next(c for c in record["calibration"] if c["kind"] == "normalize")
        values.update(quiet=calibration["min"], full=calibration["max"])
        if name == "250_500hz_bass_pulse" and "bass_range" in record:
            values.update(quiet=0., full=record["bass_range"]["full"])
        values["source"] = "bass" if name == "250_500hz_bass_pulse" else "mix"
        if name == "bass_strobe":
            values["rate"] = args["rate"]
        return values
    if name in {"solid_color", "gradient"}:
        return values
    repeat, delay = pulse_settings(record)
    values["repeat"] = repeat
    if name == "rainbow":
        return {"repeat": repeat}
    if name not in {"circle_pill", "circle_pill_step", "two_color_alternating_wash"}:
        values["delay"] = delay
    if name == "random_dimmer_mask":
        values.update(count=args["count"], grid_aligned=True)
        return values
    if name == "left_right_alternating_mask" or name == "two_color_alternating_wash":
        return values
    if name == "circle_pill":
        values["count"] = args["count"]
    if name.startswith("circle_pill"):
        values["width"], values["shape"] = spatial_profile(record)
        return values
    params = next(n["params"] for n in record["graph"]["nodes"] if n["typeId"] == "beat_envelope")
    if name == "intensity_spikes":
        values["shape"] = envelope(params)
        return values
    values.update(travel=repeat, end=params.get("amplitude", 1.), path=envelope(params))
    values["width"], values["shape"] = spatial_profile(record)
    if name in LINEAR_CHASES:
        axis = "u" if name.startswith("x_") else "z" if name.startswith("z_") else "v"
        values["mapping"] = mapping(axis, reverse=axis == "v")
    if name == "major_axis_chase":
        # This graph was mislabeled: its old attribute was rel_y. Use the
        # actual fitted axis, oriented toward upstage, as the name intended.
        values["mapping"] = {"source": {"kind": "major_axis", "toward": [0., 1., 0.]}, "reverse": False, "per_group": False}
    if name == "y_chase":
        values.update(travel=repeat*.5, path={"points": [[0., 0.], [1., 1.]]})
    return values


def translate(library, records, original):
    scores, scopes = {}, {}
    for record in records:
        if record["venue_id"] != VENUE:
            raise ValueError("reference capture belongs to a different venue")
        score_id, clip_id = record["score_id"], record["clip_id"]
        row = original[clip_id]
        if row["score_id"] != score_id:
            raise ValueError("original clip scope changed after capture")
        score = scores.setdefault(score_id, {"version": 2, "definitions": {}, "clips": {}})
        scopes[score_id] = {"score_id": score_id, "track_id": record["track_id"], "venue_id": VENUE}
        name = record["name"]
        definition = "ebf/" + ("chase" if name in LINEAR_CHASES else name.replace("soild_strobe", "strobe"))
        if definition not in score["definitions"]:
            score["definitions"][definition] = make_recipe(library, name, record)
        specs = score["definitions"][definition]["inputs"]
        inputs = {key: typed(specs[key]["value_type"], value) for key, value in clip_inputs(record).items()}
        start = beat_at(record["beat_grid"], row["start_time"])
        end = beat_at(record["beat_grid"], row["end_time"])
        selection = copy.deepcopy(record["args"]["selection"])
        selection.pop("spatialReference", None)
        # New randomness has an authored clip seed. Reusing this clip preserves
        # its look; an independent copy can intentionally choose another seed.
        seed = int.from_bytes(hashlib.sha256(clip_id.encode()).digest()[:8], "big")
        score["clips"][clip_id] = {"graph": definition, "start": start, "duration": end-start,
            "seed": seed, "selection": selection, "z_index": row["z_index"],
            "blend_mode": record["blend_mode"], "inputs": inputs}
    return scores, scopes


def compare(old, new):
    if len(old) != len(new):
        raise ValueError("preview frame count changed")
    energy_errors, strobe_errors = [], []
    for a, b in zip(old, new):
        if a["primitives"].keys() != b["primitives"].keys():
            raise ValueError("resolved head identities changed")
        for key, x in a["primitives"].items():
            y = b["primitives"][key]
            energy_errors.extend(abs(x["dimmer"]*x["color"][i] - y["dimmer"]*y["color"][i]) for i in range(3))
            strobe_errors.append(abs(x["strobe"] - y["strobe"]))
    return {"mean_rgb_error": sum(energy_errors)/len(energy_errors), "max_rgb_error": max(energy_errors),
            "mean_strobe_error": sum(strobe_errors)/len(strobe_errors), "max_strobe_error": max(strobe_errors)}


def behavior_changes(record):
    name = record["name"]
    notes = []
    if name in LINEAR_CHASES | {"intensity_spikes", "outward_circle_pulse", "left_right_alternating_mask"}:
        notes.append("Each repeat follows its own beat-grid interval; no shortest-gap timing or average-BPM offset.")
    if name in {"circle_pill", "circle_pill_step", "rainbow", "two_color_alternating_wash", "gradient"}:
        notes.append("Clip-relative progress follows the actual beat grid instead of average BPM or elapsed seconds.")
    if name in LINEAR_CHASES | {"circle_pill", "circle_pill_step", "outward_circle_pulse"}:
        notes.append("Stroke width and Envelope are independent controls; lit center/dark edges replace sampled Invert extrema.")
    if name == "major_axis_chase":
        notes.append("The named major axis is now a fitted principal axis instead of the old rel_y attribute.")
    if name == "two_color_alternating_wash":
        notes.append("Alternation follows individual heads around the solved circle; nearby pixels are no longer silently merged.")
    if name in {"random_dimmer_mask", "smooth_dimmer_noise"}:
        notes.append("Deterministic per-head seeds replace the undocumented legacy random stream.")
    if name == "kick_intensity":
        notes.append("Fade length is musical; a new kick restarts its envelope instead of inheriting an earlier kick's tail.")
    if name == "250_500hz_bass_pulse" and record["effective_audio_source"] == "mix":
        notes.append("Uses saved bass audio; the missing local PCM cache no longer silently selects the full mix.")
    if name == "250_500hz_bass_pulse":
        notes.append("Sensitivity is set from the actual bass source, with zero as silence and a recorded full level.")
    return notes


def bass_sensitivity(host, library, records, original):
    """Author explicit sensitivity from the real source, not the old fallback."""
    recipe = Recipe(library, "Bass calibration")
    energy = recipe.node("energy", "band_energy", source="bass", low_hz=250., high_hz=500.)
    mask = recipe.node("coverage", "core/clamp_coverage", value=recipe.field("level", energy))
    definition = recipe.finish(recipe.appearance(mask))
    readings = {}
    for record in records:
        if record["name"] != "250_500hz_bass_pulse":
            continue
        row = original[record["clip_id"]]
        preview = host.call("preview_composable_pattern", request={
            "venueId": VENUE, "trackId": record["track_id"], "definition": "migration/bass_probe",
            "library": {"definitions": {**library, "migration/bass_probe": definition}},
            "inputs": {}, "targets": [record["args"]["selection"]],
            "times": record["times"], "clipStart": row["start_time"], "clipEnd": row["end_time"],
        })
        levels = sorted(next(iter(frame["primitives"].values()))["dimmer"] for frame in preview["frames"])
        # The 95th percentile gives brief peaks headroom without allowing a
        # single spike to make the whole phrase dim. Never amplify silence.
        full = max(levels[round((len(levels)-1)*.95)], 1e-5)
        record["bass_range"] = {"full": full, "maximum": max(levels), "samples": len(levels)}
        readings[record["clip_id"]] = record["bass_range"]
    return readings


def apply_reviewed(options):
    """Apply inspected sources through normal authored revisions and CAS."""
    report = json.loads((options.apply_reviewed / "report.json").read_text())
    if report["clip_count"] != 1225 or len(report["comparisons"]) != 1225:
        raise ValueError("the complete EBF comparison must finish before import")
    sources = {}
    for score_id, digest in report["source_sha256"].items():
        path = options.apply_reviewed / "sources" / f"{score_id}.luma"
        source = path.read_text()
        if hashlib.sha256(source.encode()).hexdigest() != digest:
            raise ValueError(f"reviewed source changed: {score_id}")
        sources[score_id] = source
    with sqlite3.connect(f"file:{options.config / 'luma.db'}?mode=ro", uri=True) as db:
        db.row_factory = sqlite3.Row
        rows = [dict(row) for row in db.execute("SELECT id, track_id, uid FROM scores WHERE venue_id=? AND uid=?", (VENUE, OWNER))]
    if len(rows) != 38 or any(row["uid"] != OWNER for row in rows):
        raise ValueError("EBF score count or ownership changed; inspect before importing")
    host = Host(options.host, options.config, options.output / "host.log", fixture=not options.live)
    try:
        # The regular venue-open path performs this existing, idempotent stage
        # upgrade. It must precede score preparation's read transaction.
        host.call("get_fixture_facings", venue_id=VENUE)
        (options.output / "before").mkdir()
        prepared = []
        for row in rows:
            score_id = row["id"]
            scope = {"score_id": score_id, "track_id": row["track_id"], "venue_id": VENUE}
            before = host.call("score_dsl_export", **scope, include_clip_ids=True)
            (options.output / "before" / f"{score_id}.json").write_text(json.dumps(before, indent=2))
            source = sources.get(score_id)
            if source is None:
                if before["clipCount"] != 0:
                    raise ValueError(f"previously empty score now contains clips: {score_id}")
                source = json.dumps({"version": 2, "definitions": {}, "clips": {}})
            elif before["clipCount"] != len(json.loads(source)["clips"]):
                raise ValueError(f"score clip count changed: {score_id}")
            elif hashlib.sha256(before["source"].encode()).hexdigest() != report["original_source_sha256"][score_id]:
                try:
                    already_imported = same_document(json.loads(before["source"]), json.loads(source))
                except json.JSONDecodeError:
                    already_imported = False
                if not already_imported:
                    raise ValueError(f"score changed after reference review: {score_id}")
            checked = host.call("score_dsl_validate", **scope, source=source)
            if not checked["valid"]:
                raise ValueError(f"{score_id}: {checked['diagnostics']}")
            prepared.append((scope, source, before["revision"]))
        applied = []
        for scope, source, revision in prepared:
            digest = hashlib.sha256(source.encode()).hexdigest()
            result = host.call("score_dsl_import", **scope, source=source, base_revision=revision,
                               operation_id=f"ebf-graph-reset-{scope['score_id']}-{digest}")
            exported = host.call("score_dsl_export", **scope, include_clip_ids=True)
            if not same_document(json.loads(exported["source"]), json.loads(source)):
                raise ValueError(f"import did not round-trip: {scope['score_id']}")
            applied.append({**scope, "revision": result["revisionId"], "clips": exported["clipCount"]})
            (options.output / "applied.json").write_text(json.dumps(applied, indent=2))
            print(f"Imported {scope['score_id']}: {exported['clipCount']} clips", flush=True)
    finally:
        host.close()


def same_document(a, b):
    """Rust/Python decimal parsers can differ by an f64 ULP; IDs and seeds cannot."""
    if isinstance(a, dict) and isinstance(b, dict):
        if {"body", "inputs", "outputs"} <= a.keys() & b.keys():
            # Definition labels are optional and empty names serialize away.
            a, b = {"name": "", **a}, {"name": "", **b}
        return a.keys() == b.keys() and all(same_document(a[k], b[k]) for k in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(same_document(x, y) for x, y in zip(a, b))
    if type(a) in (int, float) and type(b) in (int, float):
        if type(a) is int and type(b) is int:
            return a == b
        if abs(a) >= 2**53 or abs(b) >= 2**53:
            return type(a) is type(b) and a == b
        return math.isclose(a, b, rel_tol=1e-14, abs_tol=1e-15)
    return type(a) is type(b) and a == b


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--references", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--compare", action="store_true")
    parser.add_argument("--apply-reviewed", type=Path, help="import a completed comparison directory")
    parser.add_argument("--live", action="store_true", help="use the regular library's stored identity, never fixture admission")
    options = parser.parse_args()
    options.output.mkdir(parents=True, exist_ok=False)
    if options.apply_reviewed:
        apply_reviewed(options)
        return
    if options.live:
        raise ValueError("generate and compare only against the independent library copy")
    manifest = json.loads((options.references / "manifest.json").read_text())
    records = [json.loads((options.references / item["file"]).read_text()) for item in manifest]
    effective_sources = json.loads((options.references / "effective-audio-sources.json").read_text())
    for record in records:
        if record["name"] == "250_500hz_bass_pulse":
            record["effective_audio_source"] = effective_sources[record["clip_id"]]["source"]
    if len(records) != 1225 or len({r["name"] for r in records}) != 22:
        raise ValueError("expected the complete reviewed EBF capture (1,225 clips, 22 effects)")
    with sqlite3.connect(f"file:{options.config / 'luma.db'}?mode=ro", uri=True) as db:
        db.row_factory = sqlite3.Row
        original = {r["id"]: dict(r) for r in db.execute("SELECT c.* FROM track_scores c JOIN scores s ON s.id=c.score_id WHERE s.venue_id=?", (VENUE,))}
    if original.keys() != {r["clip_id"] for r in records}:
        raise ValueError("library and reference clip identities differ")
    host = Host(options.host, options.config, options.output / "host.log")
    try:
        library = host.call("get_pattern_node_library")["definitions"]
        # Selection is the canonical expression/subset schema at the preview
        # boundary; legacy spatialReference is deliberately not a new control.
        for record in records:
            record["args"]["selection"].pop("spatialReference", None)
        readings = bass_sensitivity(host, library, records, original)
        (options.output / "bass-sensitivity.json").write_text(json.dumps(readings, indent=2))
        scores, scopes = translate(library, records, original)
        (options.output / "sources").mkdir()
        original_hashes = {}
        for score_id, score in scores.items():
            before = host.call("score_dsl_export", **scopes[score_id], include_clip_ids=True)
            original_hashes[score_id] = hashlib.sha256(before["source"].encode()).hexdigest()
            source = json.dumps(score, indent=2, allow_nan=False) + "\n"
            (options.output / "sources" / f"{score_id}.luma").write_text(source)
            checked = host.call("score_dsl_validate", **scopes[score_id], source=source)
            (options.output / f"{score_id}-validation.json").write_text(json.dumps(checked, indent=2))
            if not checked["valid"]:
                raise ValueError(f"{score_id}: {checked['diagnostics']}")
            print(f"Validated {score_id}: {len(score['clips'])} clips", flush=True)
        comparisons = []
        if options.compare:
            (options.output / "previews").mkdir()
            for record in records:
                score = scores[record["score_id"]]
                clip = score["clips"][record["clip_id"]]
                preview = host.call("preview_composable_pattern", request={
                    "venueId": VENUE, "trackId": record["track_id"], "definition": clip["graph"],
                    "library": {"definitions": {**library, **score["definitions"]}},
                    "inputs": clip["inputs"], "targets": [clip["selection"]], "times": record["times"],
                    "clipStart": original[record["clip_id"]]["start_time"],
                    "clipEnd": original[record["clip_id"]]["end_time"], "seed": clip["seed"],
                })
                (options.output / "previews" / f"{record['clip_id']}.json").write_text(json.dumps(preview))
                if preview["writes"] != record["writes"]:
                    raise ValueError(f"output capabilities changed for {record['clip_id']}")
                comparison = {"clip": record["clip_id"], "effect": record["name"],
                              "intentional_changes": behavior_changes(record), **compare(record["frames"], preview["frames"])}
                comparisons.append(comparison)
                print(json.dumps(comparison), flush=True)
        source_hashes = {path.stem: hashlib.sha256(path.read_bytes()).hexdigest() for path in (options.output / "sources").glob("*.luma")}
        report = {"venue": VENUE, "score_count": len(scores), "clip_count": len(records), "comparisons": comparisons,
                  "source_sha256": source_hashes}
        report["original_source_sha256"] = original_hashes
        (options.output / "report.json").write_text(json.dumps(report, indent=2))
    finally:
        host.close()


if __name__ == "__main__":
    main()
