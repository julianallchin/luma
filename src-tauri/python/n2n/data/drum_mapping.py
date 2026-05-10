"""GM MIDI → reduced drum-class mapping used for N2N targets.

4-class reduction (v7 — cy+rd merge):
    0: Kick
    1: Snare (incl. side stick)
    2: Hat (closed + open + pedal hat — all merged)
    3: Cymbal (crash + ride — merged per ADTOF convention)

Toms (41/43/45/47/48/50) are still dropped — they're rare, weakly-labeled in
crowdsourced data, and not lighting-critical.

v6 → v7 change (2026-05-08): ride pitches (51/53/59) used to map to None
(dropped); now they merge into class 3 with crash. This was decided after
run006 showed crash-class F1 of 0.48 dragging macro F1 11pp below the other
three classes — the synthetic dataset was crash-starved (~1 hit/clip vs 17
for hat) and ride hits were a wasted source of cymbal training signal. ADTOF
merges cy+rd for the same reason.
"""

from __future__ import annotations

DRUM_MAP_4: dict[int, int] = {
    # Kick
    35: 0, 36: 0,
    # Snare
    37: 1, 38: 1, 40: 1,
    # Hat (closed 42/44, open 46 — all merged into 2)
    42: 2, 44: 2, 46: 2,
    # Cymbal: crash (49/52/55/57) + ride (51/53/59) merged
    49: 3, 52: 3, 55: 3, 57: 3,
    51: 3, 53: 3, 59: 3,
    # Toms (41/43/45/47/48/50) still absent.
}

NUM_DRUM_CLASSES = 4

CLASS_NAMES: list[str] = ["kick", "snare", "hat", "cymbal"]


def midi_to_class(note: int) -> int | None:
    """Return reduced 4-class index, or None if note has no drum mapping
    (e.g. toms 41/43/45/47/48/50 — intentionally dropped). Ride pitches
    (51/53/59) merge into class 3 alongside crash."""
    return DRUM_MAP_4.get(note)
