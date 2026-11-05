"""luma.music, against synthetic loops.

`run_tests.py` runs this with the other suites next door.
"""
import json
import re
import sys
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec import clip
from luma_exec.bindings import LumaRecord, Unavailable
from luma_exec.music import Frame, Music, _bands, detect_feel


def setUpModule():
    """Install the band table from Rust, as the worker does. Other test
    modules install their own presets, so this runs before this module."""
    clip.install_presets(json.loads((Path(__file__).resolve().parents[3]
                                     / "crates/patterns/src/presets.json").read_text()))


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

    def test_bands_are_the_rust_band_table_with_open_outer_edges(self):
        bands = _bands()
        self.assertEqual([name for name, _, _ in bands], ["sub", "low", "mid", "high"])
        self.assertIsNone(bands[0][1])
        self.assertIsNone(bands[-1][2])
        for (_, _, top), (_, bottom, _) in zip(bands, bands[1:]):
            self.assertEqual(top, bottom)
        self.assertEqual(dict(clip.audio("mid").inputs), {"low_hz": bands[2][1], "high_hz": bands[2][2]})

    def test_positions_count_from_one_and_ranges_exclude_their_end(self):
        music = synthetic()
        self.assertEqual(music.label(0), "1.1.1")
        self.assertEqual(music.label(16 * 2 + 4 + 3), "3.2.4")
        self.assertEqual(music._bars("9-13"), (9, 12))
        self.assertEqual(music._bars((9, 13)), (9, 12))
        self.assertEqual(music._bars(13), (13, 13))
        for bad in (0, "9.2-13", "9-12-13"):
            with self.assertRaises(ValueError):
                music._bars(bad)
        kick = music.onsets["kick"]
        self.assertEqual((kick.at[0], kick.bar[0], kick.beat[0], kick.sixteenth[0]), ("1.1.1", 1, 1, 1))
        # A lone position spans what it names; a range excludes its end.
        self.assertEqual(music._span("13"), (48, 4))
        self.assertEqual(music._span("13.2"), (49, 1))
        self.assertEqual(music._span("13.1-13.3"), (48, 2))
        self.assertEqual(music._span_label(48, 2), "13.1.1-13.3.1")

    def test_bars_start_at_the_detected_downbeats_as_on_the_ruler(self):
        music = synthetic()
        # Bar 2 is three beats long: every later bar starts a beat early.
        downbeats = np.concatenate([music._beats[[0, 4]], music._beats[7:-1:4]])
        short = Music(beats=music._beats, downbeats=downbeats, beats_per_bar=4, bpm=120.0,
                      onsets={name: music._raw_onsets[name] for name in ("kick", "snare")}, rest=music._rest,
                      sample_rate=music._rate)
        kick = short.onsets["kick"]
        self.assertEqual(list(kick.at[4:9]), ["2.1.1", "2.2.1", "2.3.1", "3.1.1", "3.2.1"])
        self.assertEqual(short._span("3"), (8, 4))
        self.assertIn("|3.1 |3.2 |3.3 |3.4 ", short.listen("3-5"))

    def test_a_felt_sixteenth_at_double_feel_is_half_a_sixteenth(self):
        music = synthetic()
        object.__setattr__(music, "feel", detect_feel(70.0, np.arange(1, 200, 2.0)))
        self.assertEqual(music.label(1), "1.1.1.5")
        self.assertEqual(music.label(8 + 3), "1.2.2.5")

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
        self.assertIn("6.3.1 kick missing (100%)", text)
        self.assertEqual(len(re.findall(r"kick missing", text)), 1, text)

    def test_listen_is_one_block_per_two_bars(self):
        text = synthetic().listen("1-5")
        self.assertIn("|1.1 |1.2 |1.3 |1.4 ", text)
        self.assertIn("kick   |x...|x...|x...|x...", text)
        self.assertLessEqual(len(text.splitlines()), 20)

    def test_similar_finds_the_bar_with_the_same_missing_kick(self):
        music = synthetic(bars=16, missing_kick_bar=6)
        self.assertIn("kick@", music.similar("6", top=3))

    def test_missing_bindings_explain_themselves(self):
        features = LumaRecord({"beats": Unavailable("beat detection has not run")}, "luma.features")
        audio = LumaRecord({"rest": Unavailable("no stems")}, "luma.audio")
        self.assertIsInstance(Music.from_bindings(features, audio), Unavailable)


if __name__ == "__main__":
    unittest.main()
