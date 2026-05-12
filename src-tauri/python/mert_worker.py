#!/usr/bin/env python3
"""MERT-95M feature-extraction worker.

Computes per-track MERT-v1-95M layer-7 hidden states for the full-mix audio,
caches them as fp16 numpy arrays. The cache is consumed by both the bar
classifier (per-bar slicing) and the n2n drum-onset preprocessor (full-song
sliding-window inference), so we pay MERT extraction once per track instead
of twice.

Algorithm: overlap-add chunked extraction from `n2n.infer.compute_mert_features`
adapted to write features to disk. The 60 s chunks with 30 s overlap and a
5 s edge taper smooth out MERT's CNN feature-extractor zero-pad artifacts at
chunk boundaries — see the `compute_mert_features` docstring upstream for
the long-form rationale.

CLI:
    mert_worker.py <audio_file> --out <cache_path>

Output: writes a numpy fp16 array of shape (n_frames, 768) at 75 Hz to
`<cache_path>` and emits `{"path": "<cache_path>", "n_frames": N}` on stdout.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import pathlib
import sys


MERT_MODEL_ID = "m-a-p/MERT-v1-95M"
MERT_SAMPLE_RATE = 24_000
MERT_FRAMES_PER_SECOND = 75
MERT_LAYER = 7
CHUNK_SECONDS = 60.0
OVERLAP_SECONDS = 30.0
CROP_SECONDS = 5.0


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("audio_file", type=pathlib.Path)
    p.add_argument("--out", type=pathlib.Path, required=True,
                   help="Where to write the .npy cache file.")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    if not args.audio_file.exists():
        print(json.dumps({"error": f"Audio file does not exist: {args.audio_file}"}),
              file=sys.stderr)
        return 1

    args.out.parent.mkdir(parents=True, exist_ok=True)

    # Heavy imports + transformers logging redirected to stderr; stdout reserved
    # for the final JSON payload.
    n_frames: int = 0
    with contextlib.redirect_stdout(sys.stderr):
        try:
            import numpy as np
            import soundfile as sf
            import torch
            from transformers import AutoFeatureExtractor, AutoModel
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

            # Load + resample to 24 kHz mono.
            wav, sr = sf.read(str(args.audio_file), dtype="float32", always_2d=False)
            if wav.ndim > 1:
                wav = wav.mean(axis=1)
            if sr != MERT_SAMPLE_RATE:
                # Resample. torchaudio's resample is high-quality; soundfile/librosa
                # alternatives have subtle freq-response differences that would shift
                # the MERT input distribution away from training. Stay on torchaudio.
                import torchaudio
                wav_t = torch.from_numpy(wav).unsqueeze(0)
                wav_t = torchaudio.functional.resample(wav_t, sr, MERT_SAMPLE_RATE)
                wav = wav_t.squeeze(0).numpy()

            fe = AutoFeatureExtractor.from_pretrained(MERT_MODEL_ID, trust_remote_code=True)
            model = AutoModel.from_pretrained(MERT_MODEL_ID, trust_remote_code=True).to(device).eval()
            for p in model.parameters():
                p.requires_grad_(False)

            chunk_samples = int(CHUNK_SECONDS * MERT_SAMPLE_RATE)
            overlap_samples = int(OVERLAP_SECONDS * MERT_SAMPLE_RATE)
            stride_samples = chunk_samples - overlap_samples
            crop_frames = int(round(CROP_SECONDS * MERT_FRAMES_PER_SECOND))

            n_samples = wav.shape[0]
            starts = list(range(0, max(1, n_samples - chunk_samples + 1), stride_samples))
            if not starts or starts[-1] + chunk_samples < n_samples:
                starts.append(max(0, n_samples - chunk_samples))

            total_frames = int(round(n_samples / MERT_SAMPLE_RATE * MERT_FRAMES_PER_SECOND))
            hidden_dim: int | None = None
            out_sum: np.ndarray | None = None
            out_weight: np.ndarray | None = None

            with torch.no_grad():
                for s_audio in starts:
                    chunk = wav[s_audio:s_audio + chunk_samples]
                    if chunk.shape[0] < int(0.1 * MERT_SAMPLE_RATE):
                        continue
                    inputs = fe(chunk, sampling_rate=MERT_SAMPLE_RATE, return_tensors="pt")
                    iv = inputs["input_values"].to(device)
                    out = model(iv, output_hidden_states=True)
                    feat = out.hidden_states[MERT_LAYER].squeeze(0).float().cpu().numpy()

                    if hidden_dim is None:
                        hidden_dim = feat.shape[1]
                        out_sum = np.zeros((total_frames, hidden_dim), dtype=np.float32)
                        out_weight = np.zeros((total_frames,), dtype=np.float32)

                    n_chunk_frames = feat.shape[0]
                    weight = np.ones(n_chunk_frames, dtype=np.float32)
                    ramp = min(crop_frames, n_chunk_frames // 2)
                    if ramp > 0:
                        ramp_vals = np.linspace(0.0, 1.0, ramp + 1, dtype=np.float32)[1:]
                        weight[:ramp] = ramp_vals
                        weight[-ramp:] = ramp_vals[::-1]

                    s_frame = int(round(s_audio / MERT_SAMPLE_RATE * MERT_FRAMES_PER_SECOND))
                    end_frame = min(s_frame + n_chunk_frames, total_frames)
                    n_emit = end_frame - s_frame
                    out_sum[s_frame:end_frame] += feat[:n_emit] * weight[:n_emit, None]
                    out_weight[s_frame:end_frame] += weight[:n_emit]

            if out_sum is None or out_weight is None or hidden_dim is None:
                print(json.dumps({"error": "No MERT chunks were processed"}), file=sys.stderr)
                return 1

            safe_w = np.clip(out_weight, 1e-6, None)
            features = (out_sum / safe_w[:, None]).astype(np.float16)
            n_frames = features.shape[0]

            # Atomic write: tmp file → rename. Avoids leaving truncated caches if
            # the process gets killed mid-write.
            tmp = args.out.with_suffix(args.out.suffix + ".tmp")
            with tmp.open("wb") as f:
                np.save(f, features, allow_pickle=False)
            tmp.replace(args.out)
        except Exception as exc:
            print(json.dumps({"error": str(exc)}), file=sys.stderr)
            return 1

    sys.stdout.write(json.dumps({
        "path": str(args.out),
        "n_frames": int(n_frames),
        "frames_per_second": MERT_FRAMES_PER_SECOND,
        "layer": MERT_LAYER,
        "model_id": MERT_MODEL_ID,
    }))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
