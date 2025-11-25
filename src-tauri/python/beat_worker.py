#!/usr/bin/env python3
"""
Fixed-BPM beat grid extractor.
Given an audio file path, it emits a JSON payload containing beat/downbeat
timestamps plus the fixed BPM metadata (bpm, downbeat offset, beats_per_bar).
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys
from dataclasses import dataclass
from typing import Iterable


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compute beat and downbeat timings for an audio file.",
    )
    parser.add_argument(
        "audio_file",
        type=pathlib.Path,
        help="Path to the audio file that should be analysed.",
    )
    parser.add_argument(
        "--checkpoint",
        default="final0",
        help="beat_this checkpoint to use (defaults to 'final0').",
    )
    parser.add_argument(
        "--bpm-min",
        type=float,
        default=70.0,
        help="Lower BPM bound for the fixed-grid search.",
    )
    parser.add_argument(
        "--bpm-max",
        type=float,
        default=170.0,
        help="Upper BPM bound for the fixed-grid search.",
    )
    return parser.parse_args()


def serialize(values):
    return [float(value) for value in values]


def sigmoid(x):
    return 1.0 / (1.0 + math.exp(-x)) if isinstance(x, (float, int)) else 1.0 / (1.0 + np.exp(-x))


def _sigmoid_array(arr):
    import numpy as np

    return 1.0 / (1.0 + np.exp(-arr))


def _interpolate_at(times, values, query):
    import numpy as np

    return np.interp(query, times, values, left=0.0, right=0.0)


def _score_grid(beat_probs, times, duration, bpm, phases):
    import numpy as np

    period = 60.0 / bpm
    best_phase, best_score = 0.0, -np.inf
    for phase in phases:
        grid_times = np.arange(phase, duration, period)
        if len(grid_times) == 0:
            continue
        vals = _interpolate_at(times, beat_probs, grid_times)
        score = float(vals.mean())
        if score > best_score:
            best_score, best_phase = score, float(phase)
    return best_phase, best_score


def _pick_downbeats(downbeat_probs, times, duration, period, base_phase, beats_per_bar_candidates=(3, 4, 6)):
    import numpy as np

    best_bpb, best_phase, best_score = 4, base_phase, -np.inf
    for bpb in beats_per_bar_candidates:
        bar_period = period * bpb
        phases = np.linspace(base_phase, base_phase + period, num=16, endpoint=False)
        for phase in phases:
            grid = np.arange(phase, duration, bar_period)
            if len(grid) == 0:
                continue
            vals = _interpolate_at(times, downbeat_probs, grid)
            score = float(vals.mean())
            if score > best_score:
                best_score, best_phase, best_bpb = score, float(phase), int(bpb)
    downbeats = np.arange(best_phase, duration, period * best_bpb)
    return downbeats, best_bpb


@dataclass
class GridResult:
    bpm: float
    offset: float
    beats_per_bar: int
    beats: list[float]
    downbeats: list[float]


def fixed_bpm_from_logits(
    beat_logits,
    downbeat_logits,
    hop_seconds,
    bpm_min=70.0,
    bpm_max=170.0,
):
    import numpy as np

    times = np.arange(len(beat_logits)) * hop_seconds
    duration = times[-1] if len(times) else 0.0
    beat_probs = _sigmoid_array(beat_logits)
    downbeat_probs = _sigmoid_array(downbeat_logits)

    # coarse sweep
    bpm_grid = np.arange(bpm_min, bpm_max + 1e-6, 1.0)
    best = None
    for bpm in bpm_grid:
        period = 60.0 / bpm
        phases = np.linspace(0, period, num=24, endpoint=False)
        phase, score = _score_grid(beat_probs, times, duration, bpm, phases)
        beats = np.arange(phase, duration, period)
        downbeats, bpb = _pick_downbeats(downbeat_probs, times, duration, period, phase)
        if best is None or score > best[0]:
            best = (score, bpm, phase, bpb, beats, downbeats)

    # refine around the best BPM
    _, bpm_best, phase_best, bpb_best, beats_best, down_best = best
    fine_grid = np.arange(max(bpm_min, bpm_best - 4), min(bpm_max, bpm_best + 4), 0.1)
    for bpm in fine_grid:
        period = 60.0 / bpm
        phases = np.linspace(phase_best - 0.25 * period, phase_best + 0.25 * period, num=48, endpoint=False)
        phase, score = _score_grid(beat_probs, times, duration, bpm, phases)
        beats = np.arange(phase, duration, period)
        downbeats, bpb = _pick_downbeats(downbeat_probs, times, duration, period, phase)
        if score > best[0]:
            best = (score, bpm, phase, bpb, beats, downbeats)

    score, bpm, phase, bpb, beats, downbeats = best
    return GridResult(
        bpm=float(bpm),
        offset=float(phase),
        beats_per_bar=int(bpb),
        beats=serialize(beats),
        downbeats=serialize(downbeats),
    )


def main() -> int:
    args = parse_args()

    try:
        from beat_this.inference import Audio2Frames
        from beat_this.preprocessing import load_audio
    except Exception as exc:  # pragma: no cover - import error reporting
        print(
            json.dumps({"error": f"Failed to import beat_this: {exc}"}),
            file=sys.stderr,
        )
        return 1

    if not args.audio_file.exists():
        print(
            json.dumps({"error": f"Audio file does not exist: {args.audio_file}"}),
            file=sys.stderr,
        )
        return 1

    try:
        signal, sr = load_audio(args.audio_file)
        tracker = Audio2Frames(checkpoint_path=str(args.checkpoint), device="cpu", float16=False)
        beat_logits, downbeat_logits = tracker(signal, sr)
        hop_seconds = 441 / 22050  # matches beat_this preprocessing
        result = fixed_bpm_from_logits(
            beat_logits.cpu().numpy(),
            downbeat_logits.cpu().numpy(),
            hop_seconds,
            bpm_min=args.bpm_min,
            bpm_max=args.bpm_max,
        )
    except Exception as exc:  # pragma: no cover - runtime error reporting
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 1

    payload = {
        "beats": result.beats,
        "downbeats": result.downbeats,
        "bpm": result.bpm,
        "downbeat_offset": result.offset,
        "beats_per_bar": result.beats_per_bar,
    }
    sys.stdout.write(json.dumps(payload))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover - script entrypoint
    raise SystemExit(main())
