"""Inference: audio → drum events.

End-to-end:

    audio file
      ├─→ resample to 44.1 kHz mono → log-mel
      └─→ resample to 24 kHz → MERT-95M layer-N hidden states (75 Hz)
                ↓
          sliding-window EDM sampler (window_seconds wide, stride <= window)
                ↓
        peak-pick onsets per drum class
                ↓
            (time_s, drum_class) events

Window/stride are read from the checkpoint's config under `infer.window_seconds`
and `infer.stride_seconds` (defaults 5s/4s for v1-v9 ckpts, 30s/24s for v10+).
The trained context length is the upper bound — never exceed it.

CLI:
    python -m n2n.infer --ckpt checkpoints/run010/best.pt --audio path/to/song.wav
"""

from __future__ import annotations

import argparse
import json
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
import torchaudio
from torch import Tensor

from n2n.data.drum_mapping import CLASS_NAMES, NUM_DRUM_CLASSES
from n2n.model.decoder import N2NConfig, N2NDecoder
from n2n.model.edm import EDMConfig, sample_heun
from n2n.model.mel import LogMel


def _is_no_diffusion(cfg: dict) -> bool:
    return bool(cfg.get("model", {}).get("no_diffusion", False))

MERT_SAMPLE_RATE = 24_000
MERT_FRAMES_PER_SECOND = 75
DEFAULT_TARGET_SAMPLE_RATE = 44_100
DEFAULT_FRAMES_PER_SECOND = 100  # 10 ms hop at 44.1 kHz with hop=441


# ---------------------------------------------------------------------------
# Audio loading
# ---------------------------------------------------------------------------


def load_audio(path: Path, target_sr: int) -> Tensor:
    """Load arbitrary audio, mix to mono, resample to target_sr. Returns (T,) at target_sr."""
    audio, sr = sf.read(str(path), dtype="float32", always_2d=False)
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    wav = torch.from_numpy(audio).unsqueeze(0)
    if sr != target_sr:
        wav = torchaudio.functional.resample(wav, sr, target_sr)
    return wav.squeeze(0)


def load_checkpoint(ckpt_path: Path, device: torch.device) -> tuple[N2NDecoder, dict]:
    ckpt = torch.load(ckpt_path, map_location=device, weights_only=False)
    cfg = ckpt["config"]
    # `no_diffusion` lives in cfg, not in N2NConfig; strip before constructing.
    model_kwargs = {k: v for k, v in cfg["model"].items() if k != "no_diffusion"}
    model_cfg = N2NConfig(**model_kwargs)
    model = N2NDecoder(model_cfg).to(device)
    model.load_state_dict(ckpt["ema"])  # EMA weights for inference
    model.eval()
    return model, cfg


# ---------------------------------------------------------------------------
# MERT feature extraction
# ---------------------------------------------------------------------------


def compute_mert_features(
    audio_44k: Tensor,
    cfg: dict,
    device: torch.device,
    chunk_seconds: float = 30.0,
    overlap_seconds: float = 15.0,
    crop_seconds: float = 3.0,
    mert_model=None,
    feature_extractor=None,
) -> Tensor:
    """Compute MERT layer-N hidden states for arbitrary-length audio.

    Returns (T_mert, mert_dim) at 75 Hz.

    Chunks overlap and the seam regions get cross-fade-blended via a linear
    ramp, so the per-frame output is smooth across what would otherwise be
    chunk boundaries. We empirically observed that the original non-overlap
    chunked path produced a deterministic feature discontinuity at chunk
    seams (one specific dim — `d85` — dominated, repeatable across chunks),
    which RoPE'd cross-attn in the v10 decoder amplifies into 5-15 s drum
    prediction dropouts on long-form audio. Causes: MERT's CNN feature
    extractor zero-pads at chunk edges, producing a few frames of degraded
    output. By overlapping chunks and tapering edge weights to zero, those
    degraded frames are drowned out by neighbouring chunks' centers.

    Defaults (60 s chunks, 30 s overlap, 5 s crop): every output frame is
    covered by at least one chunk's interior; consecutive chunks' tapers
    sum smoothly through the 30 s overlap region. Total MERT compute scales
    by chunk_seconds / (chunk_seconds - overlap_seconds) = 60/30 = 2× vs the
    non-overlap path. Worth it for inference quality on long-form audio.
    """
    if mert_model is None or feature_extractor is None:
        from transformers import AutoFeatureExtractor, AutoModel
        if feature_extractor is None:
            feature_extractor = AutoFeatureExtractor.from_pretrained(
                "m-a-p/MERT-v1-95M", trust_remote_code=True
            )
        if mert_model is None:
            mert_model = AutoModel.from_pretrained(
                "m-a-p/MERT-v1-95M", trust_remote_code=True
            ).to(device).eval()
    fe = feature_extractor
    layer = cfg["data"]["mert_layer"]

    audio_24k = torchaudio.functional.resample(
        audio_44k.unsqueeze(0), cfg["mel"]["sample_rate"], MERT_SAMPLE_RATE
    ).squeeze(0).cpu().numpy()

    chunk_samples = int(chunk_seconds * MERT_SAMPLE_RATE)
    overlap_samples = int(overlap_seconds * MERT_SAMPLE_RATE)
    stride_samples = chunk_samples - overlap_samples
    if stride_samples <= 0:
        raise ValueError(
            f"overlap_seconds ({overlap_seconds}) must be < chunk_seconds ({chunk_seconds})"
        )
    crop_frames = int(round(crop_seconds * MERT_FRAMES_PER_SECOND))

    n_samples = audio_24k.shape[0]
    starts = list(range(0, max(1, n_samples - chunk_samples + 1), stride_samples))
    if not starts or starts[-1] + chunk_samples < n_samples:
        starts.append(max(0, n_samples - chunk_samples))

    total_frames = int(round(n_samples / MERT_SAMPLE_RATE * MERT_FRAMES_PER_SECOND))
    hidden_dim: int | None = None
    out_sum: torch.Tensor | None = None
    out_weight: torch.Tensor | None = None

    with torch.no_grad():
        for s_audio in starts:
            chunk_audio = audio_24k[s_audio:s_audio + chunk_samples]
            if chunk_audio.shape[0] < int(0.1 * MERT_SAMPLE_RATE):  # skip <100 ms tail
                continue
            inputs = fe(chunk_audio, sampling_rate=MERT_SAMPLE_RATE, return_tensors="pt")
            iv = inputs["input_values"].to(device)
            out = mert_model(iv, output_hidden_states=True)
            feat = out.hidden_states[layer].squeeze(0).float().cpu()  # (n_frames, dim)

            if hidden_dim is None:
                hidden_dim = feat.shape[1]
                out_sum = torch.zeros((total_frames, hidden_dim), dtype=torch.float32)
                out_weight = torch.zeros((total_frames,), dtype=torch.float32)

            n_chunk_frames = feat.shape[0]
            # Linear-ramp taper at chunk edges (0 at the boundary, 1 by `crop_frames`
            # in). Edge frames carry MERT's conv-pad artifact — taper them out.
            weight = torch.ones(n_chunk_frames, dtype=torch.float32)
            ramp = min(crop_frames, n_chunk_frames // 2)
            if ramp > 0:
                ramp_vals = torch.linspace(0.0, 1.0, ramp + 1)[1:]  # exclude 0 to avoid /0
                weight[:ramp] = ramp_vals
                weight[-ramp:] = ramp_vals.flip(0)

            s_frame = int(round(s_audio / MERT_SAMPLE_RATE * MERT_FRAMES_PER_SECOND))
            end_frame = min(s_frame + n_chunk_frames, total_frames)
            n_emit = end_frame - s_frame
            out_sum[s_frame:end_frame] += feat[:n_emit] * weight[:n_emit, None]
            out_weight[s_frame:end_frame] += weight[:n_emit]

    if out_sum is None or hidden_dim is None or out_weight is None:
        return torch.zeros((0, 768), dtype=torch.float32)

    safe_w = out_weight.clamp(min=1e-6)
    return out_sum / safe_w[:, None]


# ---------------------------------------------------------------------------
# Sliding-window sampler
# ---------------------------------------------------------------------------


@torch.no_grad()
def transcribe_sliding(
    model: N2NDecoder,
    cfg: dict,
    audio: Tensor,            # (T,) at 44.1 kHz
    mert: Tensor,             # (T_mert, mert_dim) at 75 Hz
    num_steps: int = 5,
    device: torch.device | str = "cuda",
    window_seconds: float = 5.0,
    stride_seconds: float = 4.0,
) -> Tensor:
    """Run inference in overlapping windows and stitch the outputs.

    v2-v11 (diffusion): runs an EDM Heun sampler with `num_steps` denoising
    iterations per window; output is roughly in [-1, +1] (regression onto a
    {-1, +1} target).
    v12+ (discriminative): one forward pass per window; output is sigmoid
    probabilities in [0, 1]. `num_steps` is ignored.

    Returns (n_frames, n_drum_classes, n_axes) float tensor on CPU. Use
    peak_pick() with a threshold appropriate to the mode.
    """
    no_diff = _is_no_diffusion(cfg)
    edm_cfg = None if no_diff else EDMConfig(**cfg["edm"])
    target_sr = cfg["mel"]["sample_rate"]
    hop = cfg["mel"]["hop_length"]
    mel_fps = target_sr // hop                  # 100 Hz @ 44.1 kHz / 441
    mel_module = LogMel(**cfg["mel"]).to(device)

    win_audio = int(window_seconds * target_sr)
    stride_audio = int(stride_seconds * target_sr)
    win_frames_mel = int(window_seconds * mel_fps)
    win_frames_mert = int(window_seconds * MERT_FRAMES_PER_SECOND)
    context_frames = int((window_seconds - stride_seconds) / 2 * mel_fps)

    n_samples = audio.shape[0]
    n_frames_total = int(round(n_samples / hop))
    n_axes = int(cfg["model"].get("n_axes", 1))

    # Hann-weighted overlap-add: each window contributes to its full frame
    # range, weighted by a Hann taper that goes 0 → 1 → 0 across the window.
    # Adjacent windows' tapers sum smoothly through the overlap region, so
    # frame-level class assignment isn't subject to abrupt window-boundary
    # switches. Predictions averaged (not summed) by dividing by the total
    # weight at each frame.
    hann = torch.hann_window(win_frames_mel, periodic=False).clamp(min=1e-3)
    out_sum = torch.zeros(
        (n_frames_total, NUM_DRUM_CLASSES, n_axes), dtype=torch.float32
    )
    out_weight = torch.zeros((n_frames_total,), dtype=torch.float32)

    audio_np = audio.cpu().numpy() if isinstance(audio, Tensor) else audio
    mert_np = mert.cpu().numpy() if isinstance(mert, Tensor) else mert

    starts = list(range(0, max(1, n_samples - win_audio + 1), stride_audio))
    if not starts or starts[-1] + win_audio < n_samples:
        starts.append(max(0, n_samples - win_audio))

    for s_audio in starts:
        win_wav = audio_np[s_audio:s_audio + win_audio]
        if win_wav.shape[0] < win_audio:
            win_wav = np.pad(win_wav, (0, win_audio - win_wav.shape[0]))
        win_audio_t = torch.from_numpy(win_wav).float().unsqueeze(0).to(device)

        s_mert = int(round(s_audio / target_sr * MERT_FRAMES_PER_SECOND))
        mert_slice = mert_np[s_mert:s_mert + win_frames_mert]
        if mert_slice.shape[0] < win_frames_mert:
            pad_rows = win_frames_mert - mert_slice.shape[0]
            mert_slice = np.concatenate(
                [mert_slice, np.zeros((pad_rows, mert_slice.shape[1]), dtype=mert_slice.dtype)],
                axis=0,
            )
        win_mert_t = torch.from_numpy(mert_slice).float().unsqueeze(0).to(device)

        with torch.amp.autocast("cuda", dtype=torch.bfloat16, enabled=device.type == "cuda"):
            mel_feats = mel_module(win_audio_t)
            cond = {"mel": mel_feats, "mert": win_mert_t}
            if no_diff:
                x_zero = torch.zeros(
                    (1, win_frames_mel, NUM_DRUM_CLASSES, n_axes),
                    device=device, dtype=torch.float32,
                )
                c_noise_zero = torch.zeros((1,), device=device, dtype=torch.float32)
                logits = model(x_zero, c_noise_zero, cond)
                win_out = torch.sigmoid(logits)
            else:
                win_out = sample_heun(
                    model, cond,
                    shape=(1, win_frames_mel, NUM_DRUM_CLASSES, n_axes),
                    cfg=edm_cfg, num_steps=num_steps, device=device, dtype=torch.float32,
                )
        win_out = win_out.squeeze(0).float().cpu()

        win_start_frame = int(round(s_audio / hop))
        emit_end_global = min(win_start_frame + win_frames_mel, n_frames_total)
        n_emit = emit_end_global - win_start_frame
        if n_emit <= 0:
            continue
        w = hann[:n_emit]
        out_sum[win_start_frame:emit_end_global] += (
            win_out[:n_emit] * w.unsqueeze(-1).unsqueeze(-1)
        )
        out_weight[win_start_frame:emit_end_global] += w

    # Normalize. Frames with zero coverage (shouldn't happen given full-coverage
    # `starts`) fall back to a no-onset sentinel: -1.0 for diffusion mode
    # (target ∈ [-1, +1]), 0.0 for v12 sigmoid mode (probability ∈ [0, 1]).
    safe_w = out_weight.clamp(min=1e-6).unsqueeze(-1).unsqueeze(-1)
    out = out_sum / safe_w
    out[out_weight.eq(0)] = 0.0 if no_diff else -1.0
    return out


# ---------------------------------------------------------------------------
# Peak picking + event formatting
# ---------------------------------------------------------------------------


@dataclass
class DrumEvent:
    time_seconds: float
    drum_class: int
    drum_name: str

    def asdict(self) -> dict:
        return {
            "time": self.time_seconds,
            "class": self.drum_class,
            "name": self.drum_name,
        }


def peak_pick(
    target: Tensor,
    onset_threshold: float = 0.0,
    nms_window: int = 2,
) -> list[tuple[int, int]]:
    """Convert (n_frames, D, ...) onset tensor → list of (frame, class).

    Onset axis ([..., 0]) range depends on mode: [-1, +1] for diffusion
    output, [0, 1] for v12 sigmoid output. Caller picks the threshold:
    paper/v11 used 0.0; v12 defaults to 0.5 (or per-class thresholds
    similar to ADTOF's [0.22, 0.24, 0.32, 0.22, 0.30]).

    nms_window is in frames (1 frame = 10 ms by default). Within ±nms_window
    of a chosen onset, no other onset of the same class is emitted.

    The model is trained onset-only — there is no per-event velocity. If the user
    needs loudness, they can measure peak amplitude in a window after each onset
    in the input audio at deployment time, with whatever metric and scaling fits
    their use case.
    """
    onset = target[..., 0].numpy()
    n_frames, n_classes = onset.shape
    events: list[tuple[int, int]] = []
    for cls in range(n_classes):
        mask = onset[:, cls] > onset_threshold
        if not mask.any():
            continue
        last_picked = -10**9
        for t in range(n_frames):
            if not mask[t]:
                continue
            if t - last_picked < nms_window:
                if onset[t, cls] <= onset[last_picked, cls]:
                    continue
            last_picked = t
            events.append((t, cls))
    return events


def to_drum_events(
    raw_events: list[tuple[int, int]],
    fps: int = DEFAULT_FRAMES_PER_SECOND,
) -> list[DrumEvent]:
    """Convert (frame, class) → DrumEvent. Output is the 4-class taxonomy from
    drum_mapping (kick / snare / hat / cymbal)."""
    out: list[DrumEvent] = [
        DrumEvent(frame / fps, cls, CLASS_NAMES[cls])
        for frame, cls in raw_events
    ]
    out.sort(key=lambda e: (e.time_seconds, e.drum_class))
    return out


# ---------------------------------------------------------------------------
# High-level entry points
# ---------------------------------------------------------------------------


def transcribe_file(
    ckpt_path: Path,
    audio_path: Path,
    mert_features_path: Path | None = None,
    num_steps: int = 5,
    device: str = "cuda",
    window_seconds: float | None = None,
    stride_seconds: float | None = None,
) -> list[DrumEvent]:
    """End-to-end: file → drum events. MERT features may be precomputed (.npy)
    or computed on the fly if mert_features_path is None.

    Window/stride default to the checkpoint config's `infer.window_seconds` /
    `infer.stride_seconds` (5s/4s for v1-v9, 30s/24s for v10+). Override via
    the kwargs if you want to clamp them tighter than the trained context.
    """
    dev = torch.device(device)
    model, cfg = load_checkpoint(ckpt_path, dev)
    audio = load_audio(audio_path, target_sr=cfg["mel"]["sample_rate"])

    if mert_features_path is not None:
        mert = torch.from_numpy(np.load(mert_features_path).astype(np.float32))
    else:
        mert = compute_mert_features(audio, cfg, dev)

    infer_cfg = cfg.get("infer", {}) or {}
    win_s = float(window_seconds if window_seconds is not None
                  else infer_cfg.get("window_seconds", 5.0))
    stride_s = float(stride_seconds if stride_seconds is not None
                     else infer_cfg.get("stride_seconds", win_s * 0.8))

    target = transcribe_sliding(
        model, cfg, audio, mert, num_steps=num_steps, device=dev,
        window_seconds=win_s, stride_seconds=stride_s,
    )
    if _is_no_diffusion(cfg):
        onset_threshold = float(infer_cfg.get("peak_pick_threshold", 0.5))
    else:
        onset_threshold = 0.0
    raw_events = peak_pick(target, onset_threshold=onset_threshold)
    return to_drum_events(raw_events, fps=DEFAULT_FRAMES_PER_SECOND)


def main() -> None:
    p = argparse.ArgumentParser(description="N2N drum transcription.")
    p.add_argument("--ckpt", type=Path, required=True, help="checkpoint .pt file")
    p.add_argument("--audio", type=Path, required=True, help="input audio file")
    p.add_argument("--mert", type=Path, default=None, help="optional precomputed MERT .npy")
    p.add_argument("--num-steps", type=int, default=5, help="EDM sampler steps (5 or 10)")
    p.add_argument("--device", type=str, default="cuda", help="cuda / mps / cpu")
    p.add_argument("--out", type=Path, default=None,
                   help="optional path to write JSON events; default = stdout")
    args = p.parse_args()

    events = transcribe_file(
        ckpt_path=args.ckpt,
        audio_path=args.audio,
        mert_features_path=args.mert,
        num_steps=args.num_steps,
        device=args.device,
    )
    payload = [e.asdict() for e in events]
    if args.out is None:
        print(json.dumps(payload, indent=2))
    else:
        args.out.write_text(json.dumps(payload, indent=2))
        print(f"wrote {len(events)} events to {args.out}")


if __name__ == "__main__":
    main()
