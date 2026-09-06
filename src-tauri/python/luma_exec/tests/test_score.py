"""Python snapshot/time semantics; real graph validation is covered by Rust cells."""
import copy
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec.score import GraphTrack, _typed


class ScoreTests(unittest.TestCase):
    def track(self):
        nodes = {"chase": {"name": "Chase", "inputs": {
            key: {"name": key, "description": "", "value_type": kind, "rate": "frame", "default": {"type": kind, "value": default}}
            for key, kind, default in [("width", "proportion", .25), ("color", "color", [1., 1., 1.])]
        }, "outputs": {"lighting": {"value_type": "lighting", "rate": "frame"}},
            "body": {"kind": "graph", "body": {"nodes": {}, "outputs": {}}}}}
        self.calls = []
        def call(method, payload):
            self.calls.append((method, copy.deepcopy(payload)))
            if method == "track.score_apply":
                return {"revision": "next", "score": payload["candidate"]}
            return {"ok": True}
        return GraphTrack({"id": "track", "title": "Test", "revision": "base", "editable": True,
                           "beat_origin_s": 1.5, "document": {"version": 2, "definitions": {}, "clips": {}}},
                          nodes=nodes, features={"beats": [1., 1.5, 2., 3., 4.], "downbeats": [1.5, 5.]}, host_call=call)

    def test_clock_roundtrip_uses_detected_tempo_and_origin(self):
        track = self.track()
        for seconds in [-2., .5, 1.5, 2.5, 4., 8.]:
            self.assertAlmostEqual(track.seconds_at(track.beat_at(seconds)), seconds)
        self.assertEqual(track.beat_at(1.5), 0)
        self.assertEqual(track.beat_at(2.5), 1.5)

    def test_edits_and_windows_pin_their_revision_and_do_not_mutate_the_track(self):
        track = self.track()
        edit, stale = track.edit(), track.edit()
        graph = edit.graph(node="chase", id="local")
        edit.add_clip(graph, id="clip", beats=(0, 4), inputs={"width": .8})
        window = edit.window(beats=(0, 4))
        self.assertEqual(len(track.clips), 0)
        self.assertEqual(edit.apply(), "next")
        self.assertEqual(stale.base_revision, "base")
        self.assertEqual(window._revision, "base")
        self.assertEqual(len(track.clips), 1)
        self.assertEqual(track.document["clips"]["clip"]["graph"], "local")
        with self.assertRaises(TypeError):
            track.document["clips"]["extra"] = {}
        with self.assertRaises(AttributeError):
            window.start_s = 0

    def test_override_reset_preserves_selection_subset_and_source_is_independent(self):
        track = self.track()
        edit = track.edit()
        graph = edit.graph(node="chase", id="local")
        clip = edit.add_clip(graph, id="clip", beats=(0, 4), subset=.5, inputs={"width": .8})
        edit.update_clip(clip, selection="bars", inputs={"width": None})
        value = edit.candidate["clips"]["clip"]
        self.assertEqual(value["selection"], {"expression": "bars", "subset": {"fraction": .5}})
        self.assertNotIn("width", value["inputs"])
        candidate = edit.candidate
        candidate["clips"].clear()
        self.assertEqual(len(edit.clips), 1)
        source = edit.source()
        edit.replace_source(source)
        self.assertEqual(json.loads(source), edit.candidate)

    def test_rich_values_keep_their_units(self):
        self.assertEqual(_typed("color", "#ff0000"), {"type": "color", "value": [1., 0., 0.]})
        self.assertEqual(_typed("envelope", [[0, 1], [1, 0]])["value"], {"points": [[0, 1], [1, 0]]})
        with self.assertRaises(ValueError):
            _typed("beats", {"type": "proportion", "value": .5})


if __name__ == "__main__":
    unittest.main()
