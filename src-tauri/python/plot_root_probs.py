#!/usr/bin/env python3
"""
Quick utility to visualize ACE root probabilities for a single audio file.

Usage:
    python plot_root_probs.py /path/to/audio.wav --out roots.png

This runs the same inference path as ace_root_probs_worker.py and produces
an image with:
  - Heatmap of root probabilities over time (one row per pitch class C..B)
  - Top-1 root track overlaid for easy visual alignment
"""

from __future__ import annotations

import argparse
import pathlib
from typing import Tuple

import matplotlib.pyplot as plt
import numpy as np

import ace_root_probs_worker as worker


PITCH_CLASS_LABELS = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]


def compute_roots(
    audio_file: pathlib.Path,
    ckpt: pathlib.Path,
    chunk_dur: float,
    sample_rate: int,
    hop_length: int,
) -> Tuple[np.ndarray, np.ndarray, float]:
    model = worker.load_ace_model(ckpt)
    chunker = worker.make_chunker(audio_file, sample_rate, hop_length, chunk_dur)
    hop_seconds = hop_length / float(sample_rate)

    all_times = []
    all_root_probs = []
    total_frames = 0
    chunk_index = 0

    # Guard against runaway loops.
    import librosa  # type: ignore

    total_dur = librosa.get_duration(path=str(audio_file))
    max_chunks = int(np.ceil(total_dur / chunk_dur)) + 2

    while chunk_index < max_chunks:
        onset = chunk_index * chunk_dur
        if onset - total_dur > hop_seconds:
            break
        features = chunker.process_chunk(onset=onset)
        if features is None:
            break

        if features.ndim == 2:
            features = features.unsqueeze(0).unsqueeze(0)
        elif features.ndim == 3:
            features = features.unsqueeze(0)

        root_logits = worker.predict_root_logits(model, features)
        root_pc_probs = worker.root_probs_from_logits(root_logits)  # [T, 12]

        n_frames = root_pc_probs.shape[0]
        start_time = total_frames * hop_seconds
        times = start_time + np.arange(n_frames, dtype=np.float32) * hop_seconds

        all_times.append(times)
        all_root_probs.append(root_pc_probs)
        total_frames += n_frames
        chunk_index += 1

    if not all_root_probs:
        raise RuntimeError("No frames produced for this audio.")

    frame_times = np.concatenate(all_times, axis=0)
    root_probs = np.concatenate(all_root_probs, axis=0)  # [T, 12]
    return frame_times, root_probs, total_dur


def plot_roots(frame_times: np.ndarray, root_probs: np.ndarray, out_path: pathlib.Path):
    top_idx = np.argmax(root_probs, axis=1)
    plt.figure(figsize=(12, 6))
    ax = plt.subplot(2, 1, 1)
    im = ax.imshow(
        root_probs.T,
        aspect="auto",
        origin="lower",
        interpolation="nearest",
        extent=[frame_times[0], frame_times[-1], -0.5, 11.5],
        vmin=0.0,
        vmax=1.0,
        cmap="viridis",
    )
    ax.set_yticks(range(len(PITCH_CLASS_LABELS)))
    ax.set_yticklabels(PITCH_CLASS_LABELS)
    ax.set_xlabel("Seconds")
    ax.set_title("Root probabilities (ACE)")
    plt.colorbar(im, ax=ax, label="Prob")

    ax_top = plt.subplot(2, 1, 2, sharex=ax)
    ax_top.plot(frame_times, top_idx, color="orange", linewidth=1.5)
    ax_top.set_yticks(range(len(PITCH_CLASS_LABELS)))
    ax_top.set_yticklabels(PITCH_CLASS_LABELS)
    ax_top.set_xlabel("Seconds")
    ax_top.set_ylabel("Top root")
    ax_top.grid(True, linestyle="--", alpha=0.3)
    plt.tight_layout()
    plt.savefig(out_path, dpi=200)
    print(f"[plot_root_probs] wrote {out_path}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Plot ACE root probabilities for an audio file."
    )
    parser.add_argument("audio_file", type=pathlib.Path, help="Path to audio file")
    parser.add_argument(
        "--ckpt",
        type=pathlib.Path,
        default=pathlib.Path("consonance-ACE/ACE/checkpoints/conformer_decomposed_smooth.ckpt"),
        help="Path to ACE checkpoint (default: consonance-ACE/ACE/checkpoints/conformer_decomposed_smooth.ckpt)",
    )
    parser.add_argument(
        "--chunk-dur",
        type=float,
        default=20.0,
        help="Chunk duration seconds (default 20.0)",
    )
    parser.add_argument(
        "--sample-rate",
        type=int,
        default=22050,
        help="ACE sample rate (default 22050)",
    )
    parser.add_argument(
        "--hop-length",
        type=int,
        default=512,
        help="ACE hop length (default 512)",
    )
    parser.add_argument(
        "--out",
        type=pathlib.Path,
        default=pathlib.Path("roots.png"),
        help="Output PNG path (default roots.png)",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    frame_times, root_probs, total_dur = compute_roots(
        args.audio_file,
        args.ckpt,
        args.chunk_dur,
        args.sample_rate,
        args.hop_length,
    )
    print(
        f"[plot_root_probs] frames={len(frame_times)} "
        f"hop={args.hop_length}/{args.sample_rate} ({args.hop_length/args.sample_rate:.4f}s) "
        f"duration={total_dur:.2f}s "
        f"first={frame_times[0]:.3f}s last={frame_times[-1]:.3f}s"
    )
    plot_roots(frame_times, root_probs, args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
