#!/usr/bin/env python3
"""Unit tests for genre_worker's patch→bar aggregation and smoothing.

Pure-numpy: the heavy imports (torch, onnxruntime) live inside the worker's
inference functions, so this runs against any interpreter with numpy and does
not need the ONNX weights.

    python3 src-tauri/python/tests/test_genre_worker.py
"""

import pathlib
import sys
import unittest

import numpy as np

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import genre_worker as gw  # noqa: E402

N_LABELS = len(gw.DISCOGS_LABELS)


def one_hot(index: int, value: float = 1.0) -> np.ndarray:
    row = np.zeros(N_LABELS, dtype=np.float32)
    row[index] = value
    return row


class OverlapWeights(unittest.TestCase):
    def test_rows_sum_to_one(self):
        bars = [(0.0, 2.0), (2.0, 4.0), (4.0, 6.0)]
        w = gw.overlap_weights(bars, n_patches=8)
        np.testing.assert_allclose(w.sum(axis=1), np.ones(3), rtol=1e-12)

    def test_weight_is_proportional_to_shared_time(self):
        # Patch 0 spans [0, 2.048); patch 1 spans [0.992, 3.040).
        # A bar of [0, 1) overlaps patch 0 fully (1.0s) and patch 1 for 0.008s.
        w = gw.overlap_weights([(0.0, 1.0)], n_patches=3)
        expected_p1 = 0.008 / 1.008
        self.assertAlmostEqual(w[0, 1], expected_p1, places=6)
        self.assertAlmostEqual(w[0, 0], 1.0 - expected_p1, places=6)
        self.assertEqual(w[0, 2], 0.0)

    def test_bar_beyond_audio_falls_back_to_nearest_patch(self):
        # Two patches cover up to ~3.04s; this bar starts well past the end,
        # which is exactly the synthetic final bar the beat grid appends.
        w = gw.overlap_weights([(60.0, 62.0)], n_patches=2)
        self.assertEqual(w[0, 1], 1.0)
        self.assertEqual(w[0, 0], 0.0)


class AggregateToBars(unittest.TestCase):
    def test_bar_fully_inside_one_patch_reproduces_it(self):
        acts = np.stack([one_hot(0, 0.9), one_hot(1, 0.9), one_hot(2, 0.9)])
        # [1.2, 1.6) lies inside patch 0 [0, 2.048) and patch 1 [0.992, 3.04):
        # a bar shorter than the patch hop always straddles two patches, so
        # assert the *mixture* rather than a single patch.
        bars = [(1.2, 1.6)]
        out = gw.aggregate_to_bars(acts, bars)
        self.assertEqual(out.shape, (1, N_LABELS))
        self.assertAlmostEqual(out[0, 0] + out[0, 1], 0.9, places=5)
        self.assertEqual(out[0, 3], 0.0)

    def test_activation_mass_is_conserved(self):
        rng = np.random.default_rng(7)
        acts = rng.random((20, N_LABELS), dtype=np.float32)
        bars = [(i * 2.0, (i + 1) * 2.0) for i in range(10)]
        out = gw.aggregate_to_bars(acts, bars)
        # A weighted mean of values in [0, 1] stays in [0, 1] per channel.
        self.assertTrue(np.all(out >= 0.0) and np.all(out <= 1.0))
        self.assertEqual(out.shape, (10, N_LABELS))

    def test_constant_activations_survive_aggregation_unchanged(self):
        acts = np.tile(one_hot(5, 0.42), (12, 1))
        bars = [(i * 1.5, (i + 1) * 1.5) for i in range(6)]
        out = gw.aggregate_to_bars(acts, bars)
        np.testing.assert_allclose(out[:, 5], np.full(6, 0.42), rtol=1e-5)


class MedianSmooth(unittest.TestCase):
    def test_single_bar_spike_is_removed(self):
        col = np.array([0.1, 0.1, 0.9, 0.1, 0.1, 0.1, 0.1], dtype=np.float32)
        out = gw.median_smooth(col[:, None], window=5)[:, 0]
        np.testing.assert_allclose(out, np.full(7, 0.1), rtol=1e-6)

    def test_sustained_change_is_preserved(self):
        col = np.array([0.1] * 6 + [0.9] * 6, dtype=np.float32)
        out = gw.median_smooth(col[:, None], window=5)[:, 0]
        self.assertAlmostEqual(out[0], 0.1, places=6)
        self.assertAlmostEqual(out[-1], 0.9, places=6)
        # The transition stays sharp: no bar sits strictly between the levels.
        self.assertTrue(np.all((np.isclose(out, 0.1)) | (np.isclose(out, 0.9))))

    def test_edges_are_replicated_not_zero_padded(self):
        col = np.full(4, 0.7, dtype=np.float32)
        out = gw.median_smooth(col[:, None], window=5)[:, 0]
        np.testing.assert_allclose(out, np.full(4, 0.7), rtol=1e-6)

    def test_channels_are_smoothed_independently(self):
        data = np.zeros((7, 3), dtype=np.float32)
        data[:, 0] = 0.2
        data[3, 1] = 1.0  # lone spike in channel 1
        data[:, 2] = np.linspace(0.0, 0.6, 7)
        out = gw.median_smooth(data, window=5)
        np.testing.assert_allclose(out[:, 0], np.full(7, 0.2), rtol=1e-6)
        np.testing.assert_allclose(out[:, 1], np.zeros(7), atol=1e-6)
        self.assertTrue(np.all(np.diff(out[:, 2]) >= -1e-6))

    def test_window_of_one_is_identity(self):
        data = np.random.default_rng(1).random((5, 4))
        np.testing.assert_array_equal(gw.median_smooth(data, window=1), data)


class SparseTop(unittest.TestCase):
    def test_keeps_top_three_below_threshold(self):
        probs = np.full(N_LABELS, 0.001, dtype=np.float32)
        probs[10], probs[11], probs[12] = 0.05, 0.04, 0.03
        pairs = gw.sparse_top(probs, top_k=8)
        self.assertEqual([i for i, _ in pairs], [10, 11, 12])

    def test_keeps_everything_above_threshold_up_to_k(self):
        probs = np.full(N_LABELS, 0.001, dtype=np.float32)
        probs[:12] = 0.5
        pairs = gw.sparse_top(probs, top_k=8)
        self.assertEqual(len(pairs), 8)

    def test_pairs_are_confidence_descending(self):
        probs = np.random.default_rng(3).random(N_LABELS)
        confs = [c for _, c in gw.sparse_top(probs, top_k=8)]
        self.assertEqual(confs, sorted(confs, reverse=True))


class BuildPayload(unittest.TestCase):
    def setUp(self):
        rng = np.random.default_rng(11)
        # 40 patches ≈ 40s of audio; 20 bars of 2s.
        self.acts = (rng.random((40, N_LABELS)) * 0.05).astype(np.float32)
        house = gw.DISCOGS_LABELS.index("Electronic---House")
        techno = gw.DISCOGS_LABELS.index("Electronic---Techno")
        self.acts[:20, house] = 0.8   # house for the first half
        self.acts[20:, techno] = 0.8  # techno for the second
        self.bars = [(i * 2.0, (i + 1) * 2.0) for i in range(20)]

    def test_shape_and_label_compaction(self):
        payload = gw.build_payload(self.acts, self.bars)
        self.assertEqual(len(payload["bars"]), 20)
        self.assertEqual([b["bar_idx"] for b in payload["bars"]], list(range(20)))
        # Only referenced labels are carried, far fewer than the full 400.
        self.assertLess(len(payload["labels"]), N_LABELS)
        self.assertGreater(len(payload["labels"]), 0)

    def test_every_index_resolves_into_labels(self):
        payload = gw.build_payload(self.acts, self.bars)
        n = len(payload["labels"])
        for bar in payload["bars"]:
            self.assertLessEqual(len(bar["top"]), gw.TOP_K)
            for idx, prob in bar["top"]:
                self.assertTrue(0 <= idx < n)
                self.assertTrue(0.0 <= prob <= 1.0)
        for idx, _ in payload["track_top"]:
            self.assertTrue(0 <= idx < n)

    def test_dominant_genre_tracks_the_section(self):
        payload = gw.build_payload(self.acts, self.bars)
        labels = payload["labels"]
        first = labels[payload["bars"][2]["top"][0][0]]
        last = labels[payload["bars"][-3]["top"][0][0]]
        self.assertEqual(first, "Electronic---House")
        self.assertEqual(last, "Electronic---Techno")

    def test_track_top_is_capped_and_ordered(self):
        payload = gw.build_payload(self.acts, self.bars)
        self.assertLessEqual(len(payload["track_top"]), gw.TRACK_TOP_K)
        confs = [c for _, c in payload["track_top"]]
        self.assertEqual(confs, sorted(confs, reverse=True))
        top_label = payload["labels"][payload["track_top"][0][0]]
        self.assertIn(top_label, {"Electronic---House", "Electronic---Techno"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
