#!/usr/bin/env python3
"""MERT-95M feature-extraction worker.

Computes per-track MERT-v1-95M layer-7 hidden states from two inputs — the
full-mix audio and the demucs drum stem — and writes each to its own fp16
numpy cache. Both passes happen inside this single process so MERT-95M is
loaded into memory (and GPU/MPS) exactly once per track, which dominates
runtime on consumer laptops.

The full-mix cache feeds the bar classifier (per-bar slicing for the
22-tag windowed head). The drum-stem cache feeds the n2n drum-onset model
— v6+ checkpoints were trained on drum-isolated stems, so its MERT
conditioning has to be on the drum stem, not the full mix.

Algorithm: delegated to `n2n.infer.compute_mert_features` so chunking
parameters (window/overlap/crop) stay locked to the same defaults the
training pipeline and `python -m n2n.infer` use. Duplicating the math here
once cost us a ~60× drop in detected hats on PARTY 4 U after we noticed our
worker was using 60/30/5 while training used 30/15/3 — silent
distribution shift. Single source of truth from now on.

CLI:
    mert_worker.py --fullmix <wav> --drum <wav> \
        --out-fullmix <cache_path> --out-drum <cache_path>

Output (stdout, JSON):
    {
        "fullmix_path": "...",
        "drum_path": "...",
        "fullmix_frames": N,
        "drum_frames": M,
        "frames_per_second": 75,
        "layer": 7,
        "model_id": "m-a-p/MERT-v1-95M"
    }
"""

from __future__ import annotations

import argparse
import contextlib
import json
import pathlib
import sys


MERT_MODEL_ID = "m-a-p/MERT-v1-95M"
MERT_FRAMES_PER_SECOND = 75
MERT_LAYER = 7
# `n2n.infer.compute_mert_features` consumes audio at cfg["mel"]["sample_rate"]
# (the n2n training rate) and resamples to 24 kHz internally. Hard-coding here
# keeps us aligned with the bundled v12 checkpoint without parsing it.
N2N_TARGET_SAMPLE_RATE = 44_100


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--fullmix", type=pathlib.Path, required=True,
                   help="Path to the full-mix audio file.")
    p.add_argument("--drum", type=pathlib.Path, required=True,
                   help="Path to the demucs drum stem audio file.")
    p.add_argument("--out-fullmix", type=pathlib.Path, required=True,
                   help="Where to write the full-mix .npy cache.")
    p.add_argument("--out-drum", type=pathlib.Path, required=True,
                   help="Where to write the drum-stem .npy cache.")
    return p.parse_args()


def _atomic_write_npy(features, out_path: pathlib.Path) -> None:
    """Write `.npy` via tmp → rename so a killed process can't leave a
    truncated cache visible to consumers."""
    import numpy as np

    out_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = out_path.with_suffix(out_path.suffix + ".tmp")
    with tmp.open("wb") as f:
        np.save(f, features.astype(np.float16), allow_pickle=False)
    tmp.replace(out_path)


def main() -> int:
    args = parse_args()
    for label, path in (("fullmix", args.fullmix), ("drum", args.drum)):
        if not path.exists():
            print(json.dumps({"error": f"{label} audio does not exist: {path}"}),
                  file=sys.stderr)
            return 1

    # Heavy imports + transformers logging redirected to stderr; stdout is
    # reserved for the final JSON payload.
    fullmix_frames = 0
    drum_frames = 0
    with contextlib.redirect_stdout(sys.stderr):
        try:
            import torch
            from transformers import AutoFeatureExtractor, AutoModel
            from n2n.infer import compute_mert_features, load_audio
        except Exception as exc:
            print(json.dumps({"error": f"Failed to import deps: {exc}"}), file=sys.stderr)
            return 1

        try:
            if torch.cuda.is_available():
                device = torch.device("cuda")
            elif torch.backends.mps.is_available():
                device = torch.device("mps")
            else:
                device = torch.device("cpu")

            fe = AutoFeatureExtractor.from_pretrained(MERT_MODEL_ID, trust_remote_code=True)
            model = AutoModel.from_pretrained(MERT_MODEL_ID, trust_remote_code=True).to(device).eval()
            for p in model.parameters():
                p.requires_grad_(False)

            # Minimal cfg surface used by `compute_mert_features`. Matches the
            # bundled n2n v12 checkpoint's relevant fields.
            cfg = {
                "data": {"mert_layer": MERT_LAYER},
                "mel": {"sample_rate": N2N_TARGET_SAMPLE_RATE},
            }

            for label, audio_path, out_path in (
                ("fullmix", args.fullmix, args.out_fullmix),
                ("drum", args.drum, args.out_drum),
            ):
                # `load_audio` from n2n.infer matches what `n2n.infer.main()`
                # does on the training machine: load + mono-mix + resample to
                # the n2n target SR; then compute_mert_features handles the
                # second resample to 24 kHz internally.
                audio = load_audio(audio_path, target_sr=N2N_TARGET_SAMPLE_RATE)
                features = compute_mert_features(
                    audio,
                    cfg,
                    device,
                    mert_model=model,
                    feature_extractor=fe,
                )  # (T, 768) fp32 on CPU — defaults: 30 s / 15 s / 3 s
                features_np = features.cpu().numpy()
                _atomic_write_npy(features_np, out_path)
                if label == "fullmix":
                    fullmix_frames = int(features_np.shape[0])
                else:
                    drum_frames = int(features_np.shape[0])
        except Exception as exc:
            print(json.dumps({"error": str(exc)}), file=sys.stderr)
            return 1

    sys.stdout.write(json.dumps({
        "fullmix_path": str(args.out_fullmix),
        "drum_path": str(args.out_drum),
        "fullmix_frames": fullmix_frames,
        "drum_frames": drum_frames,
        "frames_per_second": MERT_FRAMES_PER_SECOND,
        "layer": MERT_LAYER,
        "model_id": MERT_MODEL_ID,
    }))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
