#!/usr/bin/env python3
"""Parity check for the clip graphs migration (docs/specs/clip-graphs.md 8.5).

    check_clip_graphs_migration.py --database <copy of luma.db> --out <dir>
        [--old <old source_parity binary>] [--new <clip_graph_parity binary>]

Without --old, exports 6ebfb8f4 with `git archive` into <dir>/luma-6ebfb8f4
(decision 35: an export, not a worktree) and builds its
`patterns/examples/source_parity.rs`. Without --new, builds
`patterns/examples/clip_graph_parity.rs` from this checkout.

Both evaluators read NDJSON {clip, cells, beats} and answer {"ok": frames} or
{"error": text}. Frames are dumped at 1/8 beat steps plus the exclusive end
and the representable beat before it, on the 20-head stand-in rig the source
migration used. Writes <dir>/parity.md and <dir>/parity.json. Never writes
to the database or the network. Exits 1 on any hard difference.
"""
import argparse
import collections
import json
import math
import os
import pathlib
import sqlite3
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import migrate_clip_graphs as migrate  # noqa: E402

BASELINE = "6ebfb8f4"
REPO = pathlib.Path(__file__).resolve().parents[2]
TOLERANCE = 1e-6
STATISTICS = 0.05
CELLS = [dict(id=f"fixture{n // 4}:{n % 4}", group="all", world=[n % 5, n // 5, 3],
              uvz=[n % 5, n // 5, 3]) for n in range(20)]
NOT_COMPARED = {migrate.FAN, migrate.BLOOM}
STATISTICAL = {migrate.NOISE, migrate.NOISE_SPATIAL, migrate.NOISE_INDEPENDENT}
# Notes that are exact conversions, compared at full tolerance.
EXACT_NOTES = {migrate.PATH}


def build_old(scratch):
    """git archive of the baseline's patterns crate, built standalone."""
    root = pathlib.Path(scratch) / f"luma-{BASELINE}"
    crate = root / "backend/crates/patterns"
    if not crate.exists():
        root.mkdir(parents=True, exist_ok=True)
        archive = subprocess.run(["git", "-C", str(REPO), "archive", BASELINE,
                                  "backend/crates/patterns", "backend/Cargo.lock"],
                                 check=True, capture_output=True).stdout
        subprocess.run(["tar", "-x", "-C", str(root)], input=archive, check=True)
        # The baseline workspace needs crates outside the export; make the
        # patterns crate its own workspace, pinned by the baseline lock file.
        with open(crate / "Cargo.toml", "a") as f:
            f.write("\n[workspace]\n")
        (crate / "Cargo.lock").write_bytes((root / "backend/Cargo.lock").read_bytes())
    target = pathlib.Path(scratch) / "target-old"
    cargo(["--manifest-path", str(crate / "Cargo.toml"), "--example", "source_parity"], target)
    return str(target / "release/examples/source_parity")


def build_new(scratch):
    target = pathlib.Path(scratch) / "target-new"
    cargo(["--manifest-path", str(REPO / "backend/Cargo.toml"), "-p", "luma-patterns",
           "--example", "clip_graph_parity"], target)
    return str(target / "release/examples/clip_graph_parity")


def cargo(args, target):
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    subprocess.run(["cargo", "+1.97.1", "build", "--release", *args], check=True, env=env)


def evaluate(binary, requests, flag=None):
    if not requests:
        return []
    process = subprocess.run([binary] + ([flag] if flag else []),
                             input="\n".join(map(json.dumps, requests)) + "\n",
                             text=True, capture_output=True, check=True)
    results = [json.loads(line) for line in process.stdout.splitlines()]
    if len(results) != len(requests):
        raise RuntimeError(f"{binary}: expected {len(requests)} results, got {len(results)}")
    return results


def beats(clip):
    start, end = clip["start"], clip["start"] + clip["duration"]
    steps = math.ceil(clip["duration"] * 8)
    result = [start + k / 8 for k in range(steps) if start + k / 8 < end]
    return result + [math.nextafter(end, start), end]


def heads(frame):
    """Per head: rgb (color × dimmer), strobe, aim direction and weight."""
    lighting = frame.get("lighting") if isinstance(frame, dict) else None
    if not lighting:
        return {}
    values = lighting.get("value", lighting) if isinstance(lighting, dict) else {}
    result = {}
    for head, out in values.items():
        color, dimmer = out.get("color"), out.get("dimmer")
        rgb = None if color is None and dimmer is None else \
            [c * (dimmer or 0.0) for c in (color or [1.0, 1.0, 1.0])]
        aim = out.get("aim")
        result[head] = dict(rgb=rgb, strobe=out.get("strobe"),
                            aim=None if aim is None else (aim["direction"], aim["weight"]))
    return result


def angle(a, b):
    la, lb = math.sqrt(sum(x * x for x in a)), math.sqrt(sum(x * x for x in b))
    if la < 1e-12 or lb < 1e-12:
        return 0.0 if la == lb else math.inf
    cos = sum(x * y for x, y in zip(a, b)) / (la * lb)
    return math.acos(max(-1.0, min(1.0, cos)))


def frame_error(a, b):
    """The largest difference between two frames, and which channel."""
    ha, hb = heads(a), heads(b)
    worst = (0.0, "")
    for head in set(ha) | set(hb):
        x, y = ha.get(head), hb.get(head)
        if x is None or y is None:
            # An unwritten head reads as dark and unaimed.
            x = x or dict(rgb=[0.0] * 3, strobe=0.0, aim=None)
            y = y or dict(rgb=[0.0] * 3, strobe=0.0, aim=None)
        for key in ("rgb", "strobe"):
            u, v = x[key], y[key]
            if u is None and v is None:
                continue
            u = u if u is not None else ([0.0] * 3 if key == "rgb" else 0.0)
            v = v if v is not None else ([0.0] * 3 if key == "rgb" else 0.0)
            error = max(abs(p - q) for p, q in zip(u, v)) if key == "rgb" else abs(u - v)
            worst = max(worst, (error, f"{head} {key}"))
        if x["aim"] or y["aim"]:
            (da, wa), (db, wb) = x["aim"] or ([0, 0, 0], 0.0), y["aim"] or ([0, 0, 0], 0.0)
            worst = max(worst, (abs(wa - wb), f"{head} weight"))
            if wa > TOLERANCE and wb > TOLERANCE:
                worst = max(worst, (angle(da, db), f"{head} aim"))
    return worst


def flat(frame):
    """Every head's numbers, for statistics."""
    result = {}
    for head, out in heads(frame).items():
        numbers = list(out["rgb"] or []) + ([out["strobe"]] if out["strobe"] is not None else [])
        if out["aim"]:
            numbers += [c * out["aim"][1] for c in out["aim"][0]]
        result[head] = numbers
    return result


def statistics_error(a_frames, b_frames):
    """Largest difference of the per-head mean and standard deviation."""
    def stats(frames):
        series = collections.defaultdict(list)
        for frame in frames:
            for head, numbers in flat(frame).items():
                series[head].append(numbers)
        result = {}
        for head, rows in series.items():
            columns = list(zip(*rows))
            result[head] = [(sum(c) / len(c),
                             math.sqrt(sum((v - sum(c) / len(c)) ** 2 for v in c) / len(c)))
                            for c in columns]
        return result
    sa, sb = stats(a_frames), stats(b_frames)
    worst = 0.0
    for head in set(sa) | set(sb):
        if head not in sa or head not in sb or len(sa[head]) != len(sb[head]):
            return math.inf
        for (ma, da), (mb, db) in zip(sa[head], sb[head]):
            worst = max(worst, abs(ma - mb), abs(da - db))
    return worst


def contains_audio(value):
    if isinstance(value, dict):
        return value.get("type") == "audio" or any(map(contains_audio, value.values()))
    return isinstance(value, list) and any(map(contains_audio, value))


def suspects(inputs):
    """Old features that the new runtime reads differently, as a hint for
    each hard difference."""
    text = json.dumps(inputs)
    found = []
    for word, hint in (('"radial"', "radial axis (old min–max, new over the largest distance)"),
                       ('"order"', "order axis (old i/(n−1), new (i+0.5)/n)"),
                       ('"wrap"', "wrap boundary"), ('"random"', "random"),
                       ('"mirror"', "mirror"), ('"angle"', "angle axis"),
                       ('"life"', "event life"), ('"same_as"', "same_as"),
                       ('"hold"', "hold ease")):
        if word in text:
            found.append(hint)
    return found


def collect(args):
    db = sqlite3.connect(f"file:{pathlib.Path(args.database).resolve()}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    clips = []
    for row in db.execute("SELECT * FROM clips ORDER BY id"):
        clip = dict(graph=row["graph"], start=row["start"], duration=row["duration"],
                    seed=int(row["seed"]), selection=json.loads(row["selection_json"]),
                    z_index=row["z_index"], blend_mode=row["blend_mode"],
                    inputs=json.loads(row["inputs_json"]))
        if row["selection_seed"] is not None:
            clip["selection_seed"] = int(row["selection_seed"])
        clips.append((f"clip:{row['id']}", clip))
    for row in db.execute("SELECT id, base_json, state_json FROM drafts ORDER BY id"):
        seen = set()
        for column in ("base_json", "state_json"):
            for path, clip in migrate.old_clips(json.loads(row[column])):
                key = json.dumps(clip, sort_keys=True)
                if key not in seen:
                    seen.add(key)
                    clips.append((f"draft:{row['id']}:{column}:{path}", clip))
    presets = subprocess.run(["git", "-C", str(REPO), "show",
                              f"{BASELINE}:backend/crates/patterns/src/presets.json"],
                             check=True, capture_output=True, text=True).stdout
    for preset in json.loads(presets)["presets"]:
        clips.append((f"preset:{preset['name']}:{preset['form']}",
                      dict(graph=preset["form"], inputs=preset["inputs"], start=0.0, duration=16.0,
                           seed=41, selection={"expression": "all"}, z_index=0,
                           blend_mode="offset" if preset["form"] == "aim@1" else "replace")))
    return clips


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--database", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--old", help="old source_parity binary; built from an export when absent")
    parser.add_argument("--new", help="clip_graph_parity binary; built when absent")
    parser.add_argument("--limit", type=int, help="compare only the first N clips (a smoke run)")
    args = parser.parse_args()
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    old_bin = args.old or build_old(out)
    new_bin = args.new or build_new(out)
    clips = collect(args)[: args.limit]

    report = dict(total=len(clips), exact=0, statistical=0, audio_checked=0, hard=[], noise_outside=[],
                  unmappable=[], not_compared=collections.Counter(), approximate=collections.Counter(),
                  approximate_errors={}, errors=[], check_failures=[], maximum_error=0.0)
    exact, statistical, audio = [], [], []
    for key, clip in clips:
        try:
            new = migrate.convert_clip(clip)
            _, _, notes = migrate.convert_inputs(clip["graph"], clip["inputs"], clip["duration"])
        except migrate.Unmappable as e:
            report["unmappable"].append(dict(id=key, why=str(e)))
            continue
        for note in notes:
            report["approximate"][note] += 1
        notes = notes - EXACT_NOTES
        item = (key, clip, new, notes)
        if contains_audio(clip["inputs"]):
            audio.append(item)
        elif notes & NOT_COMPARED:
            for note in notes & NOT_COMPARED:
                report["not_compared"][note] += 1
            audio.append(item)  # still schema-checked
        elif notes & STATISTICAL:
            statistical.append(item)
        else:
            exact.append(item)

    # Every converted clip must pass the new checker.
    everything = exact + statistical + audio
    for (key, *_), result in zip(everything, evaluate(new_bin, [i[2] for i in everything], "--check")):
        if "error" in result:
            report["check_failures"].append(dict(id=key, error=result["error"]))
    old_check = evaluate(old_bin, [dict(clip=i[1], cells=[], beats=[]) for i in audio])
    for (key, *_), result in zip(audio, old_check):
        if "error" in result:
            report["errors"].append(dict(id=key, old=result["error"]))
        else:
            report["audio_checked"] += 1

    def compare(items, statistical_mode):
        for offset in range(0, len(items), 32):
            batch = items[offset:offset + 32]
            old = evaluate(old_bin, [dict(clip=c, cells=CELLS, beats=beats(c)) for _, c, _, _ in batch])
            new = evaluate(new_bin, [dict(clip=n, cells=CELLS, beats=beats(c)) for _, c, n, _ in batch])
            for (key, clip, _, notes), a, b in zip(batch, old, new):
                if "error" in a or "error" in b:
                    report["errors"].append(dict(id=key, old=a.get("error"), new=b.get("error")))
                    continue
                if statistical_mode:
                    error = statistics_error(a["ok"], b["ok"])
                    report["statistical"] += 1
                    if error > STATISTICS:
                        # Accepted as approximate (a new noise field): listed, not a failure.
                        report["noise_outside"].append(dict(id=key, error=error))
                    continue
                worst, where, at = 0.0, "", None
                for beat, x, y in zip(beats(clip), a["ok"], b["ok"]):
                    error, channel = frame_error(x, y)
                    if error > worst:
                        worst, where, at = error, channel, beat
                report["maximum_error"] = max(report["maximum_error"], worst)
                if notes:
                    for note in notes:
                        report["approximate_errors"][note] = max(
                            report["approximate_errors"].get(note, 0.0), worst)
                    continue
                report["exact"] += 1
                if worst > TOLERANCE:
                    report["hard"].append(dict(id=key, error=worst, channel=where, beat=at,
                                               beats_from_start=None if at is None else at - clip["start"],
                                               suspects=suspects(clip["inputs"])))
            print(f"{min(offset + 32, len(items))}/{len(items)} compared", file=sys.stderr, flush=True)

    compare(exact, False)
    compare(statistical, True)
    write(out, report, len(exact))
    failed = report["hard"] or report["check_failures"] or report["unmappable"] or report["errors"]
    sys.exit(1 if failed else 0)


def write(out, report, exact_count):
    serial = dict(report, not_compared=dict(report["not_compared"]),
                  approximate=dict(report["approximate"]))
    (out / "parity.json").write_text(json.dumps(serial, indent=2) + "\n")
    hints = collections.Counter(h for d in report["hard"] for h in d.get("suspects", []))
    lines = ["# Clip graphs migration: parity", "",
             f"- clips, draft clips and old presets: {report['total']}",
             f"- compared exactly (tolerance {TOLERANCE}): {exact_count}; "
             f"hard differences: {len(report['hard'])}",
             f"- compared by per-head mean and deviation (within {STATISTICS}): {report['statistical']}; "
             f"outside it (accepted, listed): {len(report['noise_outside'])}",
             f"- audio and lean clips schema-checked: {report['audio_checked']}",
             f"- fail the new checker: {len(report['check_failures'])}",
             f"- evaluator errors: {len(report['errors'])}",
             f"- hard unmappable: {len(report['unmappable'])}",
             f"- largest difference seen: {report['maximum_error']:.3g}", "",
             "## Approximations", ""]
    for note, n in sorted(report["approximate"].items(), key=lambda kv: -kv[1]):
        extra = report["approximate_errors"].get(note)
        detail = f"; largest difference {extra:.3g}" if extra is not None else ""
        listed = " (listed, not compared)" if note in NOT_COMPARED else ""
        lines.append(f"- {note}: {n}{listed}{detail}")
    lines += ["", "## Hard differences", ""]
    if report["hard"]:
        lines.append("Old features in the clips that differ (a clip can have several):")
        lines += [f"- {h}: {n}" for h, n in hints.most_common()]
        lines.append("")
        for d in sorted(report["hard"], key=lambda d: -d["error"])[:100]:
            lines.append(f"- {d['id']}: {d['error']:.3g} at {d.get('channel', d.get('kind'))}, "
                         f"beat +{(d.get('beats_from_start') or 0):.4g}")
    else:
        lines.append("- none")
    for title, rows in (("Unmappable", report["unmappable"]),
                        ("Checker failures", report["check_failures"]),
                        ("Evaluator errors", report["errors"])):
        lines += ["", f"## {title}", ""]
        reasons = collections.Counter(str(r.get("why") or r.get("error") or r.get("new") or r.get("old"))[:200]
                                      for r in rows)
        lines += [f"- {why}: {n}" for why, n in reasons.most_common(30)] or ["- none"]
    (out / "parity.md").write_text("\n".join(lines) + "\n")
    print(json.dumps({k: (len(v) if isinstance(v, list) else v) for k, v in serial.items()
                      if k not in ("approximate_errors",)}, default=str))


if __name__ == "__main__":
    main()
