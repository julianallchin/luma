#!/usr/bin/env python3
"""
Beat grid extractor with multi-anchor tempo segmentation.

Pipeline:
  1. Run beat_this to get per-frame beat / downbeat probability logits.
  2. Build a tempogram (windowed autocorrelation with a 120-BPM log-prior) and
     find anchor regions — long stretches where σ is tiny and the
     autocorrelation peak is strong. These are sections of the song with a
     confident, stable tempo.
  3. For each anchor, run the joint beat+downbeat scorer on that slice to get
     a precise (BPM, beat_phase, downbeat_index) — locally optimal per region.
  4. Extend each anchor outward by walking expected downbeat positions and
     keeping those that match the downbeat-prob curve above an adaptive
     threshold. Stops cleanly where the bar structure fades.
  5. Concatenate all anchors' beats/downbeats into the flat output schema.
     `bpm` is the longest anchor's tempo; `downbeat_offset` is the first
     downbeat in the song.

Single-tempo songs collapse to one anchor → indistinguishable from the old
fixed-grid output. Multi-tempo songs get multiple locally-correct grids that
together cover the song (with gaps in true breakdowns where no grid is
supported).
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


def _interp_between_snaps(downbeats, snapped_mask):
    """Distribute unsnapped (drift-following) downbeats evenly between
    bookending strong-snapped downbeats so the local BPM fits an integer bar
    count exactly. Each gap is filled at whatever uniform period fits the
    actual time between snaps — local tempo wobble gets absorbed cleanly.
    """
    if not downbeats:
        return list(downbeats)
    out = list(downbeats)
    i = 0
    while i < len(out):
        if not snapped_mask[i]:
            i += 1
            continue
        # find the next snapped index
        j = i + 1
        while j < len(out) and not snapped_mask[j]:
            j += 1
        if j >= len(out):
            break
        n_between = j - i  # number of bar intervals between the two snaps
        if n_between > 1:
            actual_period = (out[j] - out[i]) / n_between
            for k in range(1, n_between):
                out[i + k] = out[i] + k * actual_period
        i = j
    return out


def _snap_to_peaks(downbeats, db_probs, hop, max_shift_ms=40.0, threshold=0.5,
                    beat_probs=None, beat_threshold=0.6):
    """Snap each downbeat to the nearest peak within ±max_shift_ms.

    Two-stage: prefer downbeat-probability (precise bar location), but if
    db-prob is below threshold in this window, fall back to beat-probability
    (which stays strong in breakdowns where the model has lost bar structure
    but the underlying beat is still audibly there).

    Only snaps when the chosen signal clears its threshold — so completely
    silent regions leave the original grid position.

    Returns (snapped_positions, snapped_mask).
    """
    if not downbeats:
        return list(downbeats), []
    tol_frames = max(1, int(max_shift_ms / 1000.0 / hop))
    snapped = []
    mask = []
    for d in downbeats:
        idx = int(round(d / hop))
        lo = max(0, idx - tol_frames)
        hi = min(len(db_probs), idx + tol_frames + 1)
        if hi <= lo:
            snapped.append(d); mask.append(False); continue
        db_window = db_probs[lo:hi]
        k = int(np.argmax(db_window))
        if float(db_window[k]) >= threshold:
            snapped.append((lo + k) * hop); mask.append(True); continue
        # db-prob too weak — try beat-prob fallback
        if beat_probs is not None:
            b_window = beat_probs[lo:hi]
            kb = int(np.argmax(b_window))
            if float(b_window[kb]) >= beat_threshold:
                snapped.append((lo + kb) * hop); mask.append(True); continue
        snapped.append(d); mask.append(False)
    return snapped, mask


def _rederive_beats(downbeats, original_beats, bpb=4):
    """After snapping downbeats, re-derive beats as evenly-spaced positions
    within each bar. The last bar (after the final downbeat) keeps its
    original-period beats since there's no following downbeat to interpolate
    against."""
    if len(downbeats) < 2:
        return list(original_beats)
    new_beats = []
    for i in range(len(downbeats) - 1):
        start = downbeats[i]
        end = downbeats[i + 1]
        bar = end - start
        step = bar / bpb
        for k in range(bpb):
            new_beats.append(start + k * step)
    # tail: beats after the last downbeat — use prev bar's beat period
    last_db = downbeats[-1]
    prev_bar = downbeats[-1] - downbeats[-2]
    step = prev_bar / bpb
    # how far past last_db did the original beats extend?
    last_orig = max(original_beats) if original_beats else last_db
    t = last_db
    while t <= last_orig + 1e-6:
        new_beats.append(t)
        t += step
    return sorted(set(round(b, 4) for b in new_beats))


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


def multi_anchor_grid(beat_logits, downbeat_logits, hop_seconds,
                      bpm_min=70.0, bpm_max=170.0):
    """Top-level entry. Returns (bpm, downbeat_offset, beats_per_bar,
    beats_concat, downbeats_concat) for the whole song.

    Strategy: pick the longest anchor as the *primary* grid and extend it as
    far as it goes. Any region the primary can't cover (because the bar
    structure breaks down — a real tempo change) gets a separate anchor fit
    inside the gap. This avoids overlapping grids when the song is single-
    tempo, and naturally produces a multi-anchor result only when needed.
    """
    beat_probs = _sigmoid_array(beat_logits)
    db_probs = _sigmoid_array(downbeat_logits)
    duration = (len(beat_probs) - 1) * hop_seconds if len(beat_probs) else 0.0

    tg_t, tg_b, tg_c = compute_tempogram(
        beat_probs, hop_seconds,
        bpm_min=bpm_min, bpm_max=bpm_max,
    )
    anchors = _find_anchors(tg_t, tg_b, tg_c)

    if not anchors:
        grid = fixed_bpm_from_logits(
            beat_logits, downbeat_logits, hop_seconds,
            bpm_min=bpm_min, bpm_max=bpm_max,
        )
        return grid.bpm, grid.offset, grid.beats_per_bar, grid.beats, grid.downbeats

    # cluster anchors by octave-equivalent BPM; each anchor gets a target
    # BPM that all members of its cluster share
    targets = _consensus_bpms(anchors)

    # sort longest first (just for stable iteration order — no primary anchor)
    anchor_order = sorted(range(len(anchors)),
                          key=lambda i: anchors[i]["t_end"] - anchors[i]["t_start"],
                          reverse=True)

    covered_regions = []  # list of (t_start, t_end, beats, downbeats, bpm, bpb)

    def overlaps(t0, t1):
        for r in covered_regions:
            if not (t1 < r[0] or t0 > r[1]):
                return True
        return False

    # PASS 1: fit each anchor at its consensus target
    pass1_fits = {}
    for idx in anchor_order:
        a = anchors[idx]
        pass1_fits[idx] = _fit_anchor(beat_logits, downbeat_logits, hop_seconds, a,
                                       target_bpm=targets.get(idx))

    # GLOBAL doubling vote, weighted by anchor duration. Each anchor "votes"
    # for or against doubling based on whether its midpoint-downbeat pattern
    # looks like a halved fit. The longer-duration side wins — short intros
    # with a different rhythmic feel don't get to override a long main
    # section, and vice versa.
    yes_dur = 0.0
    no_dur = 0.0
    for idx in anchor_order:
        a = anchors[idx]
        dur = a["t_end"] - a["t_start"]
        fit = pass1_fits[idx]
        votes_yes = (fit["bpm"] * 2 < 200.0) and _is_octave_halved(
            fit["downbeats_abs"], db_probs, hop_seconds
        )
        if votes_yes:
            yes_dur += dur
        else:
            no_dur += dur
    needs_doubling = yes_dur > no_dur

    # PASS 2: assemble covered regions, refitting at 2× if vote passed
    for idx in anchor_order:
        a = anchors[idx]
        if overlaps(a["t_start"], a["t_end"]):
            continue
        fit = pass1_fits[idx]
        if needs_doubling and fit["bpm"] * 2 < 200.0:
            fit = _fit_anchor(beat_logits, downbeat_logits, hop_seconds, a,
                              target_bpm=fit["bpm"] * 2)
        beats, downbeats = _extend_anchor(fit, db_probs, hop_seconds, duration)
        if not beats:
            continue
        # clip extension so we don't overlap regions covered by previously-
        # accepted (longer) anchors
        eff_start = beats[0]
        eff_end = beats[-1]
        for r in covered_regions:
            if eff_start < r[0] <= eff_end:
                # we extended forward into a later anchor's region — clip
                eff_end = r[0] - 1e-3
            if eff_start <= r[1] < eff_end:
                # extended backwards into earlier anchor's region — clip
                eff_start = r[1] + 1e-3
        beats = [b for b in beats if eff_start <= b <= eff_end]
        downbeats = [d for d in downbeats if eff_start <= d <= eff_end]
        if not beats:
            continue
        covered_regions.append((beats[0], beats[-1], beats, downbeats,
                                fit["bpm"], fit["beats_per_bar"]))

    # sort regions left-to-right
    covered_regions.sort(key=lambda r: r[0])

    # ---- gap filling ----
    # For each gap between confident regions, choose an integer bar count that
    # phase-aligns both endpoints. For pre/post-song gaps, just extend the
    # adjacent anchor's grid.
    filled_segments = []  # (beats, downbeats)
    # Track which regions had their trailing partial bar absorbed by a
    # gap-fill — those regions' beats must be clipped to their last downbeat
    # so the fill owns that territory exclusively (otherwise the trailing
    # beats from one grid overlap with the fill grid's beats at a different
    # period, producing visual "6 beats in a bar" artifacts).
    region_filled_after = [False] * len(covered_regions)

    # before first region: extend backwards from first region's grid
    if covered_regions:
        first = covered_regions[0]
        if first[0] > 0.0:
            period = 60.0 / first[4]
            t = first[2][0] - period
            beats_pre = []
            while t >= 0.0:
                beats_pre.append(t)
                t -= period
            beats_pre.reverse()
            # downbeats: keep bar phase with first region
            bar = period * first[5]
            db_pre = []
            t = first[3][0] - bar if first[3] else first[2][0] - bar
            while t >= 0.0:
                db_pre.append(t)
                t -= bar
            db_pre.reverse()
            if beats_pre:
                filled_segments.append((beats_pre, db_pre))

    # interior gaps
    for i in range(len(covered_regions) - 1):
        a = covered_regions[i]
        b = covered_regions[i + 1]
        # last downbeat of A, first downbeat of B
        if not a[3] or not b[3]:
            continue
        t_a = a[3][-1]
        t_b = b[3][0]
        gap = t_b - t_a
        if gap <= 0:
            continue
        bpb = a[5]
        # if the gap is shorter than a bar at either side's tempo, the two
        # anchors are essentially adjacent — don't invent a weird-BPM bridge,
        # just let them meet at their natural downbeat positions.
        prev_bar = (60.0 / a[4]) * a[5]
        next_bar = (60.0 / b[4]) * b[5]
        if gap < min(prev_bar, next_bar):
            continue

        # prior from tempogram in the gap region
        mask = (tg_t >= a[1]) & (tg_t <= b[0])
        if mask.any():
            prior_bpm = float(np.median(tg_b[mask]))
        else:
            prior_bpm = 0.5 * (a[4] + b[4])
        # ensure positive plausible prior
        prior_bpm = max(60.0, min(200.0, prior_bpm))
        prior_bar = (60.0 / prior_bpm) * bpb

        # pick integer N that minimises |bar_period - prior_bar|
        N = max(1, int(round(gap / prior_bar)))
        # also try N±1 in case round goes the wrong way at the boundary
        candidates = sorted({N - 1, N, N + 1} - {0})
        best_n = min(candidates, key=lambda n: abs((gap / n) - prior_bar))
        if best_n < 1:
            continue
        actual_bar = gap / best_n
        actual_period = actual_bar / bpb
        # generate fill beats (exclusive of t_a, inclusive up to but not equal to t_b)
        fill_beats = []
        for k in range(1, best_n * bpb):
            fill_beats.append(t_a + k * actual_period)
        fill_db = []
        for k in range(1, best_n):
            fill_db.append(t_a + k * actual_bar)
        filled_segments.append((fill_beats, fill_db))
        region_filled_after[i] = True

    # after last region: extend forward
    if covered_regions:
        last = covered_regions[-1]
        if last[1] < duration:
            period = 60.0 / last[4]
            t = last[2][-1] + period
            beats_post = []
            while t <= duration:
                beats_post.append(t)
                t += period
            bar = period * last[5]
            db_post = []
            t = last[3][-1] + bar if last[3] else last[2][-1] + bar
            while t <= duration:
                db_post.append(t)
                t += bar
            if beats_post:
                filled_segments.append((beats_post, db_post))

    # concatenate everything
    all_beats = []
    all_downbeats = []
    for i, r in enumerate(covered_regions):
        beats = r[2]
        if region_filled_after[i] and r[3]:
            # gap-fill follows this region — clip trailing partial-bar beats
            # so the gap-fill grid takes over cleanly after the last downbeat
            last_db = r[3][-1]
            beats = [b for b in beats if b <= last_db + 1e-6]
        all_beats.extend(beats)
        all_downbeats.extend(r[3])
    for fb, fd in filled_segments:
        all_beats.extend(fb)
        all_downbeats.extend(fd)
    all_beats = sorted(set(round(b, 4) for b in all_beats))
    all_downbeats = sorted(set(round(d, 4) for d in all_downbeats))

    # drift correction: iteratively snap each downbeat to the nearest strong
    # peak (preferring db-prob, falling back to beat-prob when db-prob is
    # absent — handles breakdown sections where the model has lost bar
    # structure but the underlying beat is audibly there).
    last_mask = None
    for _shift_ms in (40.0, 60.0, 80.0, 100.0):
        snapped, mask = _snap_to_peaks(
            all_downbeats, db_probs, hop_seconds,
            max_shift_ms=_shift_ms, beat_probs=beat_probs,
        )
        if snapped == all_downbeats:
            break
        all_downbeats = snapped
        last_mask = mask
    # for unsnapped downbeats between two strong-snapped ones, distribute
    # them evenly — fixes drift in low-signal sections (Gimme breakdowns)
    # where individual snaps can't fire but the surrounding context does.
    if last_mask:
        all_downbeats = _interp_between_snaps(all_downbeats, last_mask)
    all_beats = _rederive_beats(all_downbeats, all_beats)

    # primary = longest region
    primary = max(covered_regions, key=lambda r: len(r[2]))
    primary_bpm = float(primary[4])
    primary_bpb = int(primary[5])
    offset = float(all_downbeats[0]) if all_downbeats else 0.0
    return primary_bpm, offset, primary_bpb, all_beats, all_downbeats


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
        tracker = Audio2Frames(checkpoint_path=str(args.checkpoint), device="cpu", float16=False)
        beat_logits, downbeat_logits = tracker(signal, sr)
        hop_seconds = 441 / 22050  # matches beat_this preprocessing
        bpm, offset, bpb, beats, downbeats = multi_anchor_grid(
            beat_logits.cpu().numpy(),
            downbeat_logits.cpu().numpy(),
            hop_seconds,
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
