#!/usr/bin/env python3
"""n2n drum-onset worker.

Wraps the vendored `n2n` package (./n2n/) which is the diffusion-based ADT
model from `julianallchin/n2n` (a paper-aligned reproduction of Yeung et al.,
Sony AI 2025). Takes one full-mix audio file plus a precomputed MERT-95M
layer-7 cache (.npy at 75 Hz, fp16), runs the EDM sampler in overlapping
windows, peak-picks per drum class, and emits onset timestamps on stdout
as JSON.

Output schema (4-class native to v6+ checkpoints):
    {
        "onsets": {
            "kick":   [<seconds>, ...],
            "snare":  [<seconds>, ...],
            "hat":    [<seconds>, ...],
            "cymbal": [<seconds>, ...]
        }
    }

⚠ Distribution shift: v6+ checkpoints (including the bundled v10) were trained
on drum-isolated stems. Running inference on the full mix is faster (no demucs
gate) and architecturally simpler (one MERT cache shared with the bar
classifier) but moves both conditioning streams (mel + MERT) off the trained
input distribution. Validate output quality on a representative track when
upgrading the bundled checkpoint.

Window / stride for the sliding sampler are read from the checkpoint config
under `infer.window_seconds` / `infer.stride_seconds` (30 s / 24 s for the
bundled v10 checkpoint).
"""

from __future__ import annotations

import argparse
import contextlib
import json
import pathlib
import sys


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Transcribe drum onsets from a full-mix track via n2n.",
    )
    parser.add_argument(
        "audio_file",
        type=pathlib.Path,
        help="Path to the full-mix audio file.",
    )
    parser.add_argument(
        "--ckpt",
        type=pathlib.Path,
        required=True,
        help="Path to the n2n checkpoint (.pt file with ema + config).",
    )
    parser.add_argument(
        "--mert",
        type=pathlib.Path,
        required=True,
        help="Path to the precomputed MERT layer-7 cache (.npy fp16 at 75 Hz).",
    )
    parser.add_argument(
        "--num-steps",
        type=int,
        default=5,
        help="EDM Heun sampler steps. 5 is the trained inference setting; 10 trades latency for marginal F1.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    if not args.audio_file.exists():
        print(
            json.dumps({"error": f"Audio file does not exist: {args.audio_file}"}),
            file=sys.stderr,
        )
        return 1
    if not args.ckpt.exists():
        print(
            json.dumps({"error": f"n2n checkpoint does not exist: {args.ckpt}"}),
            file=sys.stderr,
        )
        return 1
    if not args.mert.exists():
        print(
            json.dumps({"error": f"MERT cache does not exist: {args.mert}"}),
            file=sys.stderr,
        )
        return 1

    # Third-party libs (transformers / soundfile) and the EDM sampler print
    # status to stdout. Stdout is reserved for our JSON payload, so route
    # everything else to stderr while we work; final `emit` writes outside
    # this block.
    onsets: dict[str, list[float]] | None = None
    with contextlib.redirect_stdout(sys.stderr):
        try:
            import numpy as np
            import torch
            from n2n.data.drum_mapping import CLASS_NAMES
            from n2n.infer import (
                DEFAULT_FRAMES_PER_SECOND,
                load_audio,
                load_checkpoint,
                peak_pick,
                to_drum_events,
                transcribe_sliding,
            )
        except Exception as exc:
            print(
                json.dumps({"error": f"Failed to import n2n: {exc}"}),
                file=sys.stderr,
            )
            return 1

        try:
            device = torch.device("cuda" if torch.cuda.is_available() else "cpu")

            model, cfg = load_checkpoint(args.ckpt, device)
            model.eval()

            audio = load_audio(args.audio_file, target_sr=cfg["mel"]["sample_rate"])

            # MERT cache is fp16 (T_mert, 768) at 75 Hz, prepared by
            # mert_worker.py with overlap-add chunking. Promote to fp32 for
            # downstream torch ops; ~27 MB at fp32 for a 4-min track is fine
            # for CPU memory.
            mert_np = np.load(args.mert).astype(np.float32)
            mert = torch.from_numpy(mert_np)

            infer_cfg = cfg.get("infer", {}) or {}
            window_seconds = float(infer_cfg.get("window_seconds", 5.0))
            stride_seconds = float(infer_cfg.get("stride_seconds", window_seconds * 0.8))

            target = transcribe_sliding(
                model,
                cfg,
                audio,
                mert,
                num_steps=args.num_steps,
                device=device,
                window_seconds=window_seconds,
                stride_seconds=stride_seconds,
            )
            raw = peak_pick(target)
            events = to_drum_events(raw, fps=DEFAULT_FRAMES_PER_SECOND)

            onsets = {name: [] for name in CLASS_NAMES}
            for ev in events:
                onsets[ev.drum_name].append(float(ev.time_seconds))
        except Exception as exc:
            print(json.dumps({"error": str(exc)}), file=sys.stderr)
            return 1

    sys.stdout.write(json.dumps({"onsets": onsets}))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover - script entrypoint
    raise SystemExit(main())
