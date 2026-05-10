#!/usr/bin/env python3
"""Joint bar classifier worker (windowed 22-tag schema).

Pipeline:

    1. Load the precomputed MERT-95M layer-7 cache (.npy fp16 at 75 Hz)
       written by `mert_worker.py`. The cache is shared with the n2n
       drum-onset preprocessor — one MERT extraction per track.
    2. Load the bundled bar_window_classifier.pt checkpoint (BarWindowClassifier:
       per-bar AttentionPool → temporal transformer over a W=5 bar window →
       per-bar intensity + tag heads). Mirrors TANGO's
       `tango.classifier.model.BarWindowClassifier`.
    3. For each bar in the supplied bar_boundaries:
         - Slice the bar's frames out of the global cache (start_s × 75 →
           end_s × 75) → (T_bar, 768) per-bar features.
       Then for each center bar, build a (W=5, T_max, 768) window by stacking
       the surrounding bars (zero-padded + bar_mask=False at track edges) and
       run the windowed head. Only the center prediction (index half=2) is
       emitted per bar.

⚠ Distribution shift vs prior versions: the head was trained against MERT
features extracted PER BAR (each bar's audio fed independently to MERT).
Slicing from the global stream gives features computed with attention across
the whole 60 s chunk — strictly more context, but a different distribution
than training. The classifier preprocessor's `version` is bumped on this
change so prior predictions get refreshed automatically.

22-tag schema (from `tango/data/models/bar_window_classifier.pt::tag_order`,
6 multi-label heads concatenated in HEADS order):

    drums:    hats, kick, snare, perc, fill, impact
    rhythm:   four_four, halftime, breakbeat, build
    bass:     pluck, sustain
    synths:   arp, pad, lead, riser
    acoustic: piano, acoustic_guitar, electric_guitar, other
    vocals:   vocal_lead, vocal_chop

CLI:
    classifier_worker.py <mert_cache_npy> <weights_file>

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

MERT_FRAMES_PER_SECOND = 75
MERT_LAYER = 7  # informational; the cache file is already layer-7 sliced.
MIN_BAR_FRAMES = MERT_FRAMES_PER_SECOND // 10  # < 100 ms (~8 frames) = skip.
WINDOW_BATCH = 32  # windows per BarWindowClassifier forward pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "mert_cache",
        type=pathlib.Path,
        help="Precomputed MERT-95M layer-7 cache (.npy fp16 at 75 Hz).",
    )
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

    if not args.mert_cache.exists():
        print(json.dumps({"error": f"MERT cache does not exist: {args.mert_cache}"}), file=sys.stderr)
        return 1
    if not args.weights_file.exists():
        print(json.dumps({"error": f"Weights file does not exist: {args.weights_file}"}), file=sys.stderr)
        return 1

    boundaries_raw = sys.stdin.read()
    if not boundaries_raw.strip():
        print(json.dumps({"error": "No bar boundaries received on stdin"}), file=sys.stderr)
        return 1

    # Third-party libs print warnings to stdout. Stdout is reserved for our
    # JSON payload, so route everything else to stderr while we work; final
    # `emit()` writes the JSON outside this block.
    bars_out: list[dict] = []
    with contextlib.redirect_stdout(sys.stderr):
        try:
            import numpy as np
            import torch
        except Exception as exc:  # pragma: no cover - import error reporting
            print(json.dumps({"error": f"Missing python deps for classifier: {exc}"}), file=sys.stderr)
            return 1

        boundaries = json.loads(boundaries_raw)
        if not isinstance(boundaries, list) or not boundaries:
            print(json.dumps({"error": "bar_boundaries_json must be a non-empty list"}), file=sys.stderr)
            return 1

        device = "cuda" if torch.cuda.is_available() else "cpu"

        try:
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
            # Stage 1: per-bar MERT features. Slice from the precomputed
            # full-mix layer-7 cache (T_global, 768) at 75 Hz instead of
            # running MERT per-bar. Bars too short to score get a None
            # placeholder.
            # ---------------------------------------------------------------
            print(f"[classifier] loading MERT cache from {args.mert_cache}", file=sys.stderr, flush=True)
            mert_global = np.load(args.mert_cache)  # (T_global, 768) fp16
            if mert_global.dtype != np.float32:
                mert_global = mert_global.astype(np.float32)
            total_frames = mert_global.shape[0]
            n_bars = len(boundaries)
            per_bar_feats: list[np.ndarray | None] = [None] * n_bars

            for bar_idx, (start_s, end_s) in enumerate(boundaries):
                s_frame = max(0, int(round(float(start_s) * MERT_FRAMES_PER_SECOND)))
                e_frame = min(total_frames, int(round(float(end_s) * MERT_FRAMES_PER_SECOND)))
                if e_frame - s_frame < MIN_BAR_FRAMES:
                    continue
                # Copy so downstream torch.from_numpy doesn't pin a slice of
                # the (potentially mmapped) global cache.
                per_bar_feats[bar_idx] = mert_global[s_frame:e_frame].copy()

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
