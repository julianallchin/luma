"""Python snapshot/time semantics; real graph validation is covered by Rust cells."""
import copy
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec.score import GraphTrack, _typed
from luma_exec.track import TrackError


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

    def test_manifest_refresh_keeps_live_track_and_revokes_departed_scope(self):
        from luma_exec.bindings import build_namespace, reconcile_facades
        from luma_exec.track import TrackClosedError
        import tempfile
        track = self.track()
        workspace = Path(tempfile.mkdtemp(prefix="luma-refresh-"))
        def manifest(score, revision="manifest-1"):
            return {"schema_version": 1, "revision": revision, "agent_kind": "track_copilot",
                    "scope": {"track_id": "track", "venue_id": "venue", "score_id": score},
                    "root": {"venue": {"id": "venue", "views": ["front"], "fixtures": []},
                             "track": copy.deepcopy(track._values), "nodes": track._nodes,
                             "features": {"beats": [1., 1.5, 2., 3., 4.], "downbeats": [1.5, 5.]}}}
        def load(score, revision="manifest-1"):
            return build_namespace(manifest(score, revision), workspace, host_call=track._host_call)
        cached_first = load("score-a")
        first = reconcile_facades(cached_first, None)
        held = first.track
        edit, stale = held.edit(), held.edit()
        graph = edit.graph(node="chase", id="effect")
        edit.add_clip(graph, id="clip", beats=(0, 1))
        second = reconcile_facades(load("score-a", "manifest-2"), first)
        self.assertIs(second.track, held)
        edit.apply()
        self.assertEqual(len(second.track.edit().clips), 1)
        self.assertEqual(second.track.revision, "next")
        self.assertEqual(stale.base_revision, "base")
        pending = held.edit()
        other = reconcile_facades(load("score-b"), second)
        self.assertIsNot(other.track, held)
        self.assertIs(other.venue, second.venue)
        self.assertTrue(other.venue._active)
        calls = len(self.calls)
        with self.assertRaises(TrackClosedError):
            pending._preview()
        with self.assertRaises(TrackClosedError):
            pending.apply()
        self.assertEqual(len(self.calls), calls)
        # Reinstalling the cached first scope must not reactivate old edits.
        returned = reconcile_facades(cached_first, other)
        self.assertIsNot(returned.track, held)
        with self.assertRaises(TrackClosedError):
            pending.apply()
        with self.assertRaises(TrackClosedError):
            other.track.edit().apply()

    def test_cached_manifest_reinstall_restores_its_snapshot_without_changing_edit_base(self):
        from luma_exec.bindings import build_namespace, reconcile_facades
        import tempfile
        track = self.track()
        workspace = Path(tempfile.mkdtemp(prefix="luma-undo-"))
        def snapshot(revision, definitions):
            values = copy.deepcopy(track._values)
            values["revision"] = revision
            values["document"]["definitions"] = definitions
            return build_namespace({"schema_version": 1, "revision": revision,
                "agent_kind": "track_copilot", "scope": {"score_id": "score"},
                "root": {"track": values, "nodes": track._nodes}}, workspace,
                host_call=track._host_call)
        cached_a = snapshot("a", {})
        cached_b = snapshot("b", {"other": track._nodes["chase"]})
        live_a = reconcile_facades(cached_a, None)
        old_edit = live_a.track.edit()
        live_b = reconcile_facades(cached_b, live_a)
        self.assertIs(live_b.track, live_a.track)
        self.assertEqual(live_b.track.revision, "b")
        self.assertIn("other", live_b.track.document["definitions"])
        undone = reconcile_facades(cached_a, live_b)
        self.assertIs(undone.track, live_b.track)
        self.assertEqual(undone.track.revision, "a")
        self.assertEqual(dict(undone.track.document["definitions"]), {})
        self.assertEqual(cached_a.track.revision, "a")
        self.assertEqual(cached_b.track.revision, "b")
        self.assertEqual(old_edit.base_revision, "a")

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
        self.assertEqual(_typed("gradient", [(0, "#ff0000"), (1, "#0000ff")])["value"],
                         {"stops": [{"t": 0, "color": [1., 0., 0.]}, {"t": 1, "color": [0., 0., 1.]}]})
        with self.assertRaises(ValueError):
            _typed("beats", {"type": "proportion", "value": .5})

    def test_isolated_preview_preserves_clip_timing_and_the_complete_draft(self):
        track = self.track()
        edit = track.edit()
        graph = edit.graph(node="chase", id="effect")
        first = edit.add_clip(graph, id="first", beats=(1, 4), selection="front")
        edit.add_clip(graph, id="second", beats=(2, 5), selection="rear", blend="add", z=1)
        original = edit.candidate
        preview = edit._preview(first)
        self.assertEqual(preview["candidate"]["clips"], {"first": original["clips"]["first"]})
        self.assertEqual(preview["baseRevision"], edit.base_revision)
        preview["candidate"]["definitions"].clear()
        self.assertEqual(edit.candidate, original)
        self.assertEqual(len(track.clips), 0)
        self.assertEqual(edit._preview()["candidate"], original)
        with self.assertRaisesRegex(TrackError, "unknown preview clip"):
            edit._preview("missing")


if __name__ == "__main__":
    unittest.main()
