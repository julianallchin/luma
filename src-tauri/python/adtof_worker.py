#!/usr/bin/env python3
"""ADTOF-pytorch drum-onset worker.

Wraps `adtof_pytorch` (https://github.com/xavriley/ADTOF-pytorch). Takes one
audio file (the demucs `drums.ogg` stem) and emits per-class onset
timestamps on stdout as JSON.

Output schema:
    {
        "onsets": {
            "<midi_note>": [<seconds>, ...],
            ...
        }
    }

Class labels (ADTOF Frame_RNN, `LABELS_5`):
    35 = kick
    38 = snare
    47 = tom (mid)
    42 = hi-hat (closed)
    49 = cymbal / crash

Model weights (~3.5 MB) ship inside the `adtof-pytorch` pip package; no
explicit download is needed. The package locates them via
`get_default_weights_path()`.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import pathlib
import sys


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Transcribe drum onsets from a drums stem via ADTOF-pytorch.",
    )
    parser.add_argument(
        "audio_file",
        type=pathlib.Path,
        help="Path to the drums.ogg stem (or any drum-isolated audio file).",
    )
    parser.add_argument(
        "--fps",
        type=int,
        default=100,
        help="Frames per second for activation post-processing (default: 100).",
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

    # Third-party libs (adtof_pytorch.load_pytorch_weights) print status to
    # stdout. Stdout is reserved for our JSON payload, so route everything
    # else to stderr while we work.
    with contextlib.redirect_stdout(sys.stderr):
        try:
            import torch
            from adtof_pytorch import (
                FRAME_RNN_THRESHOLDS,
                LABELS_5,
                PeakPicker,
                calculate_n_bins,
                create_frame_rnn_model,
                get_default_weights_path,
                load_audio_for_model,
                load_pytorch_weights,
            )
        except Exception as exc:  # pragma: no cover - import error reporting
            print(
                json.dumps({"error": f"Failed to import adtof_pytorch: {exc}"}),
                file=sys.stderr,
            )
            return 1

        device = "cuda" if torch.cuda.is_available() else "cpu"

        try:
            n_bins = calculate_n_bins()
            model = create_frame_rnn_model(n_bins).eval()
            weights_path = get_default_weights_path()
            if weights_path is None:
                print(
                    json.dumps({"error": "ADTOF default weights not found in package"}),
                    file=sys.stderr,
                )
                return 1
            model = load_pytorch_weights(model, str(weights_path), strict=False)
            model.to(device)

            x = load_audio_for_model(str(args.audio_file)).to(device)
            with torch.no_grad():
                pred = model(x).cpu().numpy()  # [1, time, classes]

            picker = PeakPicker(thresholds=FRAME_RNN_THRESHOLDS, fps=args.fps)
            picked = picker.pick(pred, labels=LABELS_5, label_offset=0)
            # picked is List[Dict[int, List[float]]]; we have one batch element.
            per_class = picked[0] if picked else {}

            onsets = {
                str(int(label)): [float(t) for t in times]
                for label, times in per_class.items()
            }
        except Exception as exc:  # pragma: no cover - runtime error reporting
            print(json.dumps({"error": str(exc)}), file=sys.stderr)
            return 1

    sys.stdout.write(json.dumps({"onsets": onsets}))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover - script entrypoint
    raise SystemExit(main())
