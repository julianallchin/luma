"""Dirichlet-comb style fixed BPM extraction on beat logits (frame domain).

Operates on beat_this framewise beat/downbeat probabilities. For each BPM,
find the best frame-phase by summing probabilities at comb positions, then
generate beats/downbeats and save a click-overlay demo.
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


HOP_SECONDS = 441 / 22050
DEFAULT_SONG = Path(__file__).parent / "songs" / "four_chord_progression_100bpm.wav"


@dataclass
class CombResult:
    bpm: float
    offset: float
    beats_per_bar: int
    score: float
    beats: np.ndarray
    downbeats: np.ndarray


def sigmoid(x: np.ndarray) -> np.ndarray:
    return 1.0 / (1.0 + np.exp(-x))


def get_logits(audio_path: Path) -> tuple[np.ndarray, np.ndarray, float, int, np.ndarray]:
    signal, sr = load_audio(audio_path)
    tracker = Audio2Frames(checkpoint_path="final0", device="cpu", float16=False)
    beat_logits, downbeat_logits = tracker(signal, sr)
    beat_logits = beat_logits.cpu().numpy()
    downbeat_logits = downbeat_logits.cpu().numpy()
    duration = len(beat_logits) * HOP_SECONDS
    raw_audio, raw_sr = sf.read(audio_path)
    if raw_audio.ndim > 1:
        raw_audio = raw_audio.mean(axis=1)
    return beat_logits, downbeat_logits, duration, raw_sr, raw_audio.astype(np.float32)


def comb_score(probs: np.ndarray, period_frames: int) -> tuple[int, float]:
    best_phase, best_score = 0, -np.inf
    for phase in range(period_frames):
        score = float(probs[phase::period_frames].mean())  # stride-sum comb
        if score > best_score:
            best_score, best_phase = score, phase
    return best_phase, best_score


def pick_downbeats(
    downbeat_probs: np.ndarray,
    period_frames: int,
    base_phase: int,
    beats_per_bar_candidates: Iterable[int] = (3, 4, 6),
) -> tuple[np.ndarray, int, float]:
    best_bpb, best_phase, best_score = 4, base_phase, -np.inf
    for bpb in beats_per_bar_candidates:
        bar_period = period_frames * bpb
        # scan a narrow window around the beat phase for downbeats
        for delta in range(-period_frames // 2, period_frames // 2 + 1, max(1, period_frames // 16)):
            phase = max(0, base_phase + delta) % bar_period
            score = float(downbeat_probs[phase::bar_period].mean())
            if score > best_score:
                best_score, best_phase, best_bpb = score, phase, bpb
    phases = np.arange(best_phase, len(downbeat_probs), best_bpb * period_frames)
    downbeats = phases * HOP_SECONDS
    return downbeats, best_bpb, best_score


def comb_fixed_bpm(
    beat_probs: np.ndarray,
    downbeat_probs: np.ndarray,
    duration: float,
    bpm_min: float = 70,
    bpm_max: float = 170,
) -> CombResult:
    hop = HOP_SECONDS
    best = None
    for bpm in np.arange(bpm_min, bpm_max + 1e-6, 1.0):
        period = 60.0 / bpm
        period_frames = max(1, int(round(period / hop)))
        phase, score = comb_score(beat_probs, period_frames)
        bpm_actual = 60.0 / (period_frames * hop)
        offset = phase * hop
        beats = np.arange(offset, duration, period_frames * hop)
        downbeats, bpb, _ = pick_downbeats(downbeat_probs, period_frames, phase)
        result = CombResult(
            bpm=bpm_actual,
            offset=offset,
            beats_per_bar=bpb,
            score=score,
            beats=beats,
            downbeats=downbeats,
        )
        if best is None or result.score > best.score:
            best = result
    return best


def render_click_overlay(
    audio: np.ndarray, sr: int, beats: np.ndarray, downbeats: np.ndarray, beat_amp: float = 0.3
) -> np.ndarray:
    length = len(audio)
    beat_clicks = librosa.clicks(times=beats, sr=sr, length=length, click_freq=800, click_duration=0.03)
    down_clicks = librosa.clicks(times=downbeats, sr=sr, length=length, click_freq=1300, click_duration=0.06)
    mix = audio + beat_amp * beat_clicks + beat_amp * 1.5 * down_clicks
    max_abs = np.max(np.abs(mix))
    if max_abs > 1.0:
        mix = mix / max_abs
    return mix.astype(np.float32)


def main() -> None:
    import sys

    song_path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_SONG
    output_audio = (
        Path(__file__).parent / f"{song_path.stem}_fixed_bpm_comb_clicks.wav"
        if len(sys.argv) == 1
        else Path(sys.argv[2]) if len(sys.argv) > 2 else Path(f"{song_path.stem}_fixed_bpm_comb_clicks.wav")
    )

    beat_logits, downbeat_logits, duration, sr, audio = get_logits(song_path)
    beat_probs = sigmoid(beat_logits)
    downbeat_probs = sigmoid(downbeat_logits)

    result = comb_fixed_bpm(beat_probs, downbeat_probs, duration)
    print(f"Comb BPM: {result.bpm:.2f} | offset: {result.offset:.3f}s | beats/bar: {result.beats_per_bar}")
    print(f"{len(result.beats)} beats, {len(result.downbeats)} downbeats")

    overlay = render_click_overlay(audio, sr, result.beats, result.downbeats)
    sf.write(output_audio, overlay, sr)
    print(f"Wrote click demo to {output_audio}")


if __name__ == "__main__":
    torch.set_num_threads(1)
    main()
