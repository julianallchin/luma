#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "librosa",
#     "numpy",
#     "soundfile",
#     "openai",
# ]
# ///
"""
Test beat-aligned drop detection using multimodal LLMs via OpenRouter.

Hypothesis: Interleaving text markers with audio chunks helps LLMs identify
musical events at precise beat positions without needing timestamp tracking.
"""

import argparse
import base64
import io
import json
import os
import random
import sqlite3
import sys
from pathlib import Path

import librosa
import numpy as np
import soundfile as sf
from openai import OpenAI

# Default database path
DEFAULT_DB_PATH = Path.home() / "Library/Application Support/com.luma.luma/luma.db"


def get_db_connection(db_path: Path) -> sqlite3.Connection:
    """Connect to the SQLite database."""
    if not db_path.exists():
        raise FileNotFoundError(f"Database not found: {db_path}")
    return sqlite3.connect(db_path)


def get_random_track_with_beats(conn: sqlite3.Connection) -> dict | None:
    """Select a random track that has beat data."""
    cursor = conn.execute("""
        SELECT
            t.id, t.title, t.artist, t.file_path, t.duration_seconds,
            tb.bpm, tb.beats_per_bar, tb.beats_json, tb.downbeats_json
        FROM tracks t
        JOIN track_beats tb ON t.id = tb.track_id
        WHERE t.file_path IS NOT NULL
        ORDER BY RANDOM()
        LIMIT 1
    """)
    row = cursor.fetchone()
    if not row:
        return None

    return {
        "id": row[0],
        "title": row[1],
        "artist": row[2],
        "file_path": row[3],
        "duration_seconds": row[4],
        "bpm": row[5],
        "beats_per_bar": row[6],
        "beats": json.loads(row[7]),
        "downbeats": json.loads(row[8]),
    }


def get_track_by_id(conn: sqlite3.Connection, track_id: int) -> dict | None:
    """Get a specific track by ID."""
    cursor = conn.execute("""
        SELECT
            t.id, t.title, t.artist, t.file_path, t.duration_seconds,
            tb.bpm, tb.beats_per_bar, tb.beats_json, tb.downbeats_json
        FROM tracks t
        JOIN track_beats tb ON t.id = tb.track_id
        WHERE t.id = ?
    """, (track_id,))
    row = cursor.fetchone()
    if not row:
        return None

    return {
        "id": row[0],
        "title": row[1],
        "artist": row[2],
        "file_path": row[3],
        "duration_seconds": row[4],
        "bpm": row[5],
        "beats_per_bar": row[6],
        "beats": json.loads(row[7]),
        "downbeats": json.loads(row[8]),
    }


def list_tracks_with_beats(conn: sqlite3.Connection) -> list[dict]:
    """List all tracks that have beat data."""
    cursor = conn.execute("""
        SELECT t.id, t.title, t.artist, tb.bpm
        FROM tracks t
        JOIN track_beats tb ON t.id = tb.track_id
        WHERE t.file_path IS NOT NULL
        ORDER BY t.title
    """)
    return [
        {"id": row[0], "title": row[1], "artist": row[2], "bpm": row[3]}
        for row in cursor.fetchall()
    ]


def get_subdivision_times(beats: list[float], subdivision: float) -> np.ndarray:
    """
    Get chunk boundary times based on subdivision factor.

    Args:
        beats: List of beat times in seconds
        subdivision:
            1 = one chunk per beat
            2 = one chunk per half-beat (interpolate between beats)
            0.5 = one chunk per 2 beats
            0.25 = one chunk per bar (4 beats)

    Returns:
        Array of chunk boundary times
    """
    beats = np.array(beats)

    if subdivision >= 1:
        # Interpolate between beats for finer subdivisions
        if subdivision == 1:
            return beats

        result = []
        for i in range(len(beats) - 1):
            t0, t1 = beats[i], beats[i + 1]
            for j in range(int(subdivision)):
                result.append(t0 + (t1 - t0) * j / subdivision)
        result.append(beats[-1])
        return np.array(result)
    else:
        # Take every Nth beat for coarser subdivisions
        step = int(1 / subdivision)
        return beats[::step]


def chunk_audio_by_beats(
    audio_path: str,
    chunk_times: np.ndarray,
    max_chunks: int = 200,
    sample_rate: int = 16000,
) -> list[tuple[int, bytes]]:
    """
    Split audio into chunks at the given times.

    Args:
        audio_path: Path to audio file
        chunk_times: Array of chunk boundary times in seconds
        max_chunks: Maximum number of chunks to return
        sample_rate: Target sample rate (lower = smaller files). 16000 is good for speech/music understanding.

    Returns:
        List of (chunk_number, wav_bytes) tuples
    """
    # Load and resample to target rate
    y, sr = librosa.load(audio_path, sr=sample_rate, mono=True)

    chunks = []
    for i in range(len(chunk_times) - 1):
        if i >= max_chunks:
            break

        start_sec = chunk_times[i]
        end_sec = chunk_times[i + 1]

        start_sample = int(start_sec * sr)
        end_sample = int(end_sec * sr)

        chunk_audio = y[start_sample:end_sample]

        # Convert to WAV bytes (16-bit PCM)
        buf = io.BytesIO()
        sf.write(buf, chunk_audio, sr, format='WAV', subtype='PCM_16')
        wav_bytes = buf.getvalue()

        chunks.append((i + 1, wav_bytes))  # 1-indexed beat numbers

    return chunks


def estimate_chunk_size(chunks: list[tuple[int, bytes]]) -> int:
    """Estimate total size of all chunks in bytes."""
    return sum(len(data) for _, data in chunks)


def beat_to_bar_beat(beat_index: int, beats_per_bar: int = 4) -> str:
    """
    Convert a 0-indexed beat number to bar.beat notation.

    Example (4/4 time): beat 0 -> "1.1", beat 3 -> "1.4", beat 4 -> "2.1"
    """
    bar = (beat_index // beats_per_bar) + 1
    beat = (beat_index % beats_per_bar) + 1
    return f"{bar}.{beat}"


PROMPT_INTRO = (
    "I'm going to play you a song split into {num_chunks} consecutive chunks. "
    "Each chunk is labeled with bar.beat notation (e.g., 18.3 means bar 18, beat 3). "
    "This song is in {beats_per_bar}/4 time. Listen to all chunks carefully."
)

PROMPT_FINAL = (
    "\n\nBased on the audio you just heard, list ALL the drops in this song. "
    "A drop is the moment where the energy peaks and the bass/beat fully kicks in after a buildup. "
    "For each drop, give the bar.beat where it STARTS. "
    "Format your response as a list like:\n"
    "- X.Y: description\n"
    "List drops in chronological order."
)


def build_openrouter_messages(
    chunks: list[tuple[int, bytes]],
    beats_per_bar: int = 4,
) -> list[dict]:
    """Build messages list for OpenRouter API with interleaved audio."""
    content = []

    # Intro text
    content.append({
        "type": "text",
        "text": PROMPT_INTRO.format(num_chunks=len(chunks), beats_per_bar=beats_per_bar),
    })

    # Interleave labels and audio
    for beat_idx, wav_bytes in chunks:
        label = beat_to_bar_beat(beat_idx - 1, beats_per_bar)
        content.append({"type": "text", "text": f"[{label}]"})
        content.append({
            "type": "input_audio",
            "input_audio": {
                "data": base64.b64encode(wav_bytes).decode("utf-8"),
                "format": "wav",
            },
        })

    # Final prompt
    content.append({"type": "text", "text": PROMPT_FINAL})

    return [{"role": "user", "content": content}]


def parse_bar_beats_from_response(response_text: str, beats_per_bar: int = 4) -> list[tuple[str, int]]:
    """
    Extract all bar.beat values from Gemini's response.

    Returns:
        List of (bar.beat string, 0-indexed beat number) tuples
    """
    import re

    results = []
    # Look for all bar.beat patterns like "18.1", "18.3", etc.
    for match in re.finditer(r'\b(\d+)\.(\d+)\b', response_text):
        bar = int(match.group(1))
        beat = int(match.group(2))
        # Skip invalid beats (e.g., "2.5" which might be a decimal)
        if beat < 1 or beat > beats_per_bar:
            continue
        bar_beat = f"{bar}.{beat}"
        # Convert to 0-indexed beat number
        beat_index = (bar - 1) * beats_per_bar + (beat - 1)
        # Avoid duplicates
        if not any(bb == bar_beat for bb, _ in results):
            results.append((bar_beat, beat_index))

    return results


def main():
    parser = argparse.ArgumentParser(
        description="Test beat-aligned drop detection with Gemini multimodal API"
    )
    parser.add_argument(
        "--db",
        type=str,
        default=str(DEFAULT_DB_PATH),
        help=f"Path to luma.db (default: {DEFAULT_DB_PATH})",
    )
    parser.add_argument(
        "--track-id", "-t",
        type=int,
        help="Specific track ID to use. If not provided, picks a random track.",
    )
    parser.add_argument(
        "--subdivision", "-s",
        type=float,
        default=1.0,
        help="Subdivision factor: 1=per beat, 2=half-beat, 0.5=2 beats, 0.25=per bar (default: 1)",
    )
    parser.add_argument(
        "--model", "-m",
        type=str,
        default="google/gemini-2.5-flash",
        help="OpenRouter model (default: google/gemini-2.5-flash)",
    )
    parser.add_argument(
        "--max-chunks",
        type=int,
        default=200,
        help="Maximum number of chunks to send (default: 200)",
    )
    parser.add_argument(
        "--sample-rate", "-r",
        type=int,
        default=16000,
        help="Audio sample rate in Hz. Lower = smaller files (default: 16000)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Prepare chunks but don't call API",
    )
    parser.add_argument(
        "--list-tracks",
        action="store_true",
        help="List available tracks and exit",
    )

    args = parser.parse_args()

    db_path = Path(args.db)
    try:
        conn = get_db_connection(db_path)
    except FileNotFoundError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)

    if args.list_tracks:
        tracks = list_tracks_with_beats(conn)
        print(f"Tracks with beat data ({len(tracks)}):")
        for t in tracks:
            print(f"  [{t['id']:3d}] {t['title']} - {t['artist']} ({t['bpm']:.0f} BPM)")
        conn.close()
        return

    # Select track
    if args.track_id:
        track = get_track_by_id(conn, args.track_id)
        if not track:
            print(f"Error: Track ID {args.track_id} not found or has no beat data", file=sys.stderr)
            conn.close()
            sys.exit(1)
    else:
        track = get_random_track_with_beats(conn)
        if not track:
            print("Error: No tracks with beat data found", file=sys.stderr)
            conn.close()
            sys.exit(1)

    conn.close()

    # Verify audio file exists
    audio_path = Path(track["file_path"])
    if not audio_path.exists():
        print(f"Error: Audio file not found: {audio_path}", file=sys.stderr)
        sys.exit(1)

    print(f"Track: {track['title']} - {track['artist']}")
    print(f"File: {audio_path}")
    print(f"BPM: {track['bpm']:.1f}")
    print(f"Beats: {len(track['beats'])}")
    print(f"Subdivision: {args.subdivision}")
    print(f"Sample rate: {args.sample_rate} Hz")
    print(f"Model: {args.model}")
    print()

    # Get subdivision times from actual beat timestamps
    chunk_times = get_subdivision_times(track["beats"], args.subdivision)
    print(f"Chunk boundaries: {len(chunk_times)}")

    # Chunk audio
    print("Chunking audio...")
    chunks = chunk_audio_by_beats(
        str(audio_path),
        chunk_times,
        max_chunks=args.max_chunks,
        sample_rate=args.sample_rate,
    )
    print(f"Chunks created: {len(chunks)}")

    total_size = estimate_chunk_size(chunks)
    print(f"Total audio size: {total_size / 1024 / 1024:.2f} MB")

    if total_size > 20 * 1024 * 1024:
        print("Warning: Total size exceeds 20MB inline limit. Consider reducing --max-chunks.")
    print()

    if args.dry_run:
        print("Dry run - skipping API call")
        return

    # Check API key
    api_key = os.environ.get("OPENROUTER_API_KEY")
    if not api_key:
        print("Error: OPENROUTER_API_KEY environment variable not set", file=sys.stderr)
        sys.exit(1)

    beats_per_bar = track.get("beats_per_bar", 4) or 4
    print("Building interleaved content...")

    messages = build_openrouter_messages(chunks, beats_per_bar)
    client = OpenAI(
        base_url="https://openrouter.ai/api/v1",
        api_key=api_key,
    )

    print(f"Uploading {total_size / 1024 / 1024:.1f}MB to {args.model}...")

    response_text = ""
    first_chunk = True

    stream = client.chat.completions.create(
        model=args.model,
        messages=messages,
        stream=True,
    )

    for chunk in stream:
        if first_chunk:
            first_chunk = False
            print("Upload complete.")
            print()
            print("=" * 60)
            print("RESPONSE:")
            print("=" * 60)

        if chunk.choices[0].delta.content:
            text = chunk.choices[0].delta.content
            print(text, end="", flush=True)
            response_text += text

    print()
    print("=" * 60)
    print()

    # Parse all bar.beat values from response
    drops = parse_bar_beats_from_response(response_text, beats_per_bar)
    if drops:
        print(f"Detected {len(drops)} drop(s):")
        for bar_beat, beat_index in drops:
            if beat_index < len(chunk_times):
                drop_time = chunk_times[beat_index]
                print(f"  {bar_beat} ({drop_time:.2f}s)")
            else:
                print(f"  {bar_beat}")
    else:
        print("Could not parse any bar.beat values from response")


if __name__ == "__main__":
    main()
