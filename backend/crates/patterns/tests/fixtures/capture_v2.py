"""Capture graph version 2 output for every shipped preset and Slash, as played
by commit 691cd753, into v2_frames.json (read by tests/clip_graph_runtime.rs).

    git archive 691cd753 | tar -x -C <dir>; build its examples/clip_graph_parity
    python3 capture_v2.py <that clip_graph_parity>
"""
import json, pathlib, subprocess, sys
B = sys.argv[1]
HERE = pathlib.Path(__file__).resolve().parent
P = json.loads(subprocess.run(['git', '-C', str(HERE), 'show', '691cd753:backend/crates/patterns/src/presets.json'],
                              capture_output=True, text=True, check=True).stdout)
RIGS = {
  "spread": [dict(id=f"fixture{n // 4}:{n % 4}", group="all", world=[n % 5, n // 5, 3 + (3 * n) % 7 / 4],
                  uvz=[n % 5, n // 5, 3 + (3 * n) % 7 / 4]) for n in range(20)],
  "bars": [dict(id=f"bar{c}:{r}", group="left" if c < 2 else "right", world=[c, 0, r], uvz=[c, 0, r])
           for c in range(5) for r in range(5)],
  "line13": [dict(id=f"p{n:02}:0", group="all", world=[n, 0, 3], uvz=[n, 0, 3]) for n in range(13)],
}
BEATS = [i * 0.75 for i in range(22)]
SLASH = {"version": 2, "nodes": {
 "clock1":{"kind":"clock","inputs":{"every":2}},
 "time1":{"kind":"time","inputs":{"clock":{"node":"clock1"}}},
 "space1":{"kind":"space","settings":{"kind":"line","wrap":"no"},"inputs":{"direction":[-0.82,0,0.57]}},
 "curve1":{"kind":"curve","settings":{"kind":"number"},"inputs":{"x":{"node":"space1"},"low":0,"high":0.2}},
 "time2":{"kind":"time","inputs":{"clock":{"node":"clock1"},"delay":{"node":"curve1"}}},
 "curve2":{"kind":"curve","settings":{"kind":"number"},"inputs":{"x":{"node":"time2"},"shape":{"points":[[0,0],[0,1],[1,1]]}}},
 "space2":{"kind":"space","settings":{"kind":"line","wrap":"no"},"inputs":{"direction":[0.57,0,0.82]}},
 "curve3":{"kind":"curve","settings":{"kind":"number"},"inputs":{"x":{"node":"space2"},"shape":{"points":[[0,1],[0.68,0],[1,0.47]]},"low":0,"high":0.9}},
 "curve4":{"kind":"curve","settings":{"kind":"number"},"inputs":{"x":{"node":"space2"},"shape":{"points":[[0,1],[0.68,0],[1,0.47]]},"low":0.05,"high":0.4}},
 "time3":{"kind":"time","inputs":{"clock":{"node":"clock1"},"delay":{"node":"curve3"},"length":{"node":"curve4"}}},
 "curve5":{"kind":"curve","settings":{"kind":"number"},"inputs":{"x":{"node":"time3"}}},
 "curve6":{"kind":"curve","settings":{"kind":"number"},"inputs":{"x":{"node":"time1"},"shape":{"points":[[0,1,"hold"],[0.2,1,"sine-out"],[1,0]]}}},
 "curve7":{"kind":"curve","settings":{"kind":"color"},"inputs":{"x":{"node":"time1"},"gradient":{"stops":[{"t":0,"color":[1,1,1]},{"t":0.2,"color":[1,1,1]},{"t":0.5,"color":[1,0,0.01]},{"t":1,"color":[1,0,0.01]}]}}},
 "color1":{"kind":"color","inputs":{"color":{"node":"curve7"},"brightness":[{"node":"curve2"},{"node":"curve5"},{"node":"curve6"}]}}}}
clips = [(c["name"], c["blend_mode"], c["graph"]) for c in P["clips"]] + [("Slash", "replace", SLASH)]
reqs, keys = [], []
for name, blend, graph in clips:
    for rig, cells in RIGS.items():
        clip = {"name": name, "start": 0, "duration": 16, "seed": 7, "selection": {"expression": "all"},
                "z_index": 0, "blend_mode": blend, "graph": graph}
        reqs.append(json.dumps({"clip": clip, "cells": cells, "beats": BEATS})); keys.append((name, rig))
out = subprocess.run([B], input="\n".join(reqs) + "\n", capture_output=True, text=True, check=True).stdout.splitlines()
r6 = lambda v: round(v, 6) + 0.0
result = {"source": "graph version 2 at 691cd753, examples/clip_graph_parity", "beats": BEATS, "rigs": RIGS, "clips": {}}
for (name, rig), line in zip(keys, out):
    ans = json.loads(line)
    if "error" in ans:
        print("skip", name, rig, ans["error"][:80]); continue
    frames = []
    for frame in ans["ok"]:
        lighting = frame.get("lighting", {}).get("value", {})
        row = []
        for cell in RIGS[rig]:
            v = lighting.get(cell["id"], {})
            if "aim" in v:
                row.append([r6(x) for x in v["aim"]["direction"]] + [r6(v["aim"]["weight"])])
            elif "strobe" in v and "dimmer" not in v:
                row.append([r6(v["strobe"])])
            else:
                c, d = v.get("color", [1, 1, 1]), v.get("dimmer", 0.0)
                row.append([r6(x * d) for x in c])
        frames.append(row)
    result["clips"].setdefault(name, {})[rig] = frames
json.dump(result, open(HERE / 'v2_frames.json', 'w'), separators=(",", ":"))
print(len(result["clips"]))
