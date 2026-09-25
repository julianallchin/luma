"""`luma.music`: the track as a lighting designer hears it, in text and arrays.

Every position is `bar.beat.16th`: `bar` is the UI bar (1-indexed), `beat` the
UI beat inside it (0-indexed), `16th` a sixteenth at the *felt* tempo inside
that beat. When the stored grid is half the felt tempo (Skanka: 70 grid, 140
felt) a UI beat holds eight felt 16ths, so `13.0.6` is the seventh felt 16th
of bar 13.

The analysis runs on two signals and one drum source:
- `rest`: the mix without vocals, split into four bands, so rap harmonics can
  never pass for a growl;
- `vocals`: the vocals stem, for presence only;
- n2n drum onsets (kick/snare/hat/cymbal), duplicates within 30 ms merged.

The text tools (`listen`, `deviations`, `modulation`, `similar`, `sections`)
are short functions over the same arrays exposed here (`onsets`, `mert`,
`envelopes`, `beats`), so a method can be read and extended in a cell.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from functools import cached_property
from typing import Any, Mapping

import numpy as np

from .bindings import LumaRecord, Unavailable

#: Frequency bands of `rest`, in Hz. `None` is an open edge.
BANDS = (("sub", None, 70.0), ("low", 70.0, 400.0), ("mid", 400.0, 2000.0), ("high", 2000.0, None))
DRUMS = ("kick", "snare", "hat", "cymbal")
#: Level glyphs, quiet to loud within a band's own range over the track.
GLYPHS = " .:-=+*#%@"
ENVELOPE_HOP_S = 0.005
ENVELOPE_WINDOW_S = 0.025
ANALYSIS_RATE_HZ = 16000


class Frame:
    """Equal-length named numpy columns: one row per event or grid step.

    Columns read as attributes or keys. A column may be 2-D (rows first), as
    MERT vectors are. Arrays are read-only.
    """

    def __init__(self, path: str, columns: Mapping[str, Any]) -> None:
        self._path = path
        self._columns = {}
        for name, column in columns.items():
            array = np.asarray(column)
            array.flags.writeable = False
            self._columns[name] = array

    def __getattr__(self, name: str) -> np.ndarray:
        columns = self.__dict__.get("_columns", {})
        if name in columns:
            return columns[name]
        raise AttributeError(f"{self.__dict__.get('_path')} has no column {name!r}; columns: {', '.join(columns)}")

    def __getitem__(self, name: str) -> np.ndarray:
        return self._columns[name]

    def keys(self) -> list[str]:
        return list(self._columns)

    def __len__(self) -> int:
        return len(next(iter(self._columns.values()))) if self._columns else 0

    def __repr__(self) -> str:
        shapes = [name + ("" if array.ndim == 1 else f"[{','.join(map(str, array.shape[1:]))}]")
                  for name, array in self._columns.items()]
        return f"<{self._path} {len(self)} rows: {', '.join(shapes)}>"


@dataclass(frozen=True)
class Feel:
    """How the stored grid relates to the tempo the music is felt at."""

    grid_bpm: float
    ratio: int
    snare_period: float | None  # in felt beats

    @property
    def bpm(self) -> float:
        return self.grid_bpm * self.ratio

    @property
    def drums(self) -> str:
        names = {2.0: "backbeat drums (snare every 2 felt beats)", 4.0: "halftime drums (snare every 4 felt beats)"}
        if self.snare_period is None:
            return "no steady snare"
        return names.get(self.snare_period, f"snare every {self.snare_period:g} felt beats")

    def __repr__(self) -> str:
        relation = "the grid" if self.ratio == 1 else f"{self.ratio}x the {self.grid_bpm:.0f} grid"
        sixteenths = 4 * self.ratio
        return (f"feel {self.bpm:.0f} bpm = {relation}; {self.drums}. "
                f"Positions bar.beat.16th: UI bar (from 1), UI beat (from 0), felt 16th 0-{sixteenths - 1}.")


def detect_feel(grid_bpm: float, snare_beats: np.ndarray) -> Feel:
    """Pick the felt tempo as 1x or 2x the grid.

    A felt tempo sits in 85-190 bpm and puts the snare every 2 felt beats
    (backbeat) or every 4 (halftime). Skanka's 70 grid has a snare every 2
    grid beats: 70 is too slow to feel, so it is 140 halftime. Backbone's 88
    grid has a snare on every grid beat, which only reads as a 176 backbeat.
    """
    intervals = np.diff(np.sort(snare_beats))
    intervals = intervals[(intervals > 0.4) & (intervals < 8.5)]
    period = float(np.median(np.round(intervals * 2) / 2)) if len(intervals) >= 8 else None
    for ratio in (1, 2):
        felt = None if period is None else period * ratio
        if 85 <= grid_bpm * ratio <= 190 and (felt is None or felt in (2.0, 4.0)):
            return Feel(grid_bpm, ratio, felt)
    return Feel(grid_bpm, 1, period)


class Music:
    """The track as heard: felt tempo, text views and the arrays behind them.

    Text tools, all positions `bar.beat.16th` (see `feel`):
      listen(bars)      one row per rest band, n2n drums and vocals, per felt 16th
      deviations(bars)  what each bar does differently from its loop
      modulation(bars)  wobble/sweep rate, depth, shape and direction; flags rate changes
      similar(at)       ranked places that sound or groove like a beat, bar or phrase
      sections()        MERT novelty, repeats and drops
    `bars` is a bar number, an inclusive "9-12" string, or a half-open (9, 13) tuple.
    Arrays: beats, onsets, envelopes, mert.
    """

    def __init__(self, *, beats, downbeats, beats_per_bar, bpm, onsets, rest, vocals=None,
                 sample_rate=48000.0, mert=None, mert_drum=None, mert_rate=75.0) -> None:
        self._beats = np.asarray(beats, dtype=float)
        downbeats = np.asarray(downbeats, dtype=float)
        if len(self._beats) < 2 or len(downbeats) < 1:
            raise ValueError("luma.music needs a beat grid with downbeats")
        self._first = int(np.argmin(np.abs(self._beats - downbeats[0])))
        self._per_bar = int(beats_per_bar)
        self._grid_bpm = float(bpm)
        self._raw_onsets = {name: np.sort(np.asarray(times, dtype=float)) for name, times in onsets.items()}
        self._rest, self._vocals, self._rate = rest, vocals, float(sample_rate)
        self._mert = {"fullmix": mert, "drum": mert_drum}
        self._mert_rate = float(mert_rate)

    @classmethod
    def from_bindings(cls, features, audio):
        """Build from `luma.features` and `luma.audio`, or explain what is missing."""
        try:
            onsets = {name: features.drum_onsets[name].values for name in features.drum_onsets.keys()
                      if not isinstance(features.drum_onsets[name], Unavailable)}
            rest = audio.rest
            if isinstance(rest, Unavailable):
                return Unavailable(f"luma.music needs the rest signal: {rest.reason}", "luma.music")
            mert = features.mert
            vocals = audio.vocals
            return cls(beats=features.beats.values, downbeats=features.downbeats.values,
                       beats_per_bar=features.beats_per_bar, bpm=features.bpm, onsets=onsets,
                       rest=rest, vocals=None if isinstance(vocals, Unavailable) else vocals,
                       sample_rate=rest.sample_rate_hz or 48000.0,
                       mert=None if isinstance(mert.fullmix, Unavailable) else mert.fullmix,
                       mert_drum=None if isinstance(mert.drum, Unavailable) else mert.drum)
        except (AttributeError, KeyError, ValueError, TypeError) as error:
            reason = getattr(error, "reason", None) or str(error)
            return Unavailable(f"luma.music needs beats, downbeats and drum onsets: {reason}", "luma.music")

    # -- time ---------------------------------------------------------------

    @cached_property
    def feel(self) -> Feel:
        """Felt tempo versus the stored grid, from the snare period."""
        snares = self._position(self._raw_onsets.get("snare", ()))
        return detect_feel(self._grid_bpm, snares)

    @property
    def _slots_per_beat(self) -> int:
        return 4 * self.feel.ratio

    def _position(self, times) -> np.ndarray:
        """Seconds -> UI beats since the downbeat of bar 1 (extrapolated at the edges)."""
        times = np.asarray(times, dtype=float)
        index = np.arange(len(self._beats), dtype=float)
        inside = np.interp(times, self._beats, index)
        head = (times - self._beats[0]) / (self._beats[1] - self._beats[0])
        tail = len(self._beats) - 1 + (times - self._beats[-1]) / (self._beats[-1] - self._beats[-2])
        return np.where(times < self._beats[0], head, np.where(times > self._beats[-1], tail, inside)) - self._first

    def _seconds(self, positions) -> np.ndarray:
        """UI beats since the downbeat of bar 1 -> seconds."""
        raw = np.asarray(positions, dtype=float) + self._first
        index = np.arange(len(self._beats), dtype=float)
        inside = np.interp(raw, index, self._beats)
        head = self._beats[0] + raw * (self._beats[1] - self._beats[0])
        tail = self._beats[-1] + (raw - len(self._beats) + 1) * (self._beats[-1] - self._beats[-2])
        return np.where(raw < 0, head, np.where(raw > len(self._beats) - 1, tail, inside))

    @property
    def bar_count(self) -> int:
        return int((len(self._beats) - self._first) // self._per_bar)

    def label(self, slot: int) -> str:
        """A global felt-16th slot (0 = downbeat of bar 1) as `bar.beat.16th`."""
        per_bar = self._per_bar * self._slots_per_beat
        bar, rest = divmod(int(slot), per_bar)
        beat, sixteenth = divmod(rest, self._slots_per_beat)
        return f"{bar + 1}.{beat}.{sixteenth}"

    # -- arrays -------------------------------------------------------------

    @cached_property
    def beats(self) -> Frame:
        """One row per felt beat: time_s, bar, beat (UI), sixteenth (felt, inside the UI beat)."""
        step = 1.0 / self.feel.ratio
        positions = np.arange(0.0, self.bar_count * self._per_bar, step)
        return Frame("luma.music.beats", _grid_columns(self, positions, self._seconds(positions)))

    @cached_property
    def onsets(self) -> LumaRecord:
        """n2n onsets per class (the binding has merged hits within 30 ms).

        Columns: time_s, position (UI beats since bar 1), bar, beat, sixteenth,
        slot (global felt 16th) and level_db (rest level at the hit in the
        class's band). n2n has no velocity; level_db is the loudness proxy.
        """
        bands = {"kick": ("sub", "low"), "snare": ("mid",), "hat": ("high",), "cymbal": ("high",)}
        rest = self.envelopes.rest
        record = {}
        for name, times in self._raw_onsets.items():
            positions = self._position(times)
            index = np.clip(np.searchsorted(rest.time_s, times + 0.01), 0, len(rest) - 1)
            power = sum(10 ** (rest[band][index] / 10) for band in bands.get(name, ("mid",)))
            record[name] = Frame(f"luma.music.onsets.{name}", dict(
                _grid_columns(self, positions, times), level_db=10 * np.log10(power + 1e-12)))
        return LumaRecord(record, "luma.music.onsets")

    @cached_property
    def envelopes(self) -> LumaRecord:
        """5 ms dB envelopes: `rest` per band (sub/low/mid/high) and `vocals`."""
        rest = _band_envelopes(_mono(self._rest), self._rate, BANDS)
        record = {"rest": Frame("luma.music.envelopes.rest", rest)}
        if self._vocals is not None:
            vocals = _band_envelopes(_mono(self._vocals), self._rate, (("level", 100.0, 7000.0),))
            record["vocals"] = Frame("luma.music.envelopes.vocals", vocals)
        else:
            record["vocals"] = Unavailable("no vocals stem for this track", "luma.music.envelopes.vocals")
        return LumaRecord(record, "luma.music.envelopes")

    @cached_property
    def mert(self) -> LumaRecord:
        """MERT-95M layer 7 (fullmix and drum): raw 75 Hz `frames`, and mean-pooled
        per felt beat (`beats`) and per UI bar (`bars`), each with grid columns."""
        present = {name: np.asarray(getattr(value, "values", value), dtype=np.float32)
                   for name, value in self._mert.items() if value is not None}
        if not present:
            return Unavailable("no MERT embeddings for this track", "luma.music.mert")
        frame_times = np.arange(max(len(v) for v in present.values())) / self._mert_rate

        def pooled(path, positions, step):
            bounds = np.searchsorted(frame_times, self._seconds(np.append(positions, positions[-1] + step)))
            columns = _grid_columns(self, positions, self._seconds(positions))
            for name, frames in present.items():
                columns[name] = np.stack([frames[a:max(b, a + 1)].mean(axis=0) if a < len(frames)
                                          else np.zeros(frames.shape[1], np.float32)
                                          for a, b in zip(bounds[:-1], bounds[1:])])
            return Frame(path, columns)

        beat_step = 1.0 / self.feel.ratio
        return LumaRecord({
            "frames": LumaRecord({name: self._mert[name] for name in present}, "luma.music.mert.frames"),
            "beats": pooled("luma.music.mert.beats", self.beats.position, beat_step),
            "bars": pooled("luma.music.mert.bars", np.arange(self.bar_count, dtype=float) * self._per_bar, self._per_bar),
        }, "luma.music.mert")

    # -- grids over a span of bars -----------------------------------------

    def _bars(self, bars) -> tuple[int, int]:
        """A bar number, "9-12" (inclusive) or (9, 13) (half-open) -> inclusive (first, last)."""
        if isinstance(bars, (int, np.integer)):
            first = last = int(bars)
        elif isinstance(bars, str):
            match = re.fullmatch(r"\s*(\d+)\s*(?:-\s*(\d+))?\s*", bars)
            if not match:
                raise ValueError(f"bars must look like 13 or '9-12', got {bars!r}")
            first, last = int(match[1]), int(match[2] or match[1])
        else:
            start, end = bars
            first, last = int(start), int(end) - 1
        if not 1 <= first <= last <= self.bar_count:
            raise ValueError(f"bars {first}-{last} are outside 1-{self.bar_count}")
        return first, last

    def _slot_levels(self, first: int, last: int) -> dict[str, np.ndarray]:
        """Mean dB per felt 16th for each rest band (and vocals): {name: [bars, slots]}."""
        per_bar = self._per_bar * self._slots_per_beat
        edges = self._seconds(np.arange((first - 1) * per_bar, last * per_bar + 1) / self._slots_per_beat)
        out = {}
        sources = [self.envelopes.rest]
        if not isinstance(self.envelopes.vocals, Unavailable):
            sources.append(self.envelopes.vocals)
        for frame in sources:
            bounds = np.searchsorted(frame.time_s, edges)
            for name in frame.keys():
                if name == "time_s":
                    continue
                power = 10 ** (frame[name] / 10)
                cumulative = np.concatenate([[0.0], np.cumsum(power)])
                a, b = bounds[:-1], np.maximum(bounds[1:], bounds[:-1] + 1)
                b = np.minimum(b, len(power))
                mean = (cumulative[b] - cumulative[np.minimum(a, len(power) - 1)]) / np.maximum(b - a, 1)
                key = "vocals" if frame is not sources[0] else name
                out[key] = (10 * np.log10(mean + 1e-12)).reshape(last - first + 1, per_bar)
        return out

    def _hits(self, first: int, last: int) -> dict[str, np.ndarray]:
        """Drum presence per felt 16th: {class: bool[bars, slots]}."""
        per_bar = self._per_bar * self._slots_per_beat
        out = {}
        for name, frame in self.onsets.items():
            grid = np.zeros((last - first + 1) * per_bar, bool)
            slots = frame.slot - (first - 1) * per_bar
            grid[slots[(slots >= 0) & (slots < len(grid))]] = True
            out[name] = grid.reshape(last - first + 1, per_bar)
        return out

    @cached_property
    def _levels(self) -> dict[str, np.ndarray]:
        """`_slot_levels` over the whole track, computed once."""
        return self._slot_levels(1, self.bar_count)

    @cached_property
    def _band_scale(self) -> dict[str, tuple[float, float]]:
        """Each band's range for glyphs: its loudest level down to 24 dB below (or its floor)."""
        scale = {}
        for name, values in self._levels.items():
            loud = float(np.percentile(values, 99.5))
            scale[name] = (max(float(np.percentile(values, 5)), loud - 24.0), loud)
        return scale

    # -- text tools ---------------------------------------------------------

    def listen(self, bars) -> str:
        """A felt-16th grid of `bars`: rest bands as level glyphs, n2n hits as x, vocals.

        Two bars per block, so a 2-bar cycle reads side by side. Levels are
        relative to each band's own range over the track (' ' quiet .. '@' loud).
        """
        first, last = self._bars(bars)
        levels, hits = self._slot_levels(first, last), self._hits(first, last)
        rows = [name for name, _, _ in BANDS] + [d for d in DRUMS if d in hits and hits[d].any()]
        rows += ["vocals"] if "vocals" in levels else []
        spb = self._slots_per_beat
        lines = [repr(self.feel), f"levels '{GLYPHS}' = quiet..loud per band over the track; x = n2n hit"]
        for pair in range(first, last + 1, 2):
            shown = [b for b in (pair, pair + 1) if b <= last]
            lines.append("")
            lines.append("       " + "  ".join(
                "|" + "|".join(f"{b}.{beat}".ljust(spb) for beat in range(self._per_bar)) for b in shown))
            for row in rows:
                cells = []
                for b in shown:
                    i = b - first
                    if row in hits:
                        text = "".join("x" if h else "." for h in hits[row][i])
                    else:
                        lo, hi = self._band_scale[row]
                        steps = np.clip((levels[row][i] - lo) / max(hi - lo, 1e-6) * len(GLYPHS), 0, len(GLYPHS) - 1)
                        text = "".join(GLYPHS[int(s)] for s in steps)
                    cells.append("|" + "|".join(text[k:k + spb] for k in range(0, len(text), spb)))
                lines.append(f"{row:<7}" + "  ".join(cells))
        return "\n".join(lines)

    def deviations(self, bars, *, db: float = 6.0) -> str:
        """What each bar does differently from its loop.

        A bar's template is the median of the other same-parity bars in the
        span (odd with odd, even with even), so a 2-bar cycle is the norm and
        only real exceptions show. A kick, snare or cymbal is missing where it
        lands in at least 75% of those bars, extra where in at most 10%; n2n
        over-triggers hats, so hats only report missing. A band deviates where a
        run of felt 16ths is `db` louder or quieter than its template. The
        first line is the 2-bar cycle itself: odd bars against even bars.
        """
        first, last = self._bars(bars)
        if last - first < 3:
            raise ValueError("deviations needs at least 4 bars to learn the loop")
        levels, hits = self._slot_levels(first, last), self._hits(first, last)
        per_bar = self._per_bar * self._slots_per_beat
        bands = [name for name, _, _ in BANDS] + (["vocals"] if "vocals" in levels else [])
        odd = [b - first for b in range(first, last + 1) if b % 2]
        even = [b - first for b in range(first, last + 1) if not b % 2]
        cycle = [f"{self._span_text(0, s, e)} {name} {value:+.0f} dB"
                 for name, _, _ in BANDS
                 for s, e, value in _level_runs(np.median(levels[name][odd], 0) - np.median(levels[name][even], 0), db / 2)]
        lines = [repr(self.feel), f"template = median of the other same-parity bars in {first}-{last}",
                 "2-bar cycle, odd bars vs even: " + (" | ".join(cycle) or "same")]
        for b in range(first, last + 1):
            i = b - first
            peers = [j for j in (odd if b % 2 else even) if j != i]
            if len(peers) < 2:
                continue
            notes: dict[int, list[str]] = {}
            for name, grid in hits.items():
                share = grid[peers].mean(axis=0)
                for slot in np.flatnonzero((share >= 0.75) & ~grid[i]):
                    notes.setdefault(slot, []).append(f"{name} missing ({share[slot]:.0%})")
                if name != "hat":
                    for slot in np.flatnonzero((share <= 0.1) & grid[i]):
                        notes.setdefault(slot, []).append(f"{name} extra ({share[slot]:.0%})")
            runs = {}
            for name in bands:
                for s, e, value in _level_runs(levels[name][i] - np.median(levels[name][peers], 0), db):
                    runs.setdefault((s, e), []).append(f"{name} {value:+.0f}")
            events = [(slot, f"{self._span_text(b, slot, slot)} " + ", ".join(text)) for slot, text in notes.items()]
            events += [(s, f"{self._span_text(b, s, e)} " + " ".join(text) + " dB") for (s, e), text in runs.items()]
            if events:
                lines.append(f"{b}: " + " | ".join(text for _, text in sorted(events)))
        return "\n".join(lines)

    def _span_text(self, bar: int, start: int, end: int) -> str:
        """Felt-16th slots of a bar as `bar.beat.16th` or a range; `bar=0` leaves the bar out."""
        spb = self._slots_per_beat
        a, b = f"{start // spb}.{start % spb}", f"{end // spb}.{end % spb}"
        text = a if start == end else f"{a}-{b}"
        return f"{bar}.{text}" if bar else text

    def modulation(self, bars, *, db: float = 2.5) -> str:
        """Wobble and sweep rate per felt bar in the low, mid and high bands of `rest`.

        Lists sweep peaks rather than averaging: in each felt bar (16 felt
        16ths) it finds the band envelope's peaks at least `db` above their
        surroundings, drops the ones a drum hit explains (a kick rings 90 ms in
        the low band, other hits 30-50 ms), and takes the typical spacing as the rate. Faster than 8 Hz is heard as
        tone (grit), not rhythm; then the audible motion is the kick-gap sweep.
        Shape compares rise with fall (swell = slow rise, pluck = fast rise);
        direction says which band peaks first. `!` marks a rate change against
        the previous felt bar, which is what an ear notices first. The sub band
        is left out: it rides the low band's motion, and a 25 ms envelope of a
        40 Hz tone cannot place a fast wobble.
        """
        first, last = self._bars(bars)
        rest = self.envelopes.rest
        spb = self._slots_per_beat
        per_bar = self._per_bar * spb
        position = self._position(rest.time_s) * spb  # global felt 16ths
        # How long each drum's hit dominates a band: a kick's body rings on in the
        # bass, hats only touch the top. Peaks inside these windows are drums.
        ring = {"kick": {"low": 0.09, "mid": 0.05, "high": 0.05},
                "snare": {"low": 0.05, "mid": 0.05, "high": 0.05},
                "hat": {"mid": 0.03, "high": 0.03}}
        bands = ("low", "mid", "high")
        drums = {name: [(self.onsets[d].time_s, windows[name]) for d, windows in ring.items()
                        if d in self.onsets and name in windows] for name in bands}
        kicks = self.onsets["kick"].slot if "kick" in self.onsets else np.array([], int)
        lines = [repr(self.feel),
                 "per felt bar: band rate depth shape @ peak 16ths (0-15 in the felt bar); ! = rate change"]
        previous: dict[str, str] = {}
        for segment in range((first - 1) * per_bar, last * per_bar, 16):
            window = (position >= segment - 2) & (position < segment + 18)
            kick_count = int(np.sum((kicks >= segment) & (kicks < segment + 16)))
            found = {name: _swells(rest[name][window], position[window] - segment, rest.time_s[window],
                                   drums[name], db) for name in bands}
            label = self.label(segment).rsplit(".", 1)[0]
            cells, flags = [], []
            for name in bands:
                peaks, depth, shape = found[name]
                period = _period(peaks)
                if period is not None and self.feel.bpm / 60 * 4 / period <= 8:
                    rhythm = f"{_note(period)} wobble"
                    cells.append((name, f"{rhythm} {depth:.0f}dB {shape} @ {' '.join(f'{p:.1f}' for p in peaks)}"))
                else:
                    rhythm = "kick-gap sweeps" if kick_count >= 2 else "held"
                    cells.append((name, rhythm + (f" + {_note(period)} grit" if period is not None else "")))
                if previous.get(name, rhythm) != rhythm:
                    flags.append(f"{name} {rhythm} (was {previous[name]})")
                previous[name] = rhythm
            direction = _direction({name: found[name][0] for name in found})
            groups: dict[str, list[str]] = {}
            for name, cell in cells:
                groups.setdefault(cell, []).append(name)
            text = " | ".join(f"{','.join(names)} {cell}" for cell, names in groups.items())
            lines.append(f"{label:<6} {text}" + (f" | {direction}" if direction else ""))
            if flags:
                lines.append("       ! " + "; ".join(flags))
        return "\n".join(lines)

    def similar(self, at, *, top: int = 10, mode: str = "both") -> str:
        """Rank the places that sound and groove like `at`.

        `at` is a beat "13.2", a beat range "13.0-13.3", a bar "13" or bars
        "9-16". Candidates keep the query's place in the bar, so they step a
        bar at a time; a one-beat query compares the same beat of every bar.
        Rows come from `beat_features`, which subtracts the
        typical row at each place in the bar, so a match shares what is
        *special* about the query (Skanka 13's missing kick finds 21 and 53,
        not the bars that are merely the same loop). "sound" is the mean cosine
        of bands and MERT fullmix, "rhythm" of hits and MERT drum. `mode`:
        "both" ranks by their mean; "rhythm" finds the same rhythm with a
        different sound; "sound" the same sound with a different rhythm.
        """
        if mode not in ("both", "rhythm", "sound"):
            raise ValueError("mode is 'both', 'rhythm' or 'sound'")
        start, length = self._span(at)
        rows = self.beat_features
        families = [name for name in ("bands", "mert_fullmix", "hits", "mert_drum") if name in rows.keys()]
        per_bar = self._per_bar * self.feel.ratio
        matches = []
        for candidate in range(start % per_bar, len(rows) - length + 1, per_bar):
            if abs(candidate - start) < length:
                continue
            cosine = {name: _cosine(rows[name][start:start + length], rows[name][candidate:candidate + length])
                      for name in families}
            sound = np.mean([cosine[k] for k in ("bands", "mert_fullmix") if k in cosine])
            rhythm = np.mean([cosine[k] for k in ("hits", "mert_drum") if k in cosine])
            matches.append((candidate, sound, rhythm))
        if mode == "both":
            ranked = sorted(matches, key=lambda m: -(m[1] + m[2]))
        else:
            same, other = (2, 1) if mode == "rhythm" else (1, 2)
            floor = 0.5 * max(m[same] for m in matches)
            ranked = sorted((m for m in matches if m[same] >= floor), key=lambda m: -(m[same] - m[other]))
        lines = [f"similar to {self._span_label(start, length)} ({mode}); features: {', '.join(families)}",
                 "span  sound  rhythm  what differs from the query"]
        for candidate, sound, rhythm in ranked[:top]:
            lines.append(f"{self._span_label(candidate, length)}  {sound:.2f}  {rhythm:.2f}  "
                         f"{self._differences(start, candidate, length)}")
        return "\n".join(lines)

    def sections(self, *, threshold: float | None = None) -> str:
        """Form from bar-pooled MERT: boundaries, repeats and drops.

        Boundaries are peaks of novelty (1 - cosine between the two bars before
        and the two after). Repeats are diagonal runs of at least 4 bars whose
        centred MERT cosine stays above `threshold` (default: the track's top
        10% of bar pairs, at least 0.5); because runs are found
        per offset, a 2-bar insert splits a repeat instead of hiding it. A drop
        is a boundary where rest sub+low jumps at least 6 dB over the four bars before.
        """
        mert = self.mert
        if isinstance(mert, Unavailable):
            return f"sections needs MERT: {mert.reason}"
        vectors = np.concatenate([_centred(mert.bars[name]) for name in ("fullmix", "drum") if name in mert.bars.keys()], axis=1)
        vectors /= np.linalg.norm(vectors, axis=1, keepdims=True) + 1e-9
        similarity = vectors @ vectors.T
        n = len(vectors)
        novelty = np.zeros(n)
        for b in range(2, n - 1):
            before, after = vectors[b - 2:b].mean(0), vectors[b:b + 2].mean(0)
            novelty[b] = 1 - _cosine(before, after)
        cut = np.median(novelty[2:n - 1]) + np.std(novelty[2:n - 1])
        boundaries = [b for b in range(2, n - 1)
                      if novelty[b] >= cut and novelty[b] == novelty[max(0, b - 2):b + 3].max()]
        levels = self._levels
        weight = np.log10(10 ** (levels["sub"] / 10) + 10 ** (levels["low"] / 10)).mean(axis=1) * 10
        drops = [b for b in boundaries if weight[b] - weight[max(0, b - 4):b].mean() >= 6]
        lines = [f"{n} bars; boundaries (novelty) at bars " + ", ".join(
            f"{b + 1}{' DROP' if b in drops else ''} ({novelty[b]:.2f})" for b in boundaries)]
        pairs = similarity[np.triu_indices(n, 2)]
        threshold = threshold if threshold is not None else max(0.5, float(np.percentile(pairs, 90)))
        repeats = []
        for lag in range(2, n):
            diagonal = np.diagonal(similarity, lag)
            for run_start, run_end in _true_runs(diagonal >= threshold):
                if run_end - run_start >= 4:
                    repeats.append((run_end - run_start, run_start, lag, diagonal[run_start:run_end].mean()))
        repeats.sort(key=lambda r: -r[0])
        kept = []
        for length, s, lag, value in repeats:
            if not any(s >= k[1] and s + length <= k[1] + k[0] and lag == k[2] for k in kept):
                kept.append((length, s, lag, value))
        lines.append(f"repeats (later ≈ earlier, mean cosine; runs above {threshold:.2f}):")
        lines += [f"  {s + lag + 1}-{s + lag + length} ≈ {s + 1}-{s + length} ({value:.2f})"
                  for length, s, lag, value in sorted(kept[:12], key=lambda k: (k[1] + k[2], k[1]))]
        return "\n".join(lines)

    # -- similar() helpers -------------------------------------------------

    def _span(self, at) -> tuple[int, int]:
        """`at` -> (first felt beat row, number of felt beats)."""
        text = str(at).strip()
        per_bar = self._per_bar * self.feel.ratio
        point = r"(\d+)(?:\.(\d+))?"
        match = re.fullmatch(point + r"(?:\s*-\s*" + point + r")?", text)
        if not match:
            raise ValueError(f"at must look like '13', '9-16', '13.2' or '13.0-13.3', got {at!r}")
        bar, beat, end_bar, end_beat = match.groups()
        first = (int(bar) - 1) * per_bar + (int(beat) * self.feel.ratio if beat is not None else 0)
        if end_bar is None:
            end_bar, end_beat = bar, beat
        last = (int(end_bar) - 1) * per_bar + (
            (int(end_beat) + 1) * self.feel.ratio if end_beat is not None else per_bar)
        if last <= first:
            raise ValueError(f"empty span {at!r}")
        return first, last - first

    def _span_label(self, start: int, length: int) -> str:
        per_bar = self._per_bar * self.feel.ratio
        if start % per_bar == 0 and length % per_bar == 0:
            first = start // per_bar + 1
            last = first + length // per_bar - 1
            return f"{first}" if first == last else f"{first}-{last}"
        end = start + length - 1
        a = f"{start // per_bar + 1}.{(start % per_bar) // self.feel.ratio}"
        b = f"{end // per_bar + 1}.{(end % per_bar) // self.feel.ratio}"
        return a if a == b else f"{a}-{b}"

    @cached_property
    def beat_features(self) -> Frame:
        """One row per felt beat for comparing places (what `similar` uses).

        bands: rest amplitude per band and felt 16th, so loud bands count more
        than quiet ones; hits: n2n kick/snare/hat per felt 16th weighted
        1/0.7/0.2 (a missing kick is bigger news than a missing hat);
        mert_fullmix, mert_drum: beat-pooled MERT. Every family has the median
        row at the same place in the bar subtracted: a row says what is
        special there. Compare spans with a cosine over the flattened rows.
        """
        ratio, per_beat = self.feel.ratio, 4
        levels, hits = self._levels, self._hits(1, self.bar_count)
        count = self.bar_count * self._per_bar * ratio
        amplitude = np.sqrt(np.stack([10 ** (levels[name] / 10) for name, _, _ in BANDS], axis=-1))
        amplitude /= amplitude.sum(axis=-1).mean()
        weights = {"kick": 1.0, "snare": 0.7, "hat": 0.2}
        drums = np.stack([hits[d] * w for d, w in weights.items() if d in hits], axis=-1).astype(float)
        columns = {"bands": amplitude.reshape(count, per_beat * len(BANDS)),
                   "hits": drums.reshape(count, -1)}
        if not isinstance(self.mert, Unavailable):
            for name in ("fullmix", "drum"):
                if name in self.mert.beats.keys():
                    columns[f"mert_{name}"] = _centred(self.mert.beats[name])[:count]
        per_bar = self._per_bar * ratio
        for name, matrix in columns.items():
            matrix = matrix.copy()
            for place in range(per_bar):
                matrix[place::per_bar] -= np.median(matrix[place::per_bar], axis=0)
            columns[name] = matrix
        grid = {key: self.beats[key] for key in self.beats.keys() if key != "time_s"}
        return Frame("luma.music.beat_features", {**grid, **columns})

    def _differences(self, query: int, candidate: int, length: int) -> str:
        """Kicks and snares that differ, and bands over 3 dB apart, candidate vs query."""
        slots = 4  # felt 16ths per felt beat
        q0, c0, size = query * slots, candidate * slots, length * slots
        found = []
        for name in ("kick", "snare"):
            if name not in self.onsets:
                continue
            slot = self.onsets[name].slot
            ours = set((slot[(slot >= q0) & (slot < q0 + size)] - q0).tolist())
            theirs = set((slot[(slot >= c0) & (slot < c0 + size)] - c0).tolist())
            found += [f"{name}@{self.label(c0 + k)} {'present' if k in theirs else 'missing'}"
                      for k in sorted(ours ^ theirs)[:4]]
        for name, _, _ in BANDS:
            flat = self._levels[name].reshape(-1)
            delta = float(flat[c0:c0 + size].mean() - flat[q0:q0 + size].mean())
            if abs(delta) >= 3:
                found.append(f"{name} {delta:+.0f} dB")
        return ", ".join(found) or "same"

    def __repr__(self) -> str:
        return (f"<luma.music {self.feel.bpm:.0f} bpm felt (grid {self._grid_bpm:.0f}), {self.bar_count} bars: "
                "listen deviations modulation similar sections; beats onsets envelopes mert>")


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------


def _grid_columns(music: Music, positions: np.ndarray, times: np.ndarray) -> dict[str, np.ndarray]:
    spb = music._slots_per_beat
    slot = np.round(positions * spb).astype(int)
    per_bar = music._per_bar * spb
    bar, within = np.divmod(slot, per_bar)
    beat, sixteenth = np.divmod(within, spb)
    return {"time_s": times, "position": positions, "bar": bar + 1, "beat": beat,
            "sixteenth": sixteenth, "slot": slot}


def _mono(signal) -> np.ndarray:
    values = np.asarray(getattr(signal, "values", signal), dtype=np.float32)
    return values.mean(axis=1) if values.ndim == 2 else values


def _band_envelopes(samples: np.ndarray, rate: float, bands) -> dict[str, np.ndarray]:
    """Band-filter at 16 kHz and return 5 ms mean-power envelopes in dB."""
    from fractions import Fraction

    from scipy import signal

    ratio = Fraction(ANALYSIS_RATE_HZ, int(rate)).limit_denominator(1000)
    audio = signal.resample_poly(samples, ratio.numerator, ratio.denominator)
    fs = float(ANALYSIS_RATE_HZ)
    hop = int(round(ENVELOPE_HOP_S * fs))
    window = int(round(ENVELOPE_WINDOW_S * fs))
    out = {}
    for name, low, high in bands:
        if low is None:
            sos = signal.butter(6, high, "lowpass", fs=fs, output="sos")
        elif high is None:
            sos = signal.butter(6, low, "highpass", fs=fs, output="sos")
        else:
            sos = signal.butter(4, [low, high], "bandpass", fs=fs, output="sos")
        power = signal.sosfiltfilt(sos, audio) ** 2
        out[name] = 10 * np.log10(_smooth(power, window)[::hop] + 1e-12)
    length = len(next(iter(out.values())))
    return {"time_s": np.arange(length) * hop / fs, **out}


def _smooth(values: np.ndarray, frames: int) -> np.ndarray:
    from scipy.ndimage import uniform_filter1d

    return uniform_filter1d(np.asarray(values, dtype=float), max(1, int(frames)), mode="nearest")


def _level_runs(difference: np.ndarray, db: float):
    """Runs of slots at least `db` above (or below) zero -> (start, end, mean dB).

    A run bridges gaps of up to two slots, so hats cut for a beat read as one
    run even where the template had silent 16ths in it.
    """
    runs = []
    for sign in (1, -1):
        flagged = np.flatnonzero(sign * difference >= db)
        for slot in flagged:
            if runs and runs[-1][3] == sign and slot - runs[-1][1] <= 3:
                runs[-1][1] = slot
                runs[-1][2].append(slot)
            else:
                runs.append([slot, slot, [slot], sign])
    return sorted((s, e, float(np.mean(difference[members]))) for s, e, members, _ in runs)


def _true_runs(mask: np.ndarray):
    runs, start = [], None
    for i, value in enumerate(np.append(mask, False)):
        if value and start is None:
            start = i
        elif not value and start is not None:
            runs.append((start, i))
            start = None
    return runs


def _centred(matrix) -> np.ndarray:
    matrix = np.asarray(matrix, dtype=np.float32)
    matrix = matrix - matrix.mean(axis=0)
    return matrix / (np.linalg.norm(matrix, axis=1, keepdims=True) + 1e-9)


def _cosine(a: np.ndarray, b: np.ndarray) -> float:
    a, b = np.ravel(a), np.ravel(b)
    na, nb = np.linalg.norm(a), np.linalg.norm(b)
    if na < 1e-9 and nb < 1e-9:
        return 1.0
    if na < 1e-9 or nb < 1e-9:
        return 0.0
    return float(a @ b / (na * nb))


def _swells(envelope, phase, times, drums, db: float):
    """Peaks of one felt bar's envelope that no drum hit explains.

    `drums` pairs hit times with how long after a hit (s) a peak is still the drum.

    Returns (peak positions in felt 16ths, median prominence dB, shape), where
    shape compares the rise from the previous trough with the fall to the next.
    """
    from scipy.signal import find_peaks

    smooth = _smooth(envelope, 3)
    index, props = find_peaks(smooth, prominence=db, distance=max(1, int(0.04 / ENVELOPE_HOP_S)))
    keep = [k for k, i in enumerate(index)
            if 0 <= phase[i] < 16 and not any(np.any((times[i] - hits > -0.02) & (times[i] - hits < ring))
                                                for hits, ring in drums)]
    if not keep:
        return [], 0.0, ""
    index = index[keep]
    depth = float(np.median(props["prominences"][keep]))
    rises, falls = [], []
    for k, i in enumerate(index):
        left = index[k - 1] if k else max(0, i - 40)
        right = index[k + 1] if k + 1 < len(index) else min(len(smooth) - 1, i + 40)
        rises.append(i - (left + int(np.argmin(smooth[left:i + 1]))))
        falls.append(i + int(np.argmin(smooth[i:right + 1])) - i)
    ratio = np.median(rises) / max(np.median(falls), 1)
    shape = "swell" if ratio > 1.5 else "pluck" if ratio < 0.67 else "even"
    return [float(phase[i]) for i in index], depth, shape


#: Peak spacings (felt 16ths) that read as a rhythm, straight before dotted
#: before triplet: on a tie the plainer reading wins.
NOTES = {1.0: "1/16", 2.0: "1/8", 4.0: "1/4", 1.5: "dotted 1/16", 3.0: "dotted 1/8", 4 / 3: "1/8T", 8 / 3: "1/4T"}


def _period(peaks) -> float | None:
    """The note spacing most consecutive peaks share (within 20%), if at least two do."""
    spacing = np.diff(peaks)
    best, count = None, 1
    for period in NOTES:
        hits = int(np.sum(np.abs(spacing - period) <= 0.2 * period + 1e-6))
        if hits > count:
            best, count = period, hits
    return best


def _note(period: float) -> str:
    return NOTES[period]


def _direction(peaks: dict[str, list[float]]) -> str:
    """Sweep up when a lower band's peaks lead the next band's by >= 0.15 16th."""
    order = [name for name, _, _ in BANDS if len(peaks.get(name, ())) >= 3]
    for low, high in zip(order, order[1:]):
        lags = [min(peaks[high], key=lambda q: abs(q - p)) - p for p in peaks[low]]
        lags = [lag for lag in lags if abs(lag) < 1.0]
        if len(lags) >= 3 and abs(np.median(lags)) >= 0.15:
            return f"sweep {'up' if np.median(lags) > 0 else 'down'} {low}->{high}"
    return ""
