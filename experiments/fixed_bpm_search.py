"""Coarse-to-fine BPM/phase search over beat logits to force a fixed grid.

Uses beat_this framewise logits, searches BPM and phase that maximize the
interpolated probability along the grid, then regenerates beats/downbeats and
prints/saves a click-overlay demo.
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

import librosa
import numpy as np
import soundfile as sf
import torch
from beat_this.inference import Audio2Frames
from beat_this.preprocessing import load_audio


HOP_SECONDS = 441 / 22050  # hop length in seconds for beat_this spectrograms
DEFAULT_SONG = Path(__file__).parent / "songs" / "four_chord_progression_100bpm.wav"


@dataclass
class GridResult:
    bpm: float
    offset: float
    beats_per_bar: int
    score: float
    beats: np.ndarray
    downbeats: np.ndarray


def sigmoid(x: np.ndarray) -> np.ndarray:
    return 1.0 / (1.0 + np.exp(-x))


def interpolate_at(times: np.ndarray, values: np.ndarray, query: np.ndarray) -> np.ndarray:
    return np.interp(query, times, values, left=0.0, right=0.0)


def get_logits(audio_path: Path) -> tuple[np.ndarray, np.ndarray, np.ndarray, float, int, np.ndarray]:
    signal, sr = load_audio(audio_path)
    tracker = Audio2Frames(checkpoint_path="final0", device="cpu", float16=False)
    beat_logits, downbeat_logits = tracker(signal, sr)
    beat_logits = beat_logits.cpu().numpy()
    downbeat_logits = downbeat_logits.cpu().numpy()
    times = np.arange(len(beat_logits)) * HOP_SECONDS
    duration = times[-1] if len(times) else 0.0
    raw_audio, raw_sr = sf.read(audio_path)
    if raw_audio.ndim > 1:
        raw_audio = raw_audio.mean(axis=1)
    return times, beat_logits, downbeat_logits, duration, raw_sr, raw_audio.astype(np.float32)


def score_grid(
    beat_probs: np.ndarray, times: np.ndarray, duration: float, bpm: float, phases: Iterable[float]
) -> tuple[float, float]:
    period = 60.0 / bpm
    best_phase, best_score = 0.0, -np.inf
    for phase in phases:
        grid_times = np.arange(phase, duration, period)
        if len(grid_times) == 0:
            continue
        vals = interpolate_at(times, beat_probs, grid_times)
        score = float(vals.mean())
        if score > best_score:
            best_score, best_phase = score, float(phase)
    return best_phase, best_score


def pick_downbeats(
    downbeat_probs: np.ndarray,
    times: np.ndarray,
    duration: float,
    period: float,
    base_phase: float,
    beats_per_bar_candidates: Iterable[int] = (3, 4, 6),
) -> tuple[np.ndarray, int]:
    best_bpb, best_phase, best_score = 4, base_phase, -np.inf
    for bpb in beats_per_bar_candidates:
        bar_period = period * bpb
        # keep downbeat phase near the beat phase to avoid wild shifts
        phases = np.linspace(base_phase, base_phase + period, num=16, endpoint=False)
        for phase in phases:
            grid = np.arange(phase, duration, bar_period)
            if len(grid) == 0:
                continue
            vals = interpolate_at(times, downbeat_probs, grid)
            score = float(vals.mean())
            if score > best_score:
                best_score, best_phase, best_bpb = score, float(phase), int(bpb)
    downbeats = np.arange(best_phase, duration, period * best_bpb)
    return downbeats, best_bpb


def search_fixed_bpm_grid(
    beat_probs: np.ndarray,
    downbeat_probs: np.ndarray,
    times: np.ndarray,
    duration: float,
    bpm_min: float = 70,
    bpm_max: float = 170,
) -> GridResult:
    # coarse sweep
    bpm_grid = np.arange(bpm_min, bpm_max + 1e-6, 1.0)
    best = None
    for bpm in bpm_grid:
        period = 60.0 / bpm
        phases = np.linspace(0, period, num=24, endpoint=False)
        phase, score = score_grid(beat_probs, times, duration, bpm, phases)
        if best is None or score > best.score:
            beats = np.arange(phase, duration, period)
            downbeats, bpb = pick_downbeats(downbeat_probs, times, duration, period, phase)
            best = GridResult(bpm=bpm, offset=phase, beats_per_bar=bpb, score=score, beats=beats, downbeats=downbeats)

    # refine around the best BPM
    fine_grid = np.arange(max(bpm_min, best.bpm - 4), min(bpm_max, best.bpm + 4), 0.1)
    for bpm in fine_grid:
        period = 60.0 / bpm
        phases = np.linspace(best.offset - 0.25 * period, best.offset + 0.25 * period, num=48, endpoint=False)
        phase, score = score_grid(beat_probs, times, duration, bpm, phases)
        if score > best.score:
            beats = np.arange(phase, duration, period)
            downbeats, bpb = pick_downbeats(downbeat_probs, times, duration, period, phase)
            best = GridResult(bpm=bpm, offset=phase, beats_per_bar=bpb, score=score, beats=beats, downbeats=downbeats)
    return best


def render_click_overlay(
    audio: np.ndarray, sr: int, beats: np.ndarray, downbeats: np.ndarray, beat_amp: float = 0.3
) -> np.ndarray:
    length = len(audio)
    beat_clicks = librosa.clicks(times=beats, sr=sr, length=length, click_freq=900, click_duration=0.03)
    down_clicks = librosa.clicks(times=downbeats, sr=sr, length=length, click_freq=1400, click_duration=0.06)
    mix = audio + beat_amp * beat_clicks + beat_amp * 1.5 * down_clicks
    max_abs = np.max(np.abs(mix))
    if max_abs > 1.0:
        mix = mix / max_abs
    return mix.astype(np.float32)


def main() -> None:
    import sys

    song_path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_SONG
    output_audio = (
        Path(__file__).parent / f"{song_path.stem}_fixed_bpm_search_clicks.wav"
        if len(sys.argv) == 1
        else Path(sys.argv[2]) if len(sys.argv) > 2 else Path(f"{song_path.stem}_fixed_bpm_search_clicks.wav")
    )

    times, beat_logits, downbeat_logits, duration, sr, audio = get_logits(song_path)
    beat_probs = sigmoid(beat_logits)
    downbeat_probs = sigmoid(downbeat_logits)
    result = search_fixed_bpm_grid(beat_probs, downbeat_probs, times, duration)
    print(f"Fixed BPM: {result.bpm:.2f} | offset: {result.offset:.3f}s | beats/bar: {result.beats_per_bar}")
    print(f"{len(result.beats)} beats, {len(result.downbeats)} downbeats")

    overlay = render_click_overlay(audio, sr, result.beats, result.downbeats)
    sf.write(output_audio, overlay, sr)
    print(f"Wrote click demo to {output_audio}")


if __name__ == "__main__":
    torch.set_num_threads(1)
    main()
