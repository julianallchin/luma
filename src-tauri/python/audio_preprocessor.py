#!/usr/bin/env python3

from __future__ import annotations

import argparse
import contextlib
import io
import json
import platform
import shutil
import sys
import warnings
from pathlib import Path

import soundfile
from demucs import separate as demucs_separate


def resave_wav_files(source_dir: Path, target_dir: Path) -> None:
    """Resave WAV files using soundfile to avoid torchaudio warnings."""
    target_dir.mkdir(parents=True, exist_ok=True)
    
    for wav_file in sorted(source_dir.glob("*.wav")):
        # Read with soundfile
        data, sample_rate = soundfile.read(str(wav_file))
        
        # Write to target directory with soundfile (no warnings)
        target_file = target_dir / wav_file.name
        soundfile.write(str(target_file), data, sample_rate)


def main() -> int:
    parser = argparse.ArgumentParser(description="Run Demucs on a track and report the extracted stems.")
    parser.add_argument("audio_file", type=Path, help="Path to the audio file to separate.")
    parser.add_argument(
        "target_dir",
        type=Path,
        help="Directory where the final stems should be stored (will contain stem WAV files).",
    )
    parser.add_argument(
        "--model",
        default="htdemucs",
        help="Demucs model to use for separation (defaults to htdemucs).",
    )
    parser.add_argument(
        "--device",
        default=None,
        help="Device to run Demucs on (cpu/cuda/mps). If unset defaults to mps on macOS.",
    )
    args = parser.parse_args()

    warnings.filterwarnings("ignore")

    audio_path = args.audio_file.resolve()
    if not audio_path.exists():
        print(f"Error: audio file does not exist: {audio_path}", file=sys.stderr)
        return 1

    print(f"[audio_preprocessor] separating {audio_path}", file=sys.stderr, flush=True)

    target_dir = args.target_dir.resolve()
    target_dir.parent.mkdir(parents=True, exist_ok=True)

    working_dir = target_dir.parent.joinpath(f"demucs_work_{target_dir.name}")
    if working_dir.exists():
        shutil.rmtree(working_dir)
    working_dir.mkdir(parents=True, exist_ok=True)

    demucs_opts = [
        "--name",
        args.model,
        "--out",
        str(working_dir),
        str(audio_path),
    ]
    device = args.device
    if device is None and platform.system() == "Darwin":
        device = "mps"
    device_info = f" (device={device})" if device else ""
    print(
        f"[audio_preprocessor] running demucs {args.model}{device_info} -> {working_dir}",
        file=sys.stderr,
        flush=True,
    )
    
    if device:
        demucs_opts.extend(["--device", device])
    demucs_buffer = io.StringIO()
    try:
        with contextlib.redirect_stdout(demucs_buffer), contextlib.redirect_stderr(
            demucs_buffer
        ):
            demucs_separate.main(demucs_opts)
    except Exception:
        captured = demucs_buffer.getvalue().strip()
        if captured:
            print(
                f"[audio_preprocessor] demucs error output:\n{captured}",
                file=sys.stderr,
                flush=True,
            )
        raise

    source_dir = working_dir / args.model / audio_path.stem
    if not source_dir.exists():
        print(f"Error: expected Demucs output not found at {source_dir}", file=sys.stderr)
        return 1

    # Resave files using soundfile to avoid torchaudio warnings
    print("[audio_preprocessor] resaving stems with soundfile", file=sys.stderr, flush=True)
    if target_dir.exists():
        shutil.rmtree(target_dir)
    resave_wav_files(source_dir, target_dir)

    # Clean up working directory
    if working_dir.exists():
        shutil.rmtree(working_dir)

    stems = []
    for stem_file in sorted(target_dir.glob("*.wav")):
        stems.append({"name": stem_file.stem, "path": str(stem_file)})

    if not stems:
        print("Error: no stems were produced", file=sys.stderr, flush=True)
        return 1

    print(json.dumps({"stems": stems, "target_dir": str(target_dir)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
