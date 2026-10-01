#!/usr/bin/env python3
"""One-time source conversion. Never called while loading a score.

JSON mode reads a document on stdin and writes converted JSON on stdout.
The comparison runner uses the same converter before any database writes.
"""
import copy
import json
import sys


def number(value):
    return {"type": "number", "value": value}


def events(every, life=None):
    result = {"every": source(every)}
    if life is not None:
        result["life"] = source(life)
    return result


def scale(value, factor):
    if value["type"] in ("number", "proportion", "beats", "position"):
        return number(value["value"] * factor)
    value = copy.deepcopy(value)
    kind = value["type"]
    if kind in ("time", "space", "audio"):
        value["value"]["gain"] = scale(value["value"].get("gain", number(1)), factor)
    elif kind == "noise":
        value["value"]["range"] = [scale(v, factor) for v in value["value"]["range"]]
    elif kind == "random":
        value["value"]["level"] = scale(value["value"].get("level", number(1)), factor)
    else:
        raise ValueError(f"cannot scale {kind}")
    return value


def space(axis, curve, **kwargs):
    return {"type": "space", "value": {"axis": axis, "curve": {"points": curve}, **kwargs}}


def source(value, clock=None, key=None):
    value = copy.deepcopy(value)
    kind, body = value.get("type"), value.get("value")
    if kind in ("time", "hit"):
        value["type"] = "time"
        body["events"] = copy.deepcopy(clock) if kind == "hit" and clock else events(number(0))
    elif kind == "noise":
        body["speed"] = {"type": "beats", "value": body["speed"]}
        body["range"] = [number(x) for x in body["range"]]
        body["contrast"] = number(0)
        if key:
            body["key"] = key
    elif kind == "audio" and key in ("fan", "size"):
        body["gain"] = number(90)
    elif kind == "space":
        movement = body.pop("move", None)
        if movement:
            body.update(width=source(movement["width"], clock, "width"),
                        width_relative=movement["width_relative"], boundary=movement["boundary"])
            body["offset"] = {"type": "time", "value": {
                **movement["path"], "events": copy.deepcopy(clock)}}
            if movement["width"]["type"] == "hit":
                body["width"]["value"].pop("events", None)
    return value


def sine(cycles=1, cosine=False):
    import math
    points = []
    for i in range(4 * cycles + 1):
        phase = i / 4 + (0.25 if cosine else 0)
        value = round(math.sin(2 * math.pi * phase))
        ease = "sine-in" if abs(value) == 1 else "sine-out"
        points.append([i / (4 * cycles), value, ease])
    points[-1] = points[-1][:2]
    return points


def multiply(a, b):
    if a["type"] in ("number", "proportion", "beats"):
        return scale(b, a["value"])
    if a["type"] in ("time", "space"):
        a = copy.deepcopy(a)
        a["value"]["gain"] = multiply(a["value"].get("gain", number(1)), b)
        return a
    if a["type"] == "random":
        a = copy.deepcopy(a)
        a["value"]["level"] = multiply(a["value"].get("level", number(1)), b)
        return a
    if a["type"] == "noise":
        a = copy.deepcopy(a)
        a["value"]["range"] = [multiply(v, b) for v in a["value"]["range"]]
        return a
    if a["type"] == "audio":
        a = copy.deepcopy(a)
        a["value"]["gain"] = multiply(a["value"].get("gain", number(1)), b)
        return a
    raise ValueError(f"cannot multiply source {a['type']}")


def convert_clip(clip):
    clip = copy.deepcopy(clip)
    inputs = clip.get("inputs", {})
    form = clip.get("graph", clip.get("form"))
    if "fade" in inputs:
        return clip
    every = inputs.get("every", number(0))
    clock = events(every)
    if form == "color@1":
        moving = next((v["value"]["move"] for v in inputs.values()
                       if v.get("type") == "space" and "move" in v["value"]), None)
        if moving:
            clock = events(every, moving["travel"])
        result = {k: source(inputs[k], clock, k) for k in ("color", "brightness")}
        result["fade"] = source(inputs["alpha"], clock, "alpha")
    elif form == "color.sparkle@1":
        clock = events(every, inputs["duration"])
        body = {"events": clock, "grain": {0: "fixture", 1: "head", 2: "clump2", 4: "clump4", 8: "clump8"}[inputs["grain"]["value"]]}
        for old, new in (("coverage", "coverage"), ("brightness", "level")):
            body[new] = source(inputs[old], clock, old)
            if inputs[old]["type"] == "hit":
                body[new]["value"].pop("events", None)
        result = {"color": source(inputs["color"], clock, "color"), "brightness": {"type": "random", "value": body}, "fade": source(inputs["alpha"], clock, "alpha")}
        form = "color@1"
    elif form == "color.noise@1":
        result = {"color": source(inputs["color"], clock, "color"), "fade": source(inputs["alpha"], clock, "alpha"),
                  "brightness": {"type": "noise", "value": {k: source(inputs[k], clock, k) for k in ("speed", "scale", "contrast")}}}
        result["brightness"]["value"]["range"] = [number(0), number(1)]
        form = "color@1"
    elif form == "aim@1":
        axis = inputs["axis"]["value"]
        result = {k: source(inputs[k], clock, k) for k in ("base", "direction", "point", "axis")}
        result["fade"] = source(inputs["alpha"], clock, "alpha")
        result["lean"] = space(axis, [[0, 0], [1, 1]], gain=source(inputs["fan"], clock, "fan"))
        if inputs["fan"].get("value") == 0:
            result["lean"] = {"type": "vector", "value": [0, 0, 0]}
        result["horizontal"] = number(0)
        result["vertical"] = number(0)
        motion = inputs["motion"]["value"]
        size = source(inputs["size"], clock, "size")
        if motion == "shape":
            shape = inputs["shape"]["value"]
            phase = space(axis, [[0, 0], [1, 1]], gain=scale(source(inputs["spread"], clock, "spread"), -1 / 360))
            if inputs["spread"].get("value") == 0:
                phase = number(0)
            for name, enabled in (("horizontal", shape != "swing_up_down"), ("vertical", shape != "swing_left_right")):
                if not enabled:
                    continue
                double = name == "vertical" and shape == "figure_8"
                result[name] = {"type": "time", "value": {
                    "events": clock, "points": sine(2 if double else 1, name == "horizontal" and shape == "circle"),
                    "gain": scale(size, 0.5) if double else size, "phase": phase}}
            if result["horizontal"]["type"] == "time" and result["vertical"]["type"] == "time":
                result["vertical"]["value"]["events"] = {"same_as": "horizontal"}
        elif motion == "noise":
            for name, component in (("horizontal", "1"), ("vertical", "2")):
                result[name] = {"type": "noise", "value": {
                    "speed": source(inputs["speed"], clock, "speed"), "range": [scale(size, -1), size],
                    "independent": True, "key": component, "grain": "head"}}
    elif form == "strobe.constant@1":
        result = {"rate": source(inputs["rate"], clock, "rate"), "fade": source(inputs["alpha"], clock, "alpha")}
    else:
        return clip
    if inputs.get("alpha", {}).get("type") in ("hit", "noise", "audio"):
        target = "rate" if form == "strobe.constant@1" else "brightness"
        modulation = source(inputs["alpha"], clock, "alpha")
        if inputs["alpha"]["type"] == "hit" and result[target]["type"] in ("time", "space", "random"):
            modulation["value"].pop("events", None)
        result[target] = multiply(result[target], modulation)
        result["fade"] = {"type": "proportion", "value": 1}
    clip["inputs"] = result
    clip["graph" if "graph" in clip else "form"] = form
    return clip


def convert_document(value):
    if isinstance(value, list):
        return [convert_document(v) for v in value]
    if not isinstance(value, dict):
        return value
    if "inputs" in value and ("graph" in value or "form" in value):
        return convert_clip(value)
    result = {k: convert_document(v) for k, v in value.items()}
    if result.get("input") == "alpha":
        result["input"] = "fade"
    return result


if __name__ == "__main__":
    json.dump(convert_document(json.load(sys.stdin)), sys.stdout, indent=2)
    print()
