#!/usr/bin/env python3
"""
Per-bar genre worker — Discogs-EffNet (400 Discogs styles) via ONNX Runtime.

  ⚠ LICENSE — READ BEFORE SHIPPING ⚠
  The model this worker runs, `discogs-effnet-bsdynamic-1.onnx`, comes from the
  Essentia / MTG-UPF model zoo (https://essentia.upf.edu/models.html). Every
  pretrained model in that zoo is published under **CC BY-NC-ND 4.0**:
  Attribution, **NonCommercial**, **NoDerivatives**. MTG offers a proprietary
  license on request (see https://essentia.upf.edu/licensing_information.html).
  That means, as licensed:
    - commercial distribution of Luma with these weights is NOT permitted;
    - redistributing modified/converted weights is NOT permitted (ND) — note the
      ONNX file itself is a conversion of the upstream TensorFlow model;
    - attribution to MTG-UPF is required wherever the output surfaces.
  Nothing here is bundled: the weights are never committed and never downloaded
  by Luma. The user places the file themselves, which keeps local/dev use clean.
  Shipping this in a paid build requires the proprietary license from MTG first.

Pipeline (ported from binyl's embed_worker.py, single-shot and bar-aware):
  1. Decode — ffmpeg subprocess to 16 kHz mono f32.
  2. Mel patches — torchaudio, matching Essentia's TensorflowInputMusiCNN:
     n_fft=512, hop=256, 96 slaney mels, log10(1 + 10000·mel), center=False,
     patches of 128 frames (2.048 s) every 62 frames (0.992 s).
  3. ONNX — one batched forward pass over ALL patches (CoreML EP, CPU fallback)
     → (N, 400) sigmoid style activations. The model's second output (the 1280-d
     embedding) is ignored: Luma already has MERT for representation work.
  4. Aggregation — each patch is weighted by how much of it overlaps a bar, so
     bar activations are a time-accurate weighted mean; then each of the 400
     channels is median-smoothed over a 5-bar window to kill single-bar flicker.
  5. Output — per bar, a sparse top-K list of (label_index, prob) using binyl's
     "always the top 3, plus anything over 0.1, capped at K" heuristic, plus a
     whole-track top-10 from the *unsmoothed* patch mean.

Model file (never committed — 18 MB):
  discogs-effnet-bsdynamic-1.onnx, placed in $LUMA_MODELS_DIR or
  <app config dir>/models/ (macOS: ~/Library/Application Support/com.luma.luma/models/).
  Download: https://essentia.upf.edu/models.html → "Discogs-Effnet".

Usage (one-shot; bar boundaries arrive on stdin like classifier_worker.py):
  echo '[[0.0,2.0],[2.0,4.0]]' | python genre_worker.py /path/to/track.mp3
  → {"labels": [...], "bars": [...], "track_top": [...]} on stdout

`labels` is the compact list of style names actually referenced by this track;
every `label_index` in `bars` / `track_top` indexes into it, so the payload is
self-describing without carrying all 400 names.
"""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys

import numpy as np

# ffmpeg binary — overridden by LUMA_FFMPEG, set by genre_worker.rs from the
# bundled ffmpeg-runtime.
FFMPEG_BIN = os.environ.get("LUMA_FFMPEG", "ffmpeg")

MODEL_FILE = "discogs-effnet-bsdynamic-1.onnx"

# ---------------------------------------------------------------------------
# Constants — must match Essentia's TensorflowPredictEffnetDiscogs defaults
#   frameSize=512, hopSize=256, numberBands=96, patchSize=128, patchHopSize=62
# ---------------------------------------------------------------------------
SAMPLE_RATE = 16_000
FRAME_SIZE = 512  # n_fft
HOP_SIZE = 256  # hop_length
N_MEL_BANDS = 96  # numberBands
PATCH_FRAMES = 128  # patchSize (time frames per patch)
PATCH_HOP = 62  # patchHopSize (~50% overlap)

# Seconds of audio one patch spans / advances. The true tail of a patch reaches
# one analysis window further ((127·256 + 512)/16000 = 2.064 s), but the 16 ms
# difference is far below bar resolution and the round number keeps the
# overlap arithmetic legible.
PATCH_SECS = PATCH_FRAMES * HOP_SIZE / SAMPLE_RATE  # 2.048
PATCH_HOP_SECS = PATCH_HOP * HOP_SIZE / SAMPLE_RATE  # 0.992

# Bars of context for the per-channel median smoother. Odd so it is centered.
SMOOTH_BARS = 5

# Sparse per-bar output: always keep the top `MIN_RANKS`, additionally keep
# anything above `GENRE_THRESHOLD`, never more than `TOP_K` total. Lifted from
# binyl's track-level heuristic (embed_worker.py) with a tighter cap.
TOP_K = 8
MIN_RANKS = 3
GENRE_THRESHOLD = 0.1
TRACK_TOP_K = 10

# ---------------------------------------------------------------------------
# Discogs 400 labels (inline, no network required). Order is the model's output
# order — index i of the activation vector is DISCOGS_LABELS[i].
# ---------------------------------------------------------------------------
DISCOGS_LABELS = [
    "Blues---Boogie Woogie", "Blues---Chicago Blues", "Blues---Country Blues",
    "Blues---Delta Blues", "Blues---Electric Blues", "Blues---Harmonica Blues",
    "Blues---Jump Blues", "Blues---Louisiana Blues", "Blues---Modern Electric Blues",
    "Blues---Piano Blues", "Blues---Rhythm & Blues", "Blues---Texas Blues",
    "Brass & Military---Brass Band", "Brass & Military---Marches",
    "Brass & Military---Military",
    "Children's---Educational", "Children's---Nursery Rhymes", "Children's---Story",
    "Classical---Baroque", "Classical---Choral", "Classical---Classical",
    "Classical---Contemporary", "Classical---Impressionist", "Classical---Medieval",
    "Classical---Modern", "Classical---Neo-Classical", "Classical---Neo-Romantic",
    "Classical---Opera", "Classical---Post-Modern", "Classical---Renaissance",
    "Classical---Romantic",
    "Electronic---Abstract", "Electronic---Acid", "Electronic---Acid House",
    "Electronic---Acid Jazz", "Electronic---Ambient", "Electronic---Bassline",
    "Electronic---Beatdown", "Electronic---Berlin-School", "Electronic---Big Beat",
    "Electronic---Bleep", "Electronic---Breakbeat", "Electronic---Breakcore",
    "Electronic---Breaks", "Electronic---Broken Beat", "Electronic---Chillwave",
    "Electronic---Chiptune", "Electronic---Dance-pop", "Electronic---Dark Ambient",
    "Electronic---Darkwave", "Electronic---Deep House", "Electronic---Deep Techno",
    "Electronic---Disco", "Electronic---Disco Polo", "Electronic---Donk",
    "Electronic---Downtempo", "Electronic---Drone", "Electronic---Drum n Bass",
    "Electronic---Dub", "Electronic---Dub Techno", "Electronic---Dubstep",
    "Electronic---Dungeon Synth", "Electronic---EBM", "Electronic---Electro",
    "Electronic---Electro House", "Electronic---Electroclash", "Electronic---Euro House",
    "Electronic---Euro-Disco", "Electronic---Eurobeat", "Electronic---Eurodance",
    "Electronic---Experimental", "Electronic---Freestyle", "Electronic---Future Jazz",
    "Electronic---Gabber", "Electronic---Garage House", "Electronic---Ghetto",
    "Electronic---Ghetto House", "Electronic---Glitch", "Electronic---Goa Trance",
    "Electronic---Grime", "Electronic---Halftime", "Electronic---Hands Up",
    "Electronic---Happy Hardcore", "Electronic---Hard House", "Electronic---Hard Techno",
    "Electronic---Hard Trance", "Electronic---Hardcore", "Electronic---Hardstyle",
    "Electronic---Hi NRG", "Electronic---Hip Hop", "Electronic---Hip-House",
    "Electronic---House", "Electronic---IDM", "Electronic---Illbient",
    "Electronic---Industrial", "Electronic---Italo House", "Electronic---Italo-Disco",
    "Electronic---Italodance", "Electronic---Jazzdance", "Electronic---Juke",
    "Electronic---Jumpstyle", "Electronic---Jungle", "Electronic---Latin",
    "Electronic---Leftfield", "Electronic---Makina", "Electronic---Minimal",
    "Electronic---Minimal Techno", "Electronic---Modern Classical",
    "Electronic---Musique Concrète", "Electronic---Neofolk", "Electronic---New Age",
    "Electronic---New Beat", "Electronic---New Wave", "Electronic---Noise",
    "Electronic---Nu-Disco", "Electronic---Power Electronics",
    "Electronic---Progressive Breaks", "Electronic---Progressive House",
    "Electronic---Progressive Trance", "Electronic---Psy-Trance",
    "Electronic---Rhythmic Noise", "Electronic---Schranz",
    "Electronic---Sound Collage", "Electronic---Speed Garage",
    "Electronic---Speedcore", "Electronic---Synth-pop", "Electronic---Synthwave",
    "Electronic---Tech House", "Electronic---Tech Trance", "Electronic---Techno",
    "Electronic---Trance", "Electronic---Tribal", "Electronic---Tribal House",
    "Electronic---Trip Hop", "Electronic---Tropical House", "Electronic---UK Garage",
    "Electronic---Vaporwave",
    "Folk, World, & Country---African", "Folk, World, & Country---Bluegrass",
    "Folk, World, & Country---Cajun", "Folk, World, & Country---Canzone Napoletana",
    "Folk, World, & Country---Catalan Music", "Folk, World, & Country---Celtic",
    "Folk, World, & Country---Country", "Folk, World, & Country---Fado",
    "Folk, World, & Country---Flamenco", "Folk, World, & Country---Folk",
    "Folk, World, & Country---Gospel", "Folk, World, & Country---Highlife",
    "Folk, World, & Country---Hillbilly", "Folk, World, & Country---Hindustani",
    "Folk, World, & Country---Honky Tonk", "Folk, World, & Country---Indian Classical",
    "Folk, World, & Country---Laïkó", "Folk, World, & Country---Nordic",
    "Folk, World, & Country---Pacific", "Folk, World, & Country---Polka",
    "Folk, World, & Country---Raï", "Folk, World, & Country---Romani",
    "Folk, World, & Country---Soukous", "Folk, World, & Country---Séga",
    "Folk, World, & Country---Volksmusik", "Folk, World, & Country---Zouk",
    "Folk, World, & Country---Éntekhno",
    "Funk / Soul---Afrobeat", "Funk / Soul---Boogie", "Funk / Soul---Contemporary R&B",
    "Funk / Soul---Disco", "Funk / Soul---Free Funk", "Funk / Soul---Funk",
    "Funk / Soul---Gospel", "Funk / Soul---Neo Soul", "Funk / Soul---New Jack Swing",
    "Funk / Soul---P.Funk", "Funk / Soul---Psychedelic", "Funk / Soul---Rhythm & Blues",
    "Funk / Soul---Soul", "Funk / Soul---Swingbeat", "Funk / Soul---UK Street Soul",
    "Hip Hop---Bass Music", "Hip Hop---Boom Bap", "Hip Hop---Bounce",
    "Hip Hop---Britcore", "Hip Hop---Cloud Rap", "Hip Hop---Conscious",
    "Hip Hop---Crunk", "Hip Hop---Cut-up/DJ", "Hip Hop---DJ Battle Tool",
    "Hip Hop---Electro", "Hip Hop---G-Funk", "Hip Hop---Gangsta", "Hip Hop---Grime",
    "Hip Hop---Hardcore Hip-Hop", "Hip Hop---Horrorcore", "Hip Hop---Instrumental",
    "Hip Hop---Jazzy Hip-Hop", "Hip Hop---Miami Bass", "Hip Hop---Pop Rap",
    "Hip Hop---Ragga HipHop", "Hip Hop---RnB/Swing", "Hip Hop---Screw",
    "Hip Hop---Thug Rap", "Hip Hop---Trap", "Hip Hop---Trip Hop",
    "Hip Hop---Turntablism",
    "Jazz---Afro-Cuban Jazz", "Jazz---Afrobeat", "Jazz---Avant-garde Jazz",
    "Jazz---Big Band", "Jazz---Bop", "Jazz---Bossa Nova", "Jazz---Contemporary Jazz",
    "Jazz---Cool Jazz", "Jazz---Dixieland", "Jazz---Easy Listening",
    "Jazz---Free Improvisation", "Jazz---Free Jazz", "Jazz---Fusion",
    "Jazz---Gypsy Jazz", "Jazz---Hard Bop", "Jazz---Jazz-Funk", "Jazz---Jazz-Rock",
    "Jazz---Latin Jazz", "Jazz---Modal", "Jazz---Post Bop", "Jazz---Ragtime",
    "Jazz---Smooth Jazz", "Jazz---Soul-Jazz", "Jazz---Space-Age", "Jazz---Swing",
    "Latin---Afro-Cuban", "Latin---Baião", "Latin---Batucada", "Latin---Beguine",
    "Latin---Bolero", "Latin---Boogaloo", "Latin---Bossanova", "Latin---Cha-Cha",
    "Latin---Charanga", "Latin---Compas", "Latin---Cubano", "Latin---Cumbia",
    "Latin---Descarga", "Latin---Forró", "Latin---Guaguancó", "Latin---Guajira",
    "Latin---Guaracha", "Latin---MPB", "Latin---Mambo", "Latin---Mariachi",
    "Latin---Merengue", "Latin---Norteño", "Latin---Nueva Cancion", "Latin---Pachanga",
    "Latin---Porro", "Latin---Ranchera", "Latin---Reggaeton", "Latin---Rumba",
    "Latin---Salsa", "Latin---Samba", "Latin---Son", "Latin---Son Montuno",
    "Latin---Tango", "Latin---Tejano", "Latin---Vallenato",
    "Non-Music---Audiobook", "Non-Music---Comedy", "Non-Music---Dialogue",
    "Non-Music---Education", "Non-Music---Field Recording", "Non-Music---Interview",
    "Non-Music---Monolog", "Non-Music---Poetry", "Non-Music---Political",
    "Non-Music---Promotional", "Non-Music---Radioplay", "Non-Music---Religious",
    "Non-Music---Spoken Word",
    "Pop---Ballad", "Pop---Bollywood", "Pop---Bubblegum", "Pop---Chanson",
    "Pop---City Pop", "Pop---Europop", "Pop---Indie Pop", "Pop---J-pop", "Pop---K-pop",
    "Pop---Kayōkyoku", "Pop---Light Music", "Pop---Music Hall", "Pop---Novelty",
    "Pop---Parody", "Pop---Schlager", "Pop---Vocal",
    "Reggae---Calypso", "Reggae---Dancehall", "Reggae---Dub", "Reggae---Lovers Rock",
    "Reggae---Ragga", "Reggae---Reggae", "Reggae---Reggae-Pop", "Reggae---Rocksteady",
    "Reggae---Roots Reggae", "Reggae---Ska", "Reggae---Soca",
    "Rock---AOR", "Rock---Acid Rock", "Rock---Acoustic", "Rock---Alternative Rock",
    "Rock---Arena Rock", "Rock---Art Rock", "Rock---Atmospheric Black Metal",
    "Rock---Avantgarde", "Rock---Beat", "Rock---Black Metal", "Rock---Blues Rock",
    "Rock---Brit Pop", "Rock---Classic Rock", "Rock---Coldwave", "Rock---Country Rock",
    "Rock---Crust", "Rock---Death Metal", "Rock---Deathcore", "Rock---Deathrock",
    "Rock---Depressive Black Metal", "Rock---Doo Wop", "Rock---Doom Metal",
    "Rock---Dream Pop", "Rock---Emo", "Rock---Ethereal", "Rock---Experimental",
    "Rock---Folk Metal", "Rock---Folk Rock", "Rock---Funeral Doom Metal",
    "Rock---Funk Metal", "Rock---Garage Rock", "Rock---Glam", "Rock---Goregrind",
    "Rock---Goth Rock", "Rock---Gothic Metal", "Rock---Grindcore", "Rock---Grunge",
    "Rock---Hard Rock", "Rock---Hardcore", "Rock---Heavy Metal", "Rock---Indie Rock",
    "Rock---Industrial", "Rock---Krautrock", "Rock---Lo-Fi", "Rock---Lounge",
    "Rock---Math Rock", "Rock---Melodic Death Metal", "Rock---Melodic Hardcore",
    "Rock---Metalcore", "Rock---Mod", "Rock---Neofolk", "Rock---New Wave",
    "Rock---No Wave", "Rock---Noise", "Rock---Noisecore", "Rock---Nu Metal",
    "Rock---Oi", "Rock---Parody", "Rock---Pop Punk", "Rock---Pop Rock",
    "Rock---Pornogrind", "Rock---Post Rock", "Rock---Post-Hardcore", "Rock---Post-Metal",
    "Rock---Post-Punk", "Rock---Power Metal", "Rock---Power Pop",
    "Rock---Power Violence", "Rock---Prog Rock", "Rock---Progressive Metal",
    "Rock---Psychedelic Rock", "Rock---Psychobilly", "Rock---Pub Rock", "Rock---Punk",
    "Rock---Rock & Roll", "Rock---Rockabilly", "Rock---Shoegaze", "Rock---Ska",
    "Rock---Sludge Metal", "Rock---Soft Rock", "Rock---Southern Rock",
    "Rock---Space Rock", "Rock---Speed Metal", "Rock---Stoner Rock", "Rock---Surf",
    "Rock---Symphonic Rock", "Rock---Technical Death Metal", "Rock---Thrash",
    "Rock---Twist", "Rock---Viking Metal", "Rock---Yé-Yé",
    "Stage & Screen---Musical", "Stage & Screen---Score",
    "Stage & Screen---Soundtrack", "Stage & Screen---Theme",
]

assert len(DISCOGS_LABELS) == 400, f"Expected 400 labels, got {len(DISCOGS_LABELS)}"


def _log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# Model discovery
# ---------------------------------------------------------------------------


def model_search_dirs() -> list[pathlib.Path]:
    """Where the ONNX file may live, highest priority first.

    `LUMA_MODELS_DIR` is what `genre_worker.rs` sets from the app's resolved
    storage root; the rest let the worker run standalone from a shell.
    """
    dirs: list[pathlib.Path] = []
    env_dir = os.environ.get("LUMA_MODELS_DIR")
    if env_dir:
        dirs.append(pathlib.Path(env_dir))
    home = pathlib.Path.home()
    if sys.platform == "darwin":
        dirs.append(home / "Library" / "Application Support" / "com.luma.luma" / "models")
    elif sys.platform == "win32":
        appdata = os.environ.get("APPDATA")
        if appdata:
            dirs.append(pathlib.Path(appdata) / "com.luma.luma" / "models")
    else:
        dirs.append(home / ".config" / "com.luma.luma" / "models")
    dirs.append(pathlib.Path(__file__).parent)
    return dirs


def find_model_path() -> pathlib.Path:
    dirs = model_search_dirs()
    for candidate in dirs:
        path = candidate / MODEL_FILE
        if path.exists():
            return path
    searched = "\n  ".join(str(d) for d in dirs)
    raise FileNotFoundError(
        f"Genre model '{MODEL_FILE}' not found. Download Discogs-Effnet "
        f"(ONNX, dynamic batch) from https://essentia.upf.edu/models.html and "
        f"place it in the first directory below (~18 MB):\n  {searched}\n"
        f"Note the Essentia model zoo is CC BY-NC-ND 4.0 (non-commercial)."
    )


# ---------------------------------------------------------------------------
# Stage 1: audio decode
# ---------------------------------------------------------------------------


def decode_audio(path: str) -> np.ndarray:
    """Decode any audio file to 16 kHz mono float32 via ffmpeg."""
    cmd = [
        FFMPEG_BIN, "-hide_banner", "-loglevel", "error",
        "-i", path,
        "-ar", str(SAMPLE_RATE),
        "-ac", "1",
        "-f", "f32le",
        "pipe:1",
    ]
    result = subprocess.run(cmd, capture_output=True, timeout=300)
    if result.returncode != 0:
        raise RuntimeError(
            f"ffmpeg failed decoding {path}: "
            f"{result.stderr.decode(errors='replace').strip()}"
        )
    audio = np.frombuffer(result.stdout, dtype=np.float32)
    if audio.size == 0:
        raise RuntimeError(f"ffmpeg produced no samples for {path}")
    return audio


# ---------------------------------------------------------------------------
# Stage 2: mel spectrogram → patches
# ---------------------------------------------------------------------------


def audio_to_patches(audio: np.ndarray) -> np.ndarray:
    """16 kHz mono float32 → (N, 128, 96) float32 mel patches.

    Matches Essentia's TensorflowInputMusiCNN: slaney mel scale + unit-triangle
    normalization, `center=False` (Essentia's `startFromZero=True`), and
    log10(1 + 10000·power) compression. Unlike binyl this never subsamples —
    every patch is scored, because the whole point is per-bar resolution.
    """
    import torch
    import torchaudio.transforms as T

    mel_transform = T.MelSpectrogram(
        sample_rate=SAMPLE_RATE,
        n_fft=FRAME_SIZE,
        hop_length=HOP_SIZE,
        n_mels=N_MEL_BANDS,
        f_min=0.0,
        f_max=8000.0,
        mel_scale="slaney",  # Essentia warpingFormula='slaneyMel'
        norm="slaney",       # Essentia normalize='unit_tri'
        power=2.0,
        center=False,
    )

    audio_t = torch.from_numpy(audio.copy()).unsqueeze(0)  # (1, N)
    mel = mel_transform(audio_t).squeeze(0)  # (96, n_frames)
    log_mel = torch.log10(1.0 + 10000.0 * mel).T.numpy().astype(np.float32)

    n_frames = log_mel.shape[0]
    if n_frames < PATCH_FRAMES:
        return np.empty((0, PATCH_FRAMES, N_MEL_BANDS), dtype=np.float32)
    starts = range(0, n_frames - PATCH_FRAMES + 1, PATCH_HOP)
    return np.stack([log_mel[s: s + PATCH_FRAMES] for s in starts], axis=0)


# ---------------------------------------------------------------------------
# Stage 3: ONNX inference
# ---------------------------------------------------------------------------


def run_onnx(model_path: pathlib.Path, patches: np.ndarray) -> np.ndarray:
    """All patches through the dynamic-batch model in one call → (N, 400).

    The model also emits a 1280-d embedding as output[1]; Luma ignores it (MERT
    already covers representation features), but it is still computed — the
    graph is shared up to the penultimate layer, so there is nothing to save.
    """
    import onnxruntime as ort

    providers = [
        ("CoreMLExecutionProvider", {"MLComputeUnits": "ALL"}),
        "CPUExecutionProvider",
    ]
    session = ort.InferenceSession(str(model_path), providers=providers)
    active = session.get_providers()
    _log(f"[genre] {model_path.name} — providers: {active}")

    input_name = session.get_inputs()[0].name  # "melspectrogram"
    activations = session.run(None, {input_name: patches})[0]
    return np.asarray(activations, dtype=np.float32)


# ---------------------------------------------------------------------------
# Stage 4: patch → bar aggregation
# ---------------------------------------------------------------------------


def patch_times(n_patches: int) -> tuple[np.ndarray, np.ndarray]:
    """(starts, ends) in seconds for `n_patches` consecutive mel patches."""
    starts = np.arange(n_patches, dtype=np.float64) * PATCH_HOP_SECS
    return starts, starts + PATCH_SECS


def overlap_weights(
    bars: list[tuple[float, float]], n_patches: int
) -> np.ndarray:
    """(n_bars, n_patches) row-normalized time-overlap weights.

    Each patch contributes to a bar in proportion to the seconds the two share.
    A bar that overlaps no patch at all — the synthetic final bar can extend
    past the decoded audio — falls back to the single nearest patch by center
    distance, so every bar gets a real prediction instead of a hole.
    """
    p_start, p_end = patch_times(n_patches)
    b_start = np.array([b[0] for b in bars], dtype=np.float64)[:, None]
    b_end = np.array([b[1] for b in bars], dtype=np.float64)[:, None]

    overlap = np.minimum(b_end, p_end[None, :]) - np.maximum(b_start, p_start[None, :])
    weights = np.clip(overlap, 0.0, None)

    totals = weights.sum(axis=1)
    empty = np.flatnonzero(totals <= 0.0)
    if empty.size:
        p_center = (p_start + p_end) / 2.0
        b_center = ((b_start + b_end) / 2.0)[empty, 0]
        nearest = np.abs(p_center[None, :] - b_center[:, None]).argmin(axis=1)
        weights[empty, :] = 0.0
        weights[empty, nearest] = 1.0
        totals = weights.sum(axis=1)

    return weights / totals[:, None]


def aggregate_to_bars(
    activations: np.ndarray, bars: list[tuple[float, float]]
) -> np.ndarray:
    """(n_patches, 400) patch activations → (n_bars, 400) bar activations."""
    return overlap_weights(bars, activations.shape[0]) @ activations


def median_smooth(bar_activations: np.ndarray, window: int = SMOOTH_BARS) -> np.ndarray:
    """Centered per-channel median filter over `window` bars, edges replicated.

    Genre activations flicker bar to bar — one breakdown bar reading as Ambient
    inside a house track is noise, not a genre change. The median (not a mean)
    keeps real transitions sharp while dropping single-bar spikes.
    """
    if window <= 1 or bar_activations.shape[0] == 0:
        return bar_activations
    pad = window // 2
    padded = np.pad(bar_activations, ((pad, pad), (0, 0)), mode="edge")
    stacked = np.stack([padded[i: i + bar_activations.shape[0]] for i in range(window)])
    return np.median(stacked, axis=0)


def sparse_top(
    probs: np.ndarray, top_k: int, min_ranks: int = MIN_RANKS
) -> list[tuple[int, float]]:
    """Confidence-sorted (label_index, prob) pairs.

    binyl's heuristic: always keep the top `min_ranks` however weak, plus
    anything above `GENRE_THRESHOLD`, capped at `top_k`. The floor matters —
    for an unusual track every activation can sit under 0.1 and a pure
    threshold would emit nothing at all.
    """
    order = np.argsort(probs)[::-1][:top_k]
    out: list[tuple[int, float]] = []
    for rank, idx in enumerate(order):
        conf = float(probs[idx])
        if rank < min_ranks or conf > GENRE_THRESHOLD:
            out.append((int(idx), conf))
    return out


def build_payload(
    activations: np.ndarray, bars: list[tuple[float, float]]
) -> dict:
    """Full aggregation: patches + bar boundaries → the emitted JSON payload."""
    bar_probs = median_smooth(aggregate_to_bars(activations, bars))
    # Whole-track summary comes from the raw patch mean: smoothing exists to
    # stabilize the per-bar series, and would only blur a track-level average.
    track_probs = activations.mean(axis=0)

    per_bar = [sparse_top(row, TOP_K) for row in bar_probs]
    track_top = sparse_top(track_probs, TRACK_TOP_K)

    # Compact the 400-label taxonomy down to what this track actually uses, and
    # renumber every pair against it — the payload stays self-describing without
    # carrying 400 strings per track.
    used = sorted({idx for pairs in per_bar for idx, _ in pairs} | {i for i, _ in track_top})
    remap = {orig: i for i, orig in enumerate(used)}
    labels = [DISCOGS_LABELS[i] for i in used]

    return {
        "labels": labels,
        "bars": [
            {
                "bar_idx": i,
                "start": float(start),
                "end": float(end),
                "top": [[remap[idx], round(p, 5)] for idx, p in pairs],
            }
            for i, ((start, end), pairs) in enumerate(zip(bars, per_bar))
        ],
        "track_top": [[remap[idx], round(p, 5)] for idx, p in track_top],
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    if len(sys.argv) < 2:
        _log("usage: genre_worker.py <audio_path>  (bar boundaries JSON on stdin)")
        sys.exit(2)
    audio_path = sys.argv[1]

    bars_raw = json.loads(sys.stdin.read())
    bars = [(float(s), float(e)) for s, e in bars_raw]
    if not bars:
        raise ValueError("no bar boundaries supplied on stdin")

    model_path = find_model_path()
    audio = decode_audio(audio_path)
    patches = audio_to_patches(audio)
    if patches.shape[0] == 0:
        raise RuntimeError(
            f"{audio_path} is shorter than one {PATCH_SECS:.3f}s analysis patch"
        )
    _log(f"[genre] {patches.shape[0]} patches over {len(audio) / SAMPLE_RATE:.1f}s")

    activations = run_onnx(model_path, patches)
    print(json.dumps(build_payload(activations, bars)), flush=True)


if __name__ == "__main__":
    main()
