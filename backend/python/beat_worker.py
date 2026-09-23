#!/usr/bin/env python3
"""
Beat grid extractor: one fixed grid per tempo section, fitted to the audio.

Pipeline:
  1. Run beat_this to get per-frame beat / downbeat probability logits.
  2. Build a tempogram (windowed autocorrelation with a 120-BPM log-prior) and
     find anchor regions — long stretches with a confident, stable tempo.
     Octave-related anchors share one consensus BPM.
  3. Merge anchors with the same tempo into sections. A single-tempo song is
     one section that spans the whole track; only a real tempo change makes a
     second section.
  4. Fit each section as a fixed grid `t0 + n * period` against a fine onset
     envelope (1.45 ms hops). beat_this runs at 20 ms frames, which is too
     coarse to place a beat on its onset. No beat or bar moves on its own.
  5. Pick the downbeat phase from the downbeat-probability curve.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys
from dataclasses import dataclass
from typing import Iterable

import numpy as np


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
        "--bpm-min",
        type=float,
        default=70.0,
        help="Lower BPM bound for the fixed-grid search.",
    )
    parser.add_argument(
        "--bpm-max",
        type=float,
        default=170.0,
        help="Upper BPM bound for the fixed-grid search.",
    )
    return parser.parse_args()


def serialize(values):
    return [float(value) for value in values]


def sigmoid(x):
    return 1.0 / (1.0 + math.exp(-x)) if isinstance(x, (float, int)) else 1.0 / (1.0 + np.exp(-x))


def _sigmoid_array(arr):
    return 1.0 / (1.0 + np.exp(-arr))


def _interpolate_at(times, values, query):
    return np.interp(query, times, values, left=0.0, right=0.0)


def _score_joint(beat_probs, downbeat_probs, times, duration,
                 bpm, beat_phase, beats_per_bar, alpha=1.0, beta=1.0):
    """Score a (bpm, beat_phase, downbeat_index_in_bar) candidate.

    Returns (best_downbeat_index, joint_score, beat_grid, downbeat_grid).

    Constrains downbeats to land on a beat — never off-beat — by searching
    only the bpb candidate phases beat_phase + k*period for k in 0..bpb-1.
    """
    period = 60.0 / bpm
    beat_grid = np.arange(beat_phase, duration, period)
    if len(beat_grid) == 0:
        return 0, -np.inf, beat_grid, np.array([])
    beat_score = float(_interpolate_at(times, beat_probs, beat_grid).mean())

    bar_period = period * beats_per_bar
    best_idx, best_joint, best_db_grid = 0, -np.inf, np.array([])
    for db_idx in range(beats_per_bar):
        db_phase = beat_phase + db_idx * period
        db_grid = np.arange(db_phase, duration, bar_period)
        if len(db_grid) == 0:
            continue
        db_score = float(_interpolate_at(times, downbeat_probs, db_grid).mean())
        joint = alpha * beat_score + beta * db_score
        if joint > best_joint:
            best_joint = joint
            best_idx = db_idx
            best_db_grid = db_grid
    return best_idx, best_joint, beat_grid, best_db_grid


@dataclass
class GridResult:
    bpm: float
    offset: float
    beats_per_bar: int
    beats: list[float]
    downbeats: list[float]


def fixed_bpm_from_logits(
    beat_logits,
    downbeat_logits,
    hop_seconds,
    bpm_min=70.0,
    bpm_max=170.0,
    beats_per_bar=4,
    alpha=1.0,
    beta=1.0,
):
    times = np.arange(len(beat_logits)) * hop_seconds
    duration = times[-1] if len(times) else 0.0
    beat_probs = _sigmoid_array(beat_logits)
    downbeat_probs = _sigmoid_array(downbeat_logits)

    # coarse sweep: 1 BPM steps × 24 beat-phase candidates × bpb downbeat indices
    best = None  # (joint_score, bpm, beat_phase, db_idx, beats, downbeats)
    bpm_grid = np.arange(bpm_min, bpm_max + 1e-6, 1.0)
    for bpm in bpm_grid:
        period = 60.0 / bpm
        phases = np.linspace(0, period, num=24, endpoint=False)
        for ph in phases:
            db_idx, score, bgrid, dgrid = _score_joint(
                beat_probs, downbeat_probs, times, duration,
                bpm, float(ph), beats_per_bar, alpha=alpha, beta=beta,
            )
            if best is None or score > best[0]:
                best = (score, float(bpm), float(ph), int(db_idx), bgrid, dgrid)

    # refine around the best BPM, finer phase
    _, bpm_b, ph_b, _, _, _ = best
    fine_bpms = np.arange(max(bpm_min, bpm_b - 4), min(bpm_max, bpm_b + 4) + 1e-6, 0.1)
    for bpm in fine_bpms:
        period = 60.0 / bpm
        phases = np.linspace(ph_b - 0.25 * period, ph_b + 0.25 * period, num=48, endpoint=False)
        phases = phases % period  # keep within [0, period)
        for ph in phases:
            db_idx, score, bgrid, dgrid = _score_joint(
                beat_probs, downbeat_probs, times, duration,
                bpm, float(ph), beats_per_bar, alpha=alpha, beta=beta,
            )
            if score > best[0]:
                best = (score, float(bpm), float(ph), int(db_idx), bgrid, dgrid)

    # second refinement at 0.01 BPM steps — catches sub-0.1-BPM drift cases
    # (Doses/Gimme/The Spins) where the true tempo is e.g. 127.00 vs the
    # 0.1-grid landing on 126.9
    _, bpm_b, ph_b, _, _, _ = best
    finer_bpms = np.arange(max(bpm_min, bpm_b - 0.2), min(bpm_max, bpm_b + 0.2) + 1e-6, 0.01)
    for bpm in finer_bpms:
        period = 60.0 / bpm
        phases = np.linspace(ph_b - 0.05 * period, ph_b + 0.05 * period, num=16, endpoint=False)
        phases = phases % period
        for ph in phases:
            db_idx, score, bgrid, dgrid = _score_joint(
                beat_probs, downbeat_probs, times, duration,
                bpm, float(ph), beats_per_bar, alpha=alpha, beta=beta,
            )
            if score > best[0]:
                best = (score, float(bpm), float(ph), int(db_idx), bgrid, dgrid)

    score, bpm, beat_phase, db_idx, beats, downbeats = best
    return GridResult(
        bpm=float(bpm),
        offset=float(beat_phase + db_idx * (60.0 / bpm)),
        beats_per_bar=int(beats_per_bar),
        beats=serialize(beats),
        downbeats=serialize(downbeats),
    )


# ---------------------------------------------------------------------------
# Multi-anchor tempo segmentation
# ---------------------------------------------------------------------------


def _tempo_prior(bpm_axis, mu=120.0, sigma_oct=0.6):
    return np.exp(-0.5 * (np.log2(bpm_axis / mu) / sigma_oct) ** 2)


def _parabolic_peak(y, idx):
    if idx <= 0 or idx >= len(y) - 1:
        return float(idx)
    y0, y1, y2 = float(y[idx - 1]), float(y[idx]), float(y[idx + 1])
    denom = y0 - 2.0 * y1 + y2
    if abs(denom) < 1e-12:
        return float(idx)
    return float(idx) + 0.5 * (y0 - y2) / denom


def compute_tempogram(beat_probs, hop, window_sec=6.0, step_sec=0.5,
                      bpm_min=70.0, bpm_max=170.0, prior_mu=120.0):
    """Local-tempo curve via windowed autocorrelation with a perceptual prior."""
    window_frames = int(window_sec / hop)
    step_frames = int(step_sec / hop)
    lag_min = int(60.0 / bpm_max / hop)
    lag_max = int(60.0 / bpm_min / hop)
    lag_axis = np.arange(lag_min, lag_max + 1)
    bpm_axis = 60.0 / (lag_axis * hop)
    prior = _tempo_prior(bpm_axis, mu=prior_mu)

    times, bpms, confs = [], [], []
    for start in range(0, max(0, len(beat_probs) - window_frames), step_frames):
        w = beat_probs[start: start + window_frames]
        w = w - w.mean()
        n = window_frames * 2
        f = np.fft.rfft(w, n=n)
        ac = np.fft.irfft(f * np.conj(f))[:window_frames]
        ac = ac / (ac[0] + 1e-9)
        seg = ac[lag_min: lag_max + 1] * prior
        rel = int(np.argmax(seg))
        sub = _parabolic_peak(seg, rel)
        lag_frac = lag_min + sub
        bpm = 60.0 / (lag_frac * hop)
        times.append((start + window_frames / 2) * hop)
        bpms.append(bpm)
        confs.append(float(ac[lag_min + rel]))
    return np.array(times), np.array(bpms), np.array(confs)


def _find_anchors(times, bpms, confs, step_sec=0.5,
                  min_sec=15.0, sigma_max=1.2, conf_min=0.65):
    """Find long, stable, high-confidence tempo regions."""
    n = len(times)
    if n == 0:
        return []
    anchors = []
    i = 0
    while i < n:
        if confs[i] < conf_min:
            i += 1
            continue
        j = i
        while j < n and confs[j] >= conf_min:
            run = bpms[i: j + 1]
            if len(run) >= 3 and np.std(run) > sigma_max:
                break
            j += 1
        run_len = (j - i) * step_sec
        if run_len >= min_sec and j > i:
            anchors.append({
                "t_start": float(times[i]),
                "t_end": float(times[j - 1]),
                "bpm_median": float(np.median(bpms[i:j])),
                "i_start": int(i),
                "i_end": int(j - 1),
            })
        i = max(j, i + 1)
    return anchors


def _fit_anchor(beat_logits, db_logits, hop, anchor, target_bpm=None):
    """Joint-fit (BPM, phase) inside the anchor's audio slice.

    If `target_bpm` is given (from cluster consensus across all anchors), the
    search range is centered on that — overrides the anchor's own tempogram
    estimate. This is how octave-confused anchors get pulled to the
    rest-of-song's tempo.
    """
    f_start = int(anchor["t_start"] / hop)
    f_end = int(anchor["t_end"] / hop)
    bl = beat_logits[f_start: f_end]
    dl = db_logits[f_start: f_end]
    bpm_est = target_bpm if target_bpm is not None else anchor["bpm_median"]
    lo = max(60.0, bpm_est - 5.0)
    hi = min(200.0, bpm_est + 5.0)
    grid = fixed_bpm_from_logits(bl, dl, hop, bpm_min=lo, bpm_max=hi)
    return {
        "bpm": float(grid.bpm),
        "beats_per_bar": int(grid.beats_per_bar),
        "beats_abs": [b + anchor["t_start"] for b in grid.beats],
        "downbeats_abs": [d + anchor["t_start"] for d in grid.downbeats],
        "anchor": anchor,
    }




def _is_octave_halved(downbeats_abs, db_probs, hop, tol_ms=60.0, ratio=0.65):
    """Detect the 'we fit half the true tempo' case by checking downbeat-prob
    at midpoints between detected downbeats.

    If the real tempo is 2×, every midpoint is itself a real downbeat that
    we missed — its db-prob will be comparable to the on-beat db-prob. If we
    fit the correct tempo, midpoints land on snares (low db-prob).
    """
    if len(downbeats_abs) < 4:
        return False
    tol_frames = max(1, int(tol_ms / 1000 / hop))

    def peak_at(t):
        idx = int(round(t / hop))
        lo = max(0, idx - tol_frames)
        hi = min(len(db_probs), idx + tol_frames + 1)
        return float(db_probs[lo:hi].max()) if hi > lo else 0.0

    on_peaks = [peak_at(d) for d in downbeats_abs]
    mids = [(downbeats_abs[i] + downbeats_abs[i + 1]) / 2 for i in range(len(downbeats_abs) - 1)]
    mid_peaks = [peak_at(m) for m in mids]
    on_med = float(np.median(on_peaks))
    mid_med = float(np.median(mid_peaks))
    return mid_med > ratio * on_med and on_med > 0.3


def _consensus_bpms(anchors):
    """Cluster anchors by octave-equivalent BPM, return per-anchor target.

    Two anchors are octave-equivalent if their BPMs match within ±5 after a
    factor of 0.5, 1, or 2. Connected components form clusters. Each cluster
    picks the octave with the most total anchor duration; that octave's
    median BPM becomes the target for *every* anchor in the cluster.

    This recognises the "they're all really the same tempo" case (Dubstep
    Never Dies) while leaving genuine tempo changes (Afraid to Feel) alone:
    anchors at 128 BPM and 100 BPM aren't octave-related, so they stay
    independent.
    """
    n = len(anchors)
    parent = list(range(n))

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    def union(i, j):
        parent[find(i)] = find(j)

    for i in range(n):
        for j in range(i + 1, n):
            bi = anchors[i]["bpm_median"]
            bj = anchors[j]["bpm_median"]
            for ratio in (1.0, 2.0, 0.5):
                if abs(bi * ratio - bj) < 5.0:
                    union(i, j)
                    break

    groups = {}
    for i in range(n):
        groups.setdefault(find(i), []).append(i)

    target = {}
    for group in groups.values():
        ref = anchors[group[0]]["bpm_median"]
        # bucket each member by which octave it sits in relative to ref
        buckets = {1.0: [], 2.0: [], 0.5: []}
        for i in group:
            b = anchors[i]["bpm_median"]
            d = anchors[i]["t_end"] - anchors[i]["t_start"]
            for factor in (1.0, 2.0, 0.5):
                if abs(b * factor - ref) < 5.0:
                    buckets[factor].append((b, d, i))
                    break
        dominant = max(buckets.keys(), key=lambda f: sum(d for _, d, _ in buckets[f]))
        if not buckets[dominant]:
            continue
        consensus = float(np.median([b for b, _, _ in buckets[dominant]]))
        for i in group:
            target[i] = consensus
    return target


def _extend_anchor(fit, db_probs, hop, duration,
                   tol_ms=60.0, peak_floor=0.5, peak_ratio=0.7, miss_budget=1):
    """Extend anchor by predicting downbeat positions and checking against
    the downbeat-prob curve. Returns (beats_extended, downbeats_extended)."""
    period = 60.0 / fit["bpm"]
    bpb = fit["beats_per_bar"]
    bar = period * bpb
    tol = tol_ms / 1000.0
    tol_frames = max(1, int(tol / hop))

    def peak_height(t):
        if t < 0 or t > duration:
            return 0.0
        idx = int(round(t / hop))
        lo = max(0, idx - tol_frames)
        hi = min(len(db_probs), idx + tol_frames + 1)
        if hi <= lo:
            return 0.0
        return float(db_probs[lo:hi].max())

    downbeats = list(fit["downbeats_abs"])
    if len(downbeats) < 2:
        return list(fit["beats_abs"]), downbeats

    anchor_median = float(np.median([peak_height(d) for d in downbeats]))
    threshold = max(peak_floor, anchor_median * peak_ratio)

    def ok(t):
        return peak_height(t) >= threshold

    # backwards bar-by-bar
    bwd_db = []
    miss = 0
    t = downbeats[0] - bar
    while t >= 0:
        if ok(t):
            bwd_db.append(t)
            miss = 0
        else:
            miss += 1
            if miss > miss_budget:
                break
            bwd_db.append(t)
        t -= bar
    while bwd_db and not ok(bwd_db[-1]):
        bwd_db.pop()

    # forwards bar-by-bar
    fwd_db = []
    miss = 0
    t = downbeats[-1] + bar
    while t <= duration:
        if ok(t):
            fwd_db.append(t)
            miss = 0
        else:
            miss += 1
            if miss > miss_budget:
                break
            fwd_db.append(t)
        t += bar
    while fwd_db and not ok(fwd_db[-1]):
        fwd_db.pop()

    all_db = sorted(bwd_db) + downbeats + fwd_db
    # rebuild beats from extended downbeat range, beat-spaced
    if not all_db:
        return list(fit["beats_abs"]), []
    first = all_db[0]
    last = all_db[-1] + bar  # cover the final bar's beats
    all_beats = []
    t = first
    while t < last and t <= duration:
        all_beats.append(t)
        t += period
    return all_beats, all_db


# ---------------------------------------------------------------------------
# Tempo sections
# ---------------------------------------------------------------------------


def _tempo_regions(beat_logits, downbeat_logits, hop_seconds, bpm_min, bpm_max):
    """Confident fixed-tempo regions as (t_start, t_end, bpm, beats_per_bar),
    sorted by time. Empty when no anchor is stable enough."""
    beat_probs = _sigmoid_array(beat_logits)
    db_probs = _sigmoid_array(downbeat_logits)
    duration = (len(beat_probs) - 1) * hop_seconds if len(beat_probs) else 0.0

    tg_t, tg_b, tg_c = compute_tempogram(
        beat_probs, hop_seconds, bpm_min=bpm_min, bpm_max=bpm_max,
    )
    anchors = _find_anchors(tg_t, tg_b, tg_c)
    if not anchors:
        return []

    # cluster anchors by octave-equivalent BPM; each anchor gets a target
    # BPM that all members of its cluster share
    targets = _consensus_bpms(anchors)
    anchor_order = sorted(range(len(anchors)),
                          key=lambda i: anchors[i]["t_end"] - anchors[i]["t_start"],
                          reverse=True)
    fits = {
        idx: _fit_anchor(beat_logits, downbeat_logits, hop_seconds, anchors[idx],
                         target_bpm=targets.get(idx))
        for idx in anchor_order
    }

    # Global doubling vote, weighted by anchor duration. Short intros with a
    # different rhythmic feel don't get to override a long main section.
    yes_dur = no_dur = 0.0
    for idx in anchor_order:
        a = anchors[idx]
        fit = fits[idx]
        halved = (fit["bpm"] * 2 < 200.0) and _is_octave_halved(
            fit["downbeats_abs"], db_probs, hop_seconds
        )
        if halved:
            yes_dur += a["t_end"] - a["t_start"]
        else:
            no_dur += a["t_end"] - a["t_start"]
    needs_doubling = yes_dur > no_dur

    regions = []

    def overlaps(t0, t1):
        return any(not (t1 < r[0] or t0 > r[1]) for r in regions)

    for idx in anchor_order:
        a = anchors[idx]
        if overlaps(a["t_start"], a["t_end"]):
            continue
        fit = fits[idx]
        if needs_doubling and fit["bpm"] * 2 < 200.0:
            fit = _fit_anchor(beat_logits, downbeat_logits, hop_seconds, a,
                              target_bpm=fit["bpm"] * 2)
        beats, _ = _extend_anchor(fit, db_probs, hop_seconds, duration)
        if not beats:
            continue
        start, end = beats[0], beats[-1]
        for r in regions:
            if start < r[0] <= end:
                end = r[0] - 1e-3
            if start <= r[1] < end:
                start = r[1] + 1e-3
        if end > start:
            regions.append((start, end, fit["bpm"], fit["beats_per_bar"]))
    regions.sort(key=lambda r: r[0])
    return regions


def _sections(regions, duration, same_tempo_bpm=1.0):
    """Merge same-tempo regions and cover the whole track.

    Returns (t_start, t_end, bpm_hint, beats_per_bar) spans. The seam between
    two real tempo sections sits halfway through the gap between them.
    """
    merged = []
    for start, end, bpm, bpb in regions:
        if merged and abs(merged[-1][2] - bpm) < same_tempo_bpm:
            prev = merged[-1]
            weight = prev[1] - prev[0]
            span = end - start
            hint = (prev[2] * weight + bpm * span) / max(weight + span, 1e-9)
            merged[-1] = (prev[0], end, hint, prev[3])
        else:
            merged.append((start, end, bpm, bpb))
    sections = []
    for i, (start, end, bpm, bpb) in enumerate(merged):
        lo = 0.0 if i == 0 else 0.5 * (merged[i - 1][1] + start)
        hi = duration if i == len(merged) - 1 else 0.5 * (end + merged[i + 1][0])
        sections.append((lo, hi, bpm, bpb))
    return sections


# ---------------------------------------------------------------------------
# Onset-fitted fixed grid
# ---------------------------------------------------------------------------

ONSET_SR = 44100
ONSET_HOP = 64
ONSET_FPS = ONSET_SR / ONSET_HOP


def onset_envelope(audio_path):
    import librosa

    y, _ = librosa.load(str(audio_path), sr=ONSET_SR, mono=True)
    return librosa.onset.onset_strength(
        y=y, sr=ONSET_SR, hop_length=ONSET_HOP, n_fft=2048, fmax=8000,
        lag=1, max_size=1,
    )


def _comb(strength, bpms, t_start, t_end, step_sec=0.001):
    """(bpm, t0) whose grid t0 + k*period over [t_start, t_end) collects the
    most onset strength."""
    best = (-np.inf, float(bpms[0]), t_start)
    for bpm in bpms:
        period = 60.0 / bpm
        k = np.arange(max(1, int((t_end - t_start) / period)))
        phases = t_start + np.arange(0.0, period, step_sec)
        idx = np.rint((phases[:, None] + k[None, :] * period) * ONSET_FPS).astype(int)
        score = strength[np.clip(idx, 0, len(strength) - 1)].sum(axis=1)
        j = int(np.argmax(score))
        if score[j] > best[0]:
            best = (float(score[j]), float(bpm), float(phases[j]))
    return best[1], best[2]


def _refine(envelope, period, t0, t_start, t_end):
    """Robust straight-line fit of (beat index, onset time) pairs. Each pass
    narrows the search window and drops onsets far from the line, so hats and
    off-beat hits don't pull the grid."""
    threshold = np.percentile(envelope, 90)
    for window, trim in ((0.03, 0.012), (0.02, 0.008)):
        n = np.arange(int(np.ceil((t_start - t0) / period)),
                      int((t_end - t0) / period) + 1)
        w = int(window * ONSET_FPS)
        idx, times = [], []
        for k in n:
            c = int(round((t0 + k * period) * ONSET_FPS))
            lo, hi = max(0, c - w), min(len(envelope), c + w + 1)
            if hi <= lo:
                continue
            peak = lo + int(np.argmax(envelope[lo:hi]))
            if envelope[peak] >= threshold:
                idx.append(k)
                times.append(peak / ONSET_FPS)
        idx, times = np.asarray(idx, float), np.asarray(times)
        for _ in range(3):
            if len(idx) < 16:
                return period, t0
            slope, icept = np.polyfit(idx, times, 1)
            keep = np.abs(times - (slope * idx + icept)) < trim
            idx, times = idx[keep], times[keep]
        if len(idx) < 16:
            return period, t0
        period, t0 = np.polyfit(idx, times, 1)
    return float(period), float(t0)


def fit_section(envelope, bpm_hint, t_start, t_end):
    """Fixed (period, t0) for one tempo section."""
    strength = np.log1p(envelope / (np.median(envelope[envelope > 0]) + 1e-9))
    bpm, t0 = _comb(strength, np.arange(bpm_hint - 0.6, bpm_hint + 0.6, 0.02), t_start, t_end)
    bpm, t0 = _comb(strength, np.arange(bpm - 0.03, bpm + 0.03, 0.001), t_start, t_end)
    return _refine(envelope, 60.0 / bpm, t0, t_start, t_end)


def _downbeat_index(beats, db_probs, hop_seconds, beats_per_bar):
    times = np.arange(len(db_probs)) * hop_seconds
    scores = [
        float(_interpolate_at(times, db_probs, beats[k::beats_per_bar]).mean())
        if len(beats[k::beats_per_bar]) else -np.inf
        for k in range(beats_per_bar)
    ]
    return int(np.argmax(scores))


def fixed_grid(beat_logits, downbeat_logits, hop_seconds, envelope,
               bpm_min=70.0, bpm_max=170.0):
    """Top-level entry. Returns (bpm, downbeat_offset, beats_per_bar, beats,
    downbeats). `bpm` is the longest section's tempo."""
    duration = min((len(beat_logits) - 1) * hop_seconds, len(envelope) / ONSET_FPS)
    regions = _tempo_regions(beat_logits, downbeat_logits, hop_seconds, bpm_min, bpm_max)
    if not regions:
        grid = fixed_bpm_from_logits(
            beat_logits, downbeat_logits, hop_seconds, bpm_min=bpm_min, bpm_max=bpm_max,
        )
        regions = [(0.0, duration, grid.bpm, grid.beats_per_bar)]
    db_probs = _sigmoid_array(downbeat_logits)

    all_beats, all_downbeats = [], []
    primary = (0.0, 0.0, 4)  # (length, bpm, beats_per_bar)
    for t_start, t_end, bpm_hint, bpb in _sections(regions, duration):
        period, t0 = fit_section(envelope, bpm_hint, t_start, t_end)
        n = np.arange(int(np.ceil((t_start - t0) / period)),
                      int(np.ceil((t_end - t0) / period)))
        beats = t0 + n * period
        beats = beats[(beats >= 0.0) & (beats < t_end) & (beats <= duration)]
        if len(beats) == 0:
            continue
        k = _downbeat_index(beats, db_probs, hop_seconds, bpb)
        all_beats.extend(beats)
        all_downbeats.extend(beats[k::bpb])
        if t_end - t_start > primary[0]:
            primary = (t_end - t_start, 60.0 / period, bpb)

    all_beats = [round(float(b), 4) for b in all_beats]
    all_downbeats = [round(float(d), 4) for d in all_downbeats]
    offset = all_downbeats[0] if all_downbeats else 0.0
    return primary[1], offset, primary[2], all_beats, all_downbeats


def _detect_device() -> str:
    """CUDA when a real kernel runs there, else CPU."""
    import torch

    if torch.cuda.is_available():
        try:
            torch.zeros(1, device="cuda")
            return "cuda"
        except RuntimeError:
            pass
    return "cpu"


def main() -> int:
    args = parse_args()

    try:
        from beat_this.inference import Audio2Frames
        from beat_this.preprocessing import load_audio
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
        signal, sr = load_audio(args.audio_file)
        tracker = Audio2Frames(checkpoint_path=str(args.checkpoint), device=_detect_device(), float16=False)
        beat_logits, downbeat_logits = tracker(signal, sr)
        hop_seconds = 441 / 22050  # matches beat_this preprocessing
        bpm, offset, bpb, beats, downbeats = fixed_grid(
            beat_logits.cpu().numpy(),
            downbeat_logits.cpu().numpy(),
            hop_seconds,
            onset_envelope(args.audio_file),
            bpm_min=args.bpm_min,
            bpm_max=args.bpm_max,
        )
    except Exception as exc:  # pragma: no cover - runtime error reporting
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 1

    payload = {
        "beats": serialize(beats),
        "downbeats": serialize(downbeats),
        "bpm": float(bpm),
        "downbeat_offset": float(offset),
        "beats_per_bar": int(bpb),
    }
    sys.stdout.write(json.dumps(payload))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover - script entrypoint
    raise SystemExit(main())
