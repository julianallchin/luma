#!/usr/bin/env python3
"""Joint bar classifier worker (legacy 9-tag schema).

Pipeline (all internal — MERT embeddings are NOT persisted, only predictions):

    1. Load MERT-v1-95M from HuggingFace cache (~400 MB; downloads on first run).
    2. Load the bundled bar_classifier.pt checkpoint (BarClassifier, baseline
       arch from TANGO: AttentionPool → trunk MLP → intensity + tag heads).
    3. For each bar in the supplied bar_boundaries:
         - Slice the bar's audio (24 kHz mono).
         - Forward through MERT, take hidden_states[layer 7].
         - Forward those frames through the BarClassifier head.
       Emit one record per bar with intensity + per-tag sigmoid probabilities.

Why discard MERT features: the 768-d frame embeddings would dominate disk
usage (~6 MB per track) yet aren't useful downstream of the classifier in
Luma's lighting flow. Per the explicit user decision, we recompute MERT each
time the classifier runs and keep only the ~20 floats per bar.

Why ship the legacy schema now: the new 7-head schema (intensity + drums /
rhythm / bass / synths / acoustic / vocals) is not trained yet. When the
new model lands, swap the .pt + bump `ClassifierPreprocessor::version` to
trigger automatic backfill.

Legacy 9-tag labels (from `tango/data/models/bar_classifier.pt::tag_order`):
    four_on_floor, half_time_heavy, breakbeat,
    euphoric_lead, vocal_led, ambient_textural,
    build_riser, percussive_sparse, acoustic_organic

CLI:
    classifier_worker.py <audio_file> <weights_file> <bar_boundaries_json>

Where `bar_boundaries_json` is a path to a JSON file:
    [[start_seconds, end_seconds], ...]

Output (stdout, JSON):
    {
      "tag_order": [...],
      "bars": [
        {
          "bar_idx": 0,
          "start": 0.0,
          "end": 1.846,
          "predictions": {
            "intensity": 2.31,
            "four_on_floor": 0.84,
            ...
          }
        },
        ...
      ]
    }
"""

from __future__ import annotations

import argparse
import contextlib
import json
import pathlib
import sys


# Legacy 9-tag schema baked into the bundled checkpoint. Update both this
# constant AND `ClassifierPreprocessor::version` when you swap weights.
LEGACY_TAG_ORDER = [
    "four_on_floor",
    "half_time_heavy",
    "breakbeat",
    "euphoric_lead",
    "vocal_led",
    "ambient_textural",
    "build_riser",
    "percussive_sparse",
    "acoustic_organic",
]

MERT_MODEL_ID = "m-a-p/MERT-v1-95M"
MERT_TARGET_SR = 24000
MERT_LAYER = 7
MIN_BAR_SAMPLES = MERT_TARGET_SR // 10  # < 100 ms = unreliable, skip.


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("audio_file", type=pathlib.Path)
    parser.add_argument("weights_file", type=pathlib.Path)
    parser.add_argument("bar_boundaries_json", type=pathlib.Path)
    parser.add_argument("--batch", type=int, default=8, help="bars per MERT forward pass")
    return parser.parse_args()


def build_bar_classifier(input_dim: int, hidden_dim: int, n_tags: int, dropout: float):
    """Re-implement TANGO's `BarClassifier` (baseline.py) — pool + MLP + heads.

    Kept inline so the worker doesn't depend on TANGO source. Architecture
    must match `bar_classifier.pt::state_dict` keys exactly:

        pool.query
        trunk.0.weight       (LayerNorm)
        trunk.0.bias
        trunk.1.weight       (Linear input_dim → hidden_dim)
        trunk.1.bias
        trunk.4.weight       (Linear hidden_dim → hidden_dim)
        trunk.4.bias
        intensity_head.{weight, bias}
        tag_head.{weight, bias}
    """
    import torch
    from torch import Tensor, nn

    class AttentionPool(nn.Module):
        def __init__(self, dim: int) -> None:
            super().__init__()
            self.query = nn.Parameter(torch.randn(dim) * 0.02)
            self.scale = dim**-0.5

        def forward(self, x: Tensor, mask: Tensor) -> tuple[Tensor, Tensor]:
            scores = torch.einsum("btd,d->bt", x, self.query) * self.scale
            scores = scores.masked_fill(~mask, float("-inf"))
            weights = scores.softmax(dim=-1)
            pooled = torch.einsum("btd,bt->bd", x, weights)
            return pooled, weights

    class BarClassifier(nn.Module):
        def __init__(self) -> None:
            super().__init__()
            self.pool = AttentionPool(input_dim)
            self.trunk = nn.Sequential(
                nn.LayerNorm(input_dim),
                nn.Linear(input_dim, hidden_dim),
                nn.GELU(),
                nn.Dropout(dropout),
                nn.Linear(hidden_dim, hidden_dim),
                nn.GELU(),
                nn.Dropout(dropout),
            )
            self.intensity_head = nn.Linear(hidden_dim, 1)
            self.tag_head = nn.Linear(hidden_dim, n_tags)

        def forward(self, x: Tensor, mask: Tensor) -> tuple[Tensor, Tensor]:
            pooled, _ = self.pool(x, mask)
            h = self.trunk(pooled)
            return self.intensity_head(h).squeeze(-1), self.tag_head(h)

    return BarClassifier()


def emit(payload: dict) -> None:
    sys.stdout.write(json.dumps(payload))
    sys.stdout.flush()


def main() -> int:
    args = parse_args()

    if not args.audio_file.exists():
        print(json.dumps({"error": f"Audio file does not exist: {args.audio_file}"}), file=sys.stderr)
        return 1
    if not args.weights_file.exists():
        print(json.dumps({"error": f"Weights file does not exist: {args.weights_file}"}), file=sys.stderr)
        return 1
    if not args.bar_boundaries_json.exists():
        print(
            json.dumps({"error": f"Bar boundaries JSON does not exist: {args.bar_boundaries_json}"}),
            file=sys.stderr,
        )
        return 1

    # Third-party libs (transformers / MERT trust_remote_code module) print
    # warnings to stdout. Stdout is reserved for our JSON payload, so route
    # everything else to stderr while we work; final `emit()` writes the JSON
    # outside this block.
    bars_out: list[dict] = []
    with contextlib.redirect_stdout(sys.stderr):
        try:
            import librosa
            import numpy as np
            import torch
            from transformers import AutoModel, Wav2Vec2FeatureExtractor
        except Exception as exc:  # pragma: no cover - import error reporting
            print(json.dumps({"error": f"Missing python deps for classifier: {exc}"}), file=sys.stderr)
            return 1

        boundaries = json.loads(args.bar_boundaries_json.read_text(encoding="utf-8"))
        if not isinstance(boundaries, list) or not boundaries:
            print(json.dumps({"error": "bar_boundaries_json must be a non-empty list"}), file=sys.stderr)
            return 1

        device = "cuda" if torch.cuda.is_available() else "cpu"

        try:
            # MERT (heavy — ~400 MB on first run, then HF cache).
            print(f"[classifier] loading MERT ({MERT_MODEL_ID}) on {device}", file=sys.stderr, flush=True)
            mert = AutoModel.from_pretrained(MERT_MODEL_ID, trust_remote_code=True).eval().to(device)
            for p in mert.parameters():
                p.requires_grad_(False)
            processor = Wav2Vec2FeatureExtractor.from_pretrained(MERT_MODEL_ID, trust_remote_code=True)

            # BarClassifier head from bundled .pt.
            ckpt = torch.load(str(args.weights_file), map_location=device, weights_only=False)
            cfg = ckpt["config"]
            tag_order = ckpt.get("tag_order") or LEGACY_TAG_ORDER
            if list(tag_order) != LEGACY_TAG_ORDER:
                # Defensive — protect downstream JSON consumers from silent schema drift.
                print(
                    json.dumps(
                        {"error": f"Checkpoint tag_order {tag_order} does not match expected legacy schema"}
                    ),
                    file=sys.stderr,
                )
                return 1
            head = build_bar_classifier(
                input_dim=int(cfg["input_dim"]),
                hidden_dim=int(cfg["hidden_dim"]),
                n_tags=int(cfg["n_tags"]),
                dropout=float(cfg.get("dropout", 0.2)),
            ).to(device)
            head.load_state_dict(ckpt["state_dict"])
            head.eval()

            # Load audio once; bar segmentation happens per-bar.
            y, _ = librosa.load(str(args.audio_file), sr=MERT_TARGET_SR, mono=True)
            total_samples = len(y)

            for batch_start in range(0, len(boundaries), args.batch):
                batch = list(enumerate(boundaries))[batch_start : batch_start + args.batch]
                audios: list[np.ndarray] = []
                keep: list[tuple[int, float, float]] = []
                for bar_idx, (start_s, end_s) in batch:
                    s = max(0, round(float(start_s) * MERT_TARGET_SR))
                    e = min(total_samples, round(float(end_s) * MERT_TARGET_SR))
                    seg = y[s:e]
                    if len(seg) < MIN_BAR_SAMPLES:
                        continue
                    audios.append(seg.astype(np.float32))
                    keep.append((bar_idx, float(start_s), float(end_s)))
                if not audios:
                    continue

                inputs = processor(
                    audios, sampling_rate=MERT_TARGET_SR, return_tensors="pt", padding=True
                ).to(device)
                with torch.no_grad():
                    outputs = mert(**inputs, output_hidden_states=True)
                    feats = outputs.hidden_states[MERT_LAYER]  # (B, T_max, 768)

                    if "attention_mask" in inputs:
                        sample_lens = inputs["attention_mask"].sum(-1)
                    else:
                        sample_lens = torch.tensor([a.shape[0] for a in audios], device=device)
                    frame_lens = mert._get_feat_extract_output_lengths(sample_lens)

                    # Build (T_max) frame mask per row from frame_lens.
                    t_max = feats.shape[1]
                    arange = torch.arange(t_max, device=device).unsqueeze(0)  # (1, T_max)
                    frame_mask = arange < frame_lens.unsqueeze(1)  # (B, T_max)

                    intensity, tag_logits = head(feats, frame_mask)
                    intensity = intensity.clamp(0.0, 5.0).cpu().numpy()
                    probs = torch.sigmoid(tag_logits).cpu().numpy()

                for j, (bar_idx, s, e) in enumerate(keep):
                    preds = {"intensity": float(intensity[j])}
                    for ti, tag_name in enumerate(LEGACY_TAG_ORDER):
                        preds[tag_name] = float(probs[j, ti])
                    bars_out.append({"bar_idx": int(bar_idx), "start": s, "end": e, "predictions": preds})
        except Exception as exc:  # pragma: no cover - runtime error reporting
            print(json.dumps({"error": str(exc)}), file=sys.stderr)
            return 1

    emit({"tag_order": LEGACY_TAG_ORDER, "bars": bars_out})
    return 0


if __name__ == "__main__":  # pragma: no cover - script entrypoint
    raise SystemExit(main())
