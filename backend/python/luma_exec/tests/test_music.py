"""luma.music: synthetic unit tests, plus answer keys from real tracks.

The answer-key tests read Julian's local library read-only and skip when a
track's files are missing. Run from the repo root:

    ~/.cache/com.luma.luma/python-env/bin/python3 backend/python/luma_exec/tests/test_music.py
"""
import json
import re
import sqlite3
import sys
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec.bindings import LumaRecord, Unavailable
from luma_exec.music import Frame, Music, detect_feel

LIBRARY = Path.home() / ".config" / "com.luma.luma"
SKANKA = ("8bafe654-419b-441f-89a3-7051f2d450f3",
          "738e753e07f1e1166580a462784cf545d2eb7150ccdf44f6099537d4123ab0bb")
BACKBONE = ("746d8b01-6962-4251-af4d-60acd52c08c4",
            "46d842788b38ca895facd7af3110f133e5348dc3b74a55cfbd94e57bbb5eb3a6")
#: Bars where Julian placed a chase on Backbone's womps (his score, beat 0.5 of each).
WOMP_BARS = [18, 19, 20, 22, 23, 26, 27, 28, 34, 35, 36, 38, 39, 50, 51, 52, 54, 55, 56]


def synthetic(bars=16, bpm=120.0, missing_kick_bar=None, rate=16000):
    """A four-on-the-floor loop: kick on every beat, noise bursts on the kicks."""
    beat = 60.0 / bpm
    beats = np.arange(bars * 4 + 1) * beat + 0.5
    kicks = [t for i, t in enumerate(beats[:-1]) if not (missing_kick_bar and i == (missing_kick_bar - 1) * 4 + 2)]
    snares = beats[1:-1:2]
    rng = np.random.default_rng(0)
    audio = rng.normal(0, 0.01, int((beats[-1] + 1) * rate)).astype(np.float32)
    for t in kicks:
        i = int(t * rate)
        audio[i:i + 800] += np.sin(np.arange(800) * 2 * np.pi * 60 / rate).astype(np.float32) * 0.8
    return Music(beats=beats, downbeats=beats[::4], beats_per_bar=4, bpm=bpm,
                 onsets={"kick": np.array(kicks), "snare": snares, "hat": np.array([])},
                 rest=audio, sample_rate=rate)


class MusicUnitTests(unittest.TestCase):
    def test_feel_doubles_a_grid_that_is_too_slow_or_has_a_snare_on_every_beat(self):
        halftime = detect_feel(70.0, np.arange(1, 200, 2.0))  # snare every 2 grid beats
        self.assertEqual((halftime.ratio, halftime.bpm, halftime.snare_period), (2, 140.0, 4.0))
        doubled = detect_feel(88.0, np.arange(0, 200, 1.0))  # snare on every grid beat
        self.assertEqual((doubled.ratio, doubled.bpm, doubled.snare_period), (2, 176.0, 2.0))
        plain = detect_feel(128.0, np.arange(1, 200, 2.0))
        self.assertEqual((plain.ratio, plain.snare_period), (1, 2.0))
        self.assertIn("140 bpm = 2x the 70 grid", repr(halftime))

    def test_bars_count_from_one_and_positions_read_bar_beat_sixteenth(self):
        music = synthetic()
        self.assertEqual(music.label(0), "1.0.0")
        self.assertEqual(music.label(16 * 2 + 4 + 3), "3.1.3")
        self.assertEqual(music._bars("9-12"), (9, 12))
        self.assertEqual(music._bars((9, 13)), (9, 12))
        self.assertEqual(music._bars(13), (13, 13))
        with self.assertRaises(ValueError):
            music._bars(0)
        kick = music.onsets["kick"]
        self.assertEqual((kick.bar[0], kick.beat[0], kick.sixteenth[0]), (1, 0, 0))

    def test_arrays_are_read_only_frames_with_one_line_reprs(self):
        music = synthetic()
        self.assertEqual(repr(music.onsets["kick"]).count("\n"), 0)
        self.assertIn("rows: time_s", repr(music.envelopes.rest))
        with self.assertRaises(ValueError):
            music.onsets["kick"].time_s[0] = 1.0
        frame = Frame("x", {"a": np.zeros(3), "v": np.zeros((3, 2))})
        self.assertEqual(repr(frame), "<x 3 rows: a, v[2]>")

    def test_deviations_name_the_missing_kick_by_bar_beat_sixteenth(self):
        text = synthetic(missing_kick_bar=6).deviations("1-12")
        self.assertIn("6.2.0 kick missing (100%)", text)
        self.assertEqual(len(re.findall(r"kick missing", text)), 1, text)

    def test_listen_is_one_block_per_two_bars(self):
        text = synthetic().listen("1-4")
        self.assertIn("|1.0 |1.1 |1.2 |1.3 ", text)
        self.assertIn("kick   |x...|x...|x...|x...", text)
        self.assertLessEqual(len(text.splitlines()), 20)

    def test_similar_finds_the_bar_with_the_same_missing_kick(self):
        music = synthetic(bars=16, missing_kick_bar=6)
        self.assertIn("kick@", music.similar("6", top=3))

    def test_missing_bindings_explain_themselves(self):
        features = LumaRecord({"beats": Unavailable("beat detection has not run")}, "luma.features")
        audio = LumaRecord({"rest": Unavailable("no stems")}, "luma.audio")
        self.assertIsInstance(Music.from_bindings(features, audio), Unavailable)


def merge_duplicates(times, gap=0.03):
    """What the binding does to n2n onsets: hits within 30 ms are one hit."""
    kept = []
    for t in sorted(times):
        if not kept or t - kept[-1] >= gap:
            kept.append(t)
    return kept


def library_track(track_id, track_hash):
    """Build Music from Julian's library, read-only; None when files are missing."""
    try:
        import soundfile
    except ImportError:
        return None
    tracks = LIBRARY / "tracks"
    mix = tracks / f"{track_hash}.ogg"
    vocals = tracks / "stems" / track_hash / "vocals.ogg"
    database = LIBRARY / "luma.db"
    if not (mix.exists() and vocals.exists() and database.exists()):
        return None
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        beats, downbeats, bpm, per_bar = connection.execute(
            "SELECT beats_json, downbeats_json, bpm, beats_per_bar FROM track_beats WHERE track_id = ?",
            (track_id,)).fetchone()
        onsets = json.loads(connection.execute(
            "SELECT onsets_json FROM track_drum_onsets WHERE track_id = ?", (track_id,)).fetchone()[0])
    finally:
        connection.close()
    mix_audio, rate = soundfile.read(mix, dtype="float32", always_2d=True)
    vocal_audio, _ = soundfile.read(vocals, dtype="float32", always_2d=True)
    frames = min(len(mix_audio), len(vocal_audio))
    mert = {kind: tracks / "mert" / f"{track_hash}.{kind}.npy" for kind in ("fullmix", "drum")}
    return Music(beats=json.loads(beats), downbeats=json.loads(downbeats), beats_per_bar=per_bar, bpm=bpm,
                 onsets={name: merge_duplicates(times) for name, times in onsets.items()}, rest=mix_audio[:frames] - vocal_audio[:frames], vocals=vocal_audio[:frames],
                 sample_rate=rate,
                 mert=np.load(mert["fullmix"]) if mert["fullmix"].exists() else None,
                 mert_drum=np.load(mert["drum"]) if mert["drum"].exists() else None)


def segment_lines(text):
    """modulation() output as {"10.0": "line plus its ! flags"}."""
    lines, current = {}, None
    for line in text.splitlines():
        match = re.match(r"^(\d+\.\d)\s", line)
        if match:
            current = match[1]
            lines[current] = line
        elif current and line.strip().startswith("!"):
            lines[current] += "\n" + line
    return lines


class SkankaAnswerKey(unittest.TestCase):
    """Skanka (Hamdi), checked by ear with Julian; UI bars."""

    @classmethod
    def setUpClass(cls):
        cls.music = library_track(*SKANKA)
        if cls.music is None:
            raise unittest.SkipTest("Skanka is not in this library")

    def test_felt_at_140_over_the_70_grid(self):
        self.assertEqual((round(self.music.feel.bpm), self.music.feel.ratio), (140, 2))

    def test_the_scoop_opens_every_odd_bar_in_the_low_band(self):
        cycle = self.music.deviations("9-32").splitlines()[2]
        self.assertRegex(cycle, r"\b0\.\d(-\d\.\d)? low \+", cycle)

    def test_wobble_switches_to_eighths_at_10_and_back_at_11(self):
        lines = segment_lines(self.music.modulation("9-16"))
        for bar in (10, 12, 14):
            self.assertIn("mid 1/8 wobble", lines[f"{bar}.0"])
            self.assertIn("! mid 1/8 wobble (was kick-gap sweeps)", lines[f"{bar}.0"])
        for bar in (9, 11, 13):
            self.assertNotIn("mid 1/8 wobble", lines[f"{bar}.0"])
        for bar in (11, 13):
            self.assertIn("mid kick-gap sweeps (was 1/8 wobble)", lines[f"{bar}.0"])

    def test_bar_13_drops_the_kick_at_16th_6_and_cuts_hats_for_the_first_beat(self):
        text = self.music.deviations("9-32")
        self.assertIn("13.0.6 kick missing", text)
        self.assertRegex(text, r"13\.0\.0-0\.[5-7] high -")

    def test_the_same_notch_is_in_21_and_53_but_not_29_45_61(self):
        text = self.music.deviations("9-32") + self.music.deviations("41-64")
        for bar in (21, 53):
            self.assertIn(f"{bar}.0.6 kick missing", text)
        for bar in (29, 45, 61):
            self.assertNotIn(f"{bar}.0.6 kick missing", text)

    def test_drops_at_9_and_41_and_the_second_half_repeats_the_first(self):
        text = self.music.sections()
        self.assertEqual(re.findall(r"(\d+) DROP", text), ["9", "41"])
        repeats = [tuple(map(int, m)) for m in re.findall(r"(\d+)-(\d+) ≈ (\d+)-(\d+)", text)]
        self.assertTrue(any(a <= 41 and b >= 63 and a - c == 32 for a, b, c, _ in repeats), text)

    def test_similar_to_13_is_21_and_53_before_the_hat_cut_bars(self):
        ranked = [int(line.split()[0]) for line in self.music.similar("13").splitlines()[2:]]
        self.assertEqual(set(ranked[:2]), {21, 53}, ranked)
        for bar in (29, 45, 61):
            self.assertGreater(ranked.index(bar), 1)

    def test_similar_to_the_first_drop_phrase_is_the_second(self):
        self.assertEqual(self.music.similar("9-16").splitlines()[2].split()[0], "41-48")

    def test_listen_fits_a_phrase_in_about_thirty_lines(self):
        text = self.music.listen("9-12")
        self.assertLessEqual(len(text.splitlines()), 30)
        self.assertIn("kick   |x.....x.|....x...|x.....x.|....x...", text)


class BackboneAnswerKey(unittest.TestCase):
    """Backbone (Chase & Status): womps are three 8th swells in 70-400 Hz."""

    @classmethod
    def setUpClass(cls):
        cls.music = library_track(*BACKBONE)
        if cls.music is None:
            raise unittest.SkipTest("Backbone is not in this library")

    def test_felt_at_176_over_the_88_grid(self):
        self.assertEqual((round(self.music.feel.bpm), self.music.feel.ratio), (176, 2))

    def test_modulation_places_three_eighth_swells_where_julian_put_chases(self):
        found = 0
        for bar in WOMP_BARS:
            line = segment_lines(self.music.modulation(bar))[f"{bar}.0"]
            match = re.search(r"low [^|]*@ ([\d. ]+)", line)
            peaks = [float(x) for x in match[1].split()] if match else []
            if all(any(abs(p - womp) <= 0.6 for p in peaks) for womp in (5, 7, 9)):
                found += 1
        self.assertGreaterEqual(found / len(WOMP_BARS), 0.8, found)

    def test_similar_to_a_womp_cell_returns_the_other_womp_cells(self):
        text = self.music.similar("18.0-18.1", top=8)
        ranked = [int(line.split()[0].split(".")[0]) for line in text.splitlines()[2:]]
        self.assertTrue(set(ranked) <= set(WOMP_BARS), text)


if __name__ == "__main__":
    unittest.main()
