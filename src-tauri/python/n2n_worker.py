#!/usr/bin/env python3
"""n2n drum-onset worker.

Wraps the vendored `n2n` package (./n2n/) — the ADT model from
`julianallchin/n2n` (paper-aligned reproduction of Yeung et al., Sony AI 2025).
Takes one full-mix audio file plus a precomputed MERT-95M layer-7 cache (.npy
at 75 Hz, fp16), runs the model in overlapping windows, peak-picks per drum
class, and emits onset timestamps on stdout as JSON.

The vendored model can be either v11 (diffusion, Heun sampler) or v12+
(discriminative sigmoid head, single forward pass). The branch is taken from
the checkpoint's `cfg["model"]["no_diffusion"]` flag inside `n2n.infer`, so
this worker treats both modes uniformly. The bundled `weights.pt` is currently
v12 (run012, step 42000).

Output schema (4-class native to v6+ checkpoints):
    {
        "onsets": {
            "kick":   [<seconds>, ...],
            "snare":  [<seconds>, ...],
            "hat":    [<seconds>, ...],
            "cymbal": [<seconds>, ...]
        }
    }

⚠ Distribution shift: v6+ checkpoints were trained on drum-isolated stems and
ADTOF/synthetic full mixes. Running inference on the full mix moves both
conditioning streams (mel + MERT) partway off the drum-only training
distribution; v12's ADTOF mix component closes most of that gap. Validate
output quality on a representative track when upgrading the bundled
checkpoint.

Window / stride for the sliding sampler are read from the checkpoint config
under `infer.window_seconds` / `infer.stride_seconds` (15 s / 12 s for the
bundled v12 checkpoint, 30 s / 24 s for older v10/v11 ckpts).
"""

from __future__ import annotations

import argparse
import contextlib
import json
import pathlib
import sys

# Peak-pick threshold defaults by checkpoint family. v11 diffusion outputs are
# in [-1, +1], so 0.0 splits the bipolar onset/no-onset target. v12 sigmoid
# outputs are in [0, 1]; the training-time cfg default is 0.5, but the run012
# threshold sweep against ADTOF F1 (see logs/v12_threshold_sweep.log in the n2n
# repo) put the peak at 0.9. We hard-code that here so the bundled v12 ckpt
# gets the calibrated threshold regardless of what cfg.peak_pick_threshold was
# baked in at training time. `--threshold` on the CLI still overrides.
DEFAULT_THRESHOLD_V11_DIFFUSION = 0.0
DEFAULT_THRESHOLD_V12_NODIFFUSION = 0.9


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
        help="EDM Heun sampler steps. 5 is the trained inference setting; 10 trades latency for marginal F1. Ignored for v12+ (no_diffusion) checkpoints.",
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=None,
        help=(
            "Peak-pick threshold override. If omitted, defaults are picked per "
            f"checkpoint family: v11 diffusion → {DEFAULT_THRESHOLD_V11_DIFFUSION} "
            f"(outputs in [-1, +1]); v12 sigmoid head → "
            f"{DEFAULT_THRESHOLD_V12_NODIFFUSION} (ADTOF-F1 peak per the run012 "
            "threshold sweep)."
        ),
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

            # Peak-pick threshold depends on output range, picked per
            # checkpoint family. See the module-level constants for rationale.
            no_diffusion = bool(cfg.get("model", {}).get("no_diffusion", False))
            if args.threshold is not None:
                onset_threshold = float(args.threshold)
            elif no_diffusion:
                onset_threshold = DEFAULT_THRESHOLD_V12_NODIFFUSION
            else:
                onset_threshold = DEFAULT_THRESHOLD_V11_DIFFUSION

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
            raw = peak_pick(target, onset_threshold=onset_threshold)
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
