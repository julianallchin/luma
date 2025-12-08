#!/usr/bin/env python3
"""
ACE chord-sections worker.

Given an audio file, runs consonance-ACE's ConformerDecomposedModel and emits
merged chord sections as JSON: { sections: [{start, end, root, label}], hop_seconds }.
"""

from __future__ import annotations

import argparse
import json
import sys
import traceback
import tempfile
import os
from pathlib import Path
from typing import List, Tuple

import numpy as np
import torch


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compute chord sections using consonance-ACE.",
    )
    parser.add_argument(
        "audio_files",
        type=Path,
        nargs="+",
        help="Path to the audio file(s) to analyse. If multiple are provided, they will be mixed (summed) before analysis.",
    )
    parser.add_argument(
        "--ckpt",
        type=Path,
        default=Path("consonance-ACE/ACE/checkpoints/conformer_decomposed_smooth.ckpt"),
        help="Path to ACE conformer_decomposed checkpoint.",
    )
    parser.add_argument(
        "--chunk-dur",
        type=float,
        default=20.0,
        help="Chunk duration in seconds (must match training, default 20.0).",
    )
    parser.add_argument(
        "--sample-rate",
        type=int,
        default=22050,
        help="Internal ACE sample rate (must match training, default 22050).",
    )
    parser.add_argument(
        "--hop-length",
        type=int,
        default=512,
        help="CQ/hop length in samples (must match training, default 512).",
    )
    parser.add_argument(
        "--save-logits",
        action="store_true",
        help="Save raw root logits to a binary file sidecar.",
    )
    parser.add_argument(
        "--min-chord-dur",
        type=float,
        default=0.5,
        help="Minimum chord duration in seconds (matches inference.py default).",
    )
    return parser.parse_args()


def load_ace_model(ckpt_path: Path) -> torch.nn.Module:
    # Add local consonance-ACE repo to sys.path
    repo_root = Path(__file__).parent / "consonance-ACE"
    sys.path.insert(0, str(repo_root))

    from ACE.models.conformer_decomposed import ConformerDecomposedModel  # type: ignore

    device = "cuda" if torch.cuda.is_available() else "cpu"
    model = ConformerDecomposedModel.load_from_checkpoint(
        str(ckpt_path),
        vocabularies={"root": 13, "bass": 13, "onehot": 12},
        map_location=device,
        loss="consonance_decomposed",
    )
    model.eval().to(device)
    return model


def make_chunker(
    audio_path: Path, sample_rate: int, hop_length: int, chunk_dur: float
):
    # consonance-ACE preprocess imports (after sys.path tweak in load_ace_model)
    from ACE.preprocess.audio_processor import AudioChunkProcessor  # type: ignore
    from ACE.preprocess.transforms import CQTransform  # type: ignore

    if torch.cuda.is_available():
        device = "cuda"
    elif hasattr(torch.backends, "mps") and torch.backends.mps.is_available():
        device = "mps"
    else:
        device = "cpu"

    transform = CQTransform(sample_rate, hop_length)
    chunker = AudioChunkProcessor(
        audio_path=str(audio_path),
        target_sample_rate=sample_rate,
        hop_length=hop_length,
        max_sequence_length=chunk_dur,
        device=device,
        transform=transform,
        normalize=True,
    )
    return chunker


@torch.no_grad()
def predict_logits(
    model: torch.nn.Module, features: torch.Tensor
) -> Tuple[torch.Tensor, torch.Tensor, torch.Tensor]:
    """
    features: [1, 1, F, T] or [1, F, T]
    returns: root_logits [T, 13], bass_logits [T, 13], chord_logits [T, 12]
    """
    device = next(model.parameters()).device
    features = features.to(device)
    outputs = model(features)
    root_logits = outputs["root"].squeeze(0)  # [T, 13]
    bass_logits = outputs["bass"].squeeze(0)  # [T, 13]
    chord_logits = outputs["onehot"].squeeze(0)  # [T, 12]
    return root_logits, bass_logits, chord_logits


def merge_identical_consecutive(intervals: np.ndarray, labels: List[str]):
    if len(labels) == 0:
        return intervals, labels

    merged_intervals = [intervals[0].tolist()]
    merged_labels = [labels[0]]

    for i in range(1, len(labels)):
        if labels[i] == merged_labels[-1]:
            merged_intervals[-1][1] = intervals[i][1]
        else:
            merged_intervals.append(intervals[i].tolist())
            merged_labels.append(labels[i])

    return np.array(merged_intervals), merged_labels


def main() -> int:
    args = parse_args()

    # Basic checks
    for af in args.audio_files:
        if not af.exists():
            print(
                json.dumps(
                    {"error": f"Audio file does not exist: {af}"}
                ),
                file=sys.stderr,
            )
            return 1
            
    if not args.ckpt.exists():
        print(
            json.dumps(
                {"error": f"Checkpoint file does not exist: {args.ckpt}"}
            ),
            file=sys.stderr,
        )
        return 1

    try:
        model = load_ace_model(args.ckpt)
    except Exception as exc:
        traceback.print_exc()
        print(
            json.dumps({"error": f"Failed to load ACE model: {exc}"}),
            file=sys.stderr,
        )
        return 1

    temp_mix_path = None
    target_audio_path = args.audio_files[0] # Used for duration check and sidecar naming

    try:
        # do librosa import lazily to avoid overhead if model load fails
        import librosa  # type: ignore
        import soundfile as sf # type: ignore
        from ACE.mir_evaluation import convert_predictions_decomposed, remove_short_chords  # type: ignore

        # Mixdown logic if multiple files
        if len(args.audio_files) > 1:
            try:
                # Load first file to establish sr/length
                y_sum, sr = librosa.load(str(args.audio_files[0]), sr=args.sample_rate, mono=True)
                
                for other_path in args.audio_files[1:]:
                    y_next, _ = librosa.load(str(other_path), sr=args.sample_rate, mono=True)
                    # Resize to match
                    if len(y_next) < len(y_sum):
                        y_next = np.pad(y_next, (0, len(y_sum) - len(y_next)))
                    elif len(y_next) > len(y_sum):
                        y_sum = np.pad(y_sum, (0, len(y_next) - len(y_sum)))
                    
                    y_sum += y_next
                
                # Normalize to prevent clipping
                max_val = np.max(np.abs(y_sum))
                if max_val > 1.0:
                    y_sum /= max_val

                # Save to temp file
                fd, temp_mix_path = tempfile.mkstemp(suffix=".wav")
                os.close(fd)
                sf.write(temp_mix_path, y_sum, sr)
                
                # Use temp path for analysis
                analysis_source = Path(temp_mix_path)
            except Exception as e:
                print(json.dumps({"error": f"Failed to mix stem files: {e}"}), file=sys.stderr)
                return 1
        else:
            analysis_source = args.audio_files[0]

        chunker = make_chunker(
            analysis_source, args.sample_rate, args.hop_length, args.chunk_dur
        )

        hop_seconds = args.hop_length / float(args.sample_rate)
        # Use the mixed source for duration to be accurate
        duration = librosa.get_duration(path=str(analysis_source))
        
        all_intervals: List[np.ndarray] = []
        all_labels: List[str] = []
        all_root_logits: List[np.ndarray] = [] # Accumulate logits
        chunk_index = 0
        max_chunks = int(np.ceil(duration / args.chunk_dur)) + 2
        timeline_time = 0.0  # accumulate actual processed duration instead of chunk_dur multiples

        while chunk_index < max_chunks:
            if timeline_time - duration > hop_seconds:
                break
            features = chunker.process_chunk(onset=timeline_time)
            if features is None:
                break

            # Ensure shape [1, 1, F, T]
            if features.ndim == 2:
                features = features.unsqueeze(0).unsqueeze(0)
            elif features.ndim == 3:
                features = features.unsqueeze(0)

            root_logits, bass_logits, chord_logits = predict_logits(model, features)

            # Accumulate raw logits (on CPU)
            if args.save_logits:
                # root_logits is [T, 13]
                all_root_logits.append(root_logits.cpu().numpy())

            root_pred = torch.argmax(root_logits, dim=-1).cpu().numpy()
            bass_pred = torch.argmax(bass_logits, dim=-1).cpu().numpy()
            chord_pred = torch.sigmoid(chord_logits).cpu().numpy()

            chunk_duration_real = root_pred.shape[0] * hop_seconds

            intervals, labels = convert_predictions_decomposed(
                root_predictions=root_pred,
                bass_predictions=bass_pred,
                chord_predictions=chord_pred,
                segment_duration=chunk_duration_real,
                threshold=0.5,
                remove_short_min_duration=args.min_chord_dur,
            )

            if len(intervals) > 0:
                intervals = intervals.copy()
                intervals[:, 0] += timeline_time
                intervals[:, 1] += timeline_time
                all_intervals.append(intervals)
                all_labels.extend(labels)

            timeline_time += chunk_duration_real
            chunk_index += 1

        logits_path_str = None
        if args.save_logits and all_root_logits:
            # Concatenate all chunks
            full_logits = np.vstack(all_root_logits) # [Total_T, 13]
            # Save as raw float32 binary
            # IMPORTANT: Save relative to the PRIMARY target file, not the temp mix
            logits_file = target_audio_path.with_suffix(target_audio_path.suffix + ".logits.bin")
            full_logits.astype(np.float32).tofile(logits_file)
            logits_path_str = str(logits_file)

        if not all_intervals:
            # Even if no sections, we might have logits
            if logits_path_str:
                 payload = {
                    "sample_rate": int(args.sample_rate),
                    "hop_length": int(args.hop_length),
                    "frame_hop_seconds": hop_seconds,
                    "sections": [],
                    "logits_path": logits_path_str,
                }
                 sys.stdout.write(json.dumps(payload))
                 sys.stdout.flush()
                 return 0

            print(
                json.dumps({"error": "No chord sections produced for this audio."}),
                file=sys.stderr,
            )
            return 1

        intervals = np.vstack(all_intervals)
        intervals, all_labels = remove_short_chords(intervals, all_labels)
        intervals, all_labels = merge_identical_consecutive(intervals, all_labels)

        sections = []
        for (start, end), label in zip(intervals.tolist(), all_labels):
            s = max(0.0, float(start))
            e = min(float(end), float(duration) + hop_seconds * 0.5)
            if e <= s:
                continue
            sections.append({"start": s, "end": e, "label": label})

        payload = {
            "sample_rate": int(args.sample_rate),
            "hop_length": int(args.hop_length),
            "frame_hop_seconds": hop_seconds,
            "sections": sections,
            "logits_path": logits_path_str,
        }

        sys.stdout.write(json.dumps(payload))
        sys.stdout.flush()
        return 0

    except Exception as exc:
        traceback.print_exc()
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 1
    finally:
        # Cleanup temp file
        if temp_mix_path and os.path.exists(temp_mix_path):
            try:
                os.unlink(temp_mix_path)
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
