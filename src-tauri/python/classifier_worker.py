#!/usr/bin/env python3
"""Joint bar classifier worker (windowed 22-tag schema).

Pipeline (all internal — MERT embeddings are NOT persisted, only predictions):

    1. Load MERT-v1-95M from HuggingFace cache (~400 MB; downloads on first run).
    2. Load the bundled bar_window_classifier.pt checkpoint (BarWindowClassifier:
       per-bar AttentionPool → temporal transformer over a W=5 bar window →
       per-bar intensity + tag heads). Mirrors TANGO's
       `tango.classifier.model.BarWindowClassifier`.
    3. For each bar in the supplied bar_boundaries:
         - Slice the bar's audio (24 kHz mono) and forward through MERT.
         - Take hidden_states[layer 7] → (T_bar, 768) per-bar features.
       Then for each center bar, build a (W=5, T_max, 768) window by stacking
       the surrounding bars (zero-padded + bar_mask=False at track edges) and
       run the windowed head. Only the center prediction (index half=2) is
       emitted per bar.

Why discard MERT features: the 768-d frame embeddings would dominate disk
usage (~6 MB per track) yet aren't useful downstream of the classifier in
Luma's lighting flow. Per the explicit user decision, we recompute MERT each
time the classifier runs and keep only the ~22 floats per bar.

22-tag schema (from `tango/data/models/bar_window_classifier.pt::tag_order`,
6 multi-label heads concatenated in HEADS order):

    drums:    hats, kick, snare, perc, fill, impact
    rhythm:   four_four, halftime, breakbeat, build
    bass:     pluck, sustain
    synths:   arp, pad, lead, riser
    acoustic: piano, acoustic_guitar, electric_guitar, other
    vocals:   vocal_lead, vocal_chop

CLI:
    classifier_worker.py <audio_file> <weights_file>

Bar boundaries are read from stdin as JSON:
    [[start_seconds, end_seconds], ...]

Stdin avoids a shared temp-file race when multiple classifier workers run
concurrently — each child gets its own pipe.

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
            "kick": 0.84,
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


# 22-tag schema baked into bar_window_classifier.pt. Update both this constant
# AND `ClassifierPreprocessor::version` when you swap weights.
TAG_ORDER = [
    "hats",
    "kick",
    "snare",
    "perc",
    "fill",
    "impact",
    "four_four",
    "halftime",
    "breakbeat",
    "build",
    "pluck",
    "sustain",
    "arp",
    "pad",
    "lead",
    "riser",
    "piano",
    "acoustic_guitar",
    "electric_guitar",
    "other",
    "vocal_lead",
    "vocal_chop",
]

MERT_MODEL_ID = "m-a-p/MERT-v1-95M"
MERT_TARGET_SR = 24000
MERT_LAYER = 7
MIN_BAR_SAMPLES = MERT_TARGET_SR // 10  # < 100 ms = unreliable, skip.
MERT_BATCH = 8  # bars per MERT forward pass
WINDOW_BATCH = 32  # windows per BarWindowClassifier forward pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("audio_file", type=pathlib.Path)
    parser.add_argument("weights_file", type=pathlib.Path)
    return parser.parse_args()


def build_bar_window_classifier(
    input_dim: int,
    hidden_dim: int,
    n_tags: int,
    window_size: int,
    n_layers: int,
    n_heads: int,
    dropout: float,
):
    """Re-implement TANGO's `BarWindowClassifier` (model.py) inline.

    Architecture must match `bar_window_classifier.pt::state_dict` keys:
        pool.query
        proj.0.{weight, bias}        (LayerNorm input_dim)
        proj.1.{weight, bias}        (Linear input_dim → hidden_dim)
        pos_embed
        encoder.layers.{i}...        (TransformerEncoderLayer × n_layers, norm_first=True)
        head_norm.{weight, bias}
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

    class BarWindowClassifier(nn.Module):
        def __init__(self) -> None:
            super().__init__()
            self.window_size = window_size
            self.pool = AttentionPool(input_dim)
            self.proj = nn.Sequential(
                nn.LayerNorm(input_dim),
                nn.Linear(input_dim, hidden_dim),
                nn.Dropout(dropout),
            )
            self.pos_embed = nn.Parameter(torch.randn(window_size, hidden_dim) * 0.02)
            encoder_layer = nn.TransformerEncoderLayer(
                d_model=hidden_dim,
                nhead=n_heads,
                dim_feedforward=hidden_dim * 4,
                dropout=dropout,
                activation="gelu",
                batch_first=True,
                norm_first=True,
            )
            self.encoder = nn.TransformerEncoder(encoder_layer, num_layers=n_layers)
            self.head_norm = nn.LayerNorm(hidden_dim)
            self.intensity_head = nn.Linear(hidden_dim, 1)
            self.tag_head = nn.Linear(hidden_dim, n_tags)

        def forward(
            self, embeddings: Tensor, frame_mask: Tensor, bar_mask: Tensor
        ) -> tuple[Tensor, Tensor]:
            b, w, t, d = embeddings.shape
            flat_x = embeddings.reshape(b * w, t, d)
            flat_mask = frame_mask.reshape(b * w, t).clone()
            empty = ~flat_mask.any(dim=-1)
            flat_mask[empty, 0] = True
            pooled, _ = self.pool(flat_x, flat_mask)
            pooled[empty] = 0.0
            pooled = pooled.reshape(b, w, d)
            h = self.proj(pooled) + self.pos_embed[:w]
            out = self.encoder(h, src_key_padding_mask=~bar_mask)
            out = self.head_norm(out)
            return self.intensity_head(out).squeeze(-1), self.tag_head(out)

    return BarWindowClassifier()


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

    boundaries_raw = sys.stdin.read()
    if not boundaries_raw.strip():
        print(json.dumps({"error": "No bar boundaries received on stdin"}), file=sys.stderr)
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

        boundaries = json.loads(boundaries_raw)
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

            # BarWindowClassifier head from bundled .pt.
            ckpt = torch.load(str(args.weights_file), map_location=device, weights_only=False)
            cfg = ckpt["config"]
            tag_order = ckpt.get("tag_order")
            if list(tag_order) != TAG_ORDER:
                # Defensive — protect downstream JSON consumers from silent schema drift.
                print(
                    json.dumps(
                        {"error": f"Checkpoint tag_order {tag_order} does not match expected schema"}
                    ),
                    file=sys.stderr,
                )
                return 1
            head = build_bar_window_classifier(
                input_dim=int(cfg["input_dim"]),
                hidden_dim=int(cfg["hidden_dim"]),
                n_tags=int(cfg["n_tags"]),
                window_size=int(cfg["window_size"]),
                n_layers=int(cfg["n_layers"]),
                n_heads=int(cfg["n_heads"]),
                dropout=float(cfg["dropout"]),
            ).to(device)
            head.load_state_dict(ckpt["state_dict"])
            head.eval()

            window_size = int(cfg["window_size"])
            half = window_size // 2

            # ---------------------------------------------------------------
            # Stage 1: per-bar MERT features. We compute (T_bar, 768) features
            # for every bar in `boundaries`, keeping them on CPU as a list
            # (varying T_bar). Bars too short to score get a None placeholder.
            # ---------------------------------------------------------------
            y, _ = librosa.load(str(args.audio_file), sr=MERT_TARGET_SR, mono=True)
            total_samples = len(y)
            n_bars = len(boundaries)
            per_bar_feats: list[np.ndarray | None] = [None] * n_bars

            for batch_start in range(0, n_bars, MERT_BATCH):
                batch_indices = list(range(batch_start, min(batch_start + MERT_BATCH, n_bars)))
                audios: list[np.ndarray] = []
                keep_indices: list[int] = []
                for bar_idx in batch_indices:
                    start_s, end_s = boundaries[bar_idx]
                    s = max(0, round(float(start_s) * MERT_TARGET_SR))
                    e = min(total_samples, round(float(end_s) * MERT_TARGET_SR))
                    seg = y[s:e]
                    if len(seg) < MIN_BAR_SAMPLES:
                        continue
                    audios.append(seg.astype(np.float32))
                    keep_indices.append(bar_idx)
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
                    frame_lens = mert._get_feat_extract_output_lengths(sample_lens).cpu().numpy()

                feats_cpu = feats.cpu().numpy()
                for j, bar_idx in enumerate(keep_indices):
                    fl = int(frame_lens[j])
                    per_bar_feats[bar_idx] = feats_cpu[j, :fl].copy()

            # Free MERT now that we're done with it.
            del mert, processor
            if device == "cuda":
                torch.cuda.empty_cache()

            # ---------------------------------------------------------------
            # Stage 2: assemble W=5 windows centered on each scorable bar and
            # run BarWindowClassifier in batches. Only the center bar's
            # prediction is emitted (the windowed model attends across
            # neighbors but only its center output is calibrated).
            # ---------------------------------------------------------------
            scorable: list[int] = [i for i, f in enumerate(per_bar_feats) if f is not None]
            d_emb = next(f.shape[1] for f in per_bar_feats if f is not None)

            for win_start in range(0, len(scorable), WINDOW_BATCH):
                centers = scorable[win_start : win_start + WINDOW_BATCH]
                # T_max across this batch's windows (5 bars per center).
                t_max = 1
                for center in centers:
                    for off in range(-half, half + 1):
                        nb = center + off
                        if 0 <= nb < n_bars and per_bar_feats[nb] is not None:
                            t_max = max(t_max, per_bar_feats[nb].shape[0])

                b = len(centers)
                emb = np.zeros((b, window_size, t_max, d_emb), dtype=np.float32)
                fmask = np.zeros((b, window_size, t_max), dtype=bool)
                bmask = np.zeros((b, window_size), dtype=bool)
                for i, center in enumerate(centers):
                    for j, off in enumerate(range(-half, half + 1)):
                        nb = center + off
                        if 0 <= nb < n_bars and per_bar_feats[nb] is not None:
                            arr = per_bar_feats[nb]
                            t = arr.shape[0]
                            emb[i, j, :t] = arr
                            fmask[i, j, :t] = True
                            bmask[i, j] = True

                emb_t = torch.from_numpy(emb).to(device)
                fmask_t = torch.from_numpy(fmask).to(device)
                bmask_t = torch.from_numpy(bmask).to(device)
                with torch.no_grad():
                    intensity_w, tag_logits_w = head(emb_t, fmask_t, bmask_t)
                    intensity_c = intensity_w[:, half].clamp(0.0, 5.0).cpu().numpy()
                    probs_c = torch.sigmoid(tag_logits_w[:, half]).cpu().numpy()

                for i, center in enumerate(centers):
                    start_s, end_s = boundaries[center]
                    preds = {"intensity": float(intensity_c[i])}
                    for ti, tag_name in enumerate(TAG_ORDER):
                        preds[tag_name] = float(probs_c[i, ti])
                    bars_out.append(
                        {
                            "bar_idx": int(center),
                            "start": float(start_s),
                            "end": float(end_s),
                            "predictions": preds,
                        }
                    )
        except Exception as exc:  # pragma: no cover - runtime error reporting
            print(json.dumps({"error": str(exc)}), file=sys.stderr)
            return 1

    bars_out.sort(key=lambda r: r["bar_idx"])
    emit({"tag_order": TAG_ORDER, "bars": bars_out})
    return 0


if __name__ == "__main__":  # pragma: no cover - script entrypoint
    raise SystemExit(main())
