#!/usr/bin/env python3
"""
Simple helper that mirrors the notebook in experiments/beatgrid/test.ipynb.
Given an audio file path, it emits a JSON payload containing beat and
downbeat timestamps (seconds) using beat_this' File2Beats helper.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import traceback


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compute beat and downbeat timings for an audio file.",
    )
    parser.add_argument(
        "audio_file",
        type=pathlib.Path,
        help="Path to the audio file that should be analysed.",
    )
    parser.add_argument(
        "--checkpoint",
        default="final0",
        help="beat_this checkpoint to use (defaults to 'final0').",
    )
    parser.add_argument(
        "--no-dbn",
        action="store_true",
        help="Disable the DBN post-processing stage.",
    )
    return parser.parse_args()


def serialize(values):
    return [float(value) for value in values]


def main() -> int:
    args = parse_args()

    try:
        from beat_this.inference import File2Beats
    except Exception as exc:  # pragma: no cover - import error reporting
        print(
            json.dumps({"error": f"Failed to import beat_this: {exc}"}),
            file=sys.stderr,
        )
        return 1

    if not args.audio_file.exists():
        print(
            json.dumps({"error": f"Audio file does not exist: {args.audio_file}"}),
            file=sys.stderr,
        )
        return 1

    try:
        file2beats = File2Beats(
            checkpoint_path=str(args.checkpoint),
            dbn=not args.no_dbn,
        )
        beats, downbeats = file2beats(str(args.audio_file))
    except Exception as exc:  # pragma: no cover - runtime error reporting
        traceback.print_exc()
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 1

    payload = {"beats": serialize(beats), "downbeats": serialize(downbeats)}
    sys.stdout.write(json.dumps(payload))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover - script entrypoint
    raise SystemExit(main())
