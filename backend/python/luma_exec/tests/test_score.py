"""Python snapshot/time semantics; the Rust checker owns graph validation."""
import copy
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec.color import from_srgb, to_display
from luma_exec.clip import color
from luma_exec.score import GraphTrack
from luma_exec.track import TrackError


class ScoreTests(unittest.TestCase):
    def test_new_clips_get_nonoverlapping_layers_unless_explicitly_chosen(self):
        track = self.track()
        edit = track.edit()
        graph = color()
        first = edit.add_clip(graph, name="Wash", id="first", beats=(0, 4))
        second = edit.add_clip(graph, name="Wash", id="second", beats=(2, 6))
        adjacent = edit.add_clip(graph, name="Wash", id="adjacent", beats=(6, 8))
        explicit = edit.add_clip(graph, name="Wash", id="explicit", beats=(0, 4), z=0)
        self.assertEqual((first.z, second.z, adjacent.z, explicit.z), (0, 1, 0, 0))
        edit.apply()
        saved = self.calls[-1][1]["candidate"]["clips"]
        self.assertEqual(saved["second"]["z_index"], 1)
        self.assertEqual(saved["explicit"]["z_index"], 0)
        # The explicit clip covers first whole in its z.
        self.assertNotIn("first", saved)

    def test_a_clip_written_at_a_z_cuts_what_it_covers_there(self):
        track = self.track()
        edit = track.edit()
        graph = color()
        edit.add_clip(graph, name="Wash", id="around", beats=(0, 8), z=0)
        edit.add_clip(graph, name="Wash", id="before", beats=(8, 12), z=1)
        edit.add_clip(graph, name="Wash", id="after", beats=(12, 16), z=1)
        edit.add_clip(graph, name="Wash", id="other", beats=(0, 16), z=2)
        edit.add_clip(graph, name="Wash", id="placed", beats=(2, 4), z=0)
        edit.update_clip("placed", beats=(10, 14), z=1)
        spans = {id: (clip["start"], clip["start"] + clip["duration"], clip["z_index"])
                 for id, clip in edit.candidate["clips"].items()}
        # Split in two, trimmed at each end, and left alone in another z.
        tail = next(id for id in spans if id not in {"around", "before", "after", "other", "placed"})
        self.assertEqual(spans.pop(tail), (4, 8, 0))
        self.assertEqual(spans, {"around": (0, 2, 0), "before": (8, 10, 1), "after": (14, 16, 1),
                                 "other": (0, 16, 2), "placed": (10, 14, 1)})
        # A clip that only touches another cuts nothing.
        edit.add_clip(graph, name="Wash", id="touching", beats=(16, 20), z=1)
        self.assertEqual(edit.candidate["clips"]["after"]["duration"], 2)

    def track(self):
        nodes = [{"kind": "color", "output": "color", "inputs": {
            "color": {"type": "color", "unit": "rgb", "default": None}}, "settings": {}}]
        self.calls = []
        def call(method, payload):
            self.calls.append((method, copy.deepcopy(payload)))
            if method == "track.score_apply":
                return payload["candidate"]
            return {"ok": True}
        return GraphTrack({"id": "track", "title": "Test", "revision": "base", "editable": True,
                           "beat_origin_s": 1.5, "document": {"clips": {}}},
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
        graph = color()
        edit.add_clip(graph, name="Wash", id="clip", beats=(0, 1))
        second = reconcile_facades(load("score-a", "manifest-2"), first)
        self.assertIs(second.track, held)
        edit.apply()
        self.assertEqual(len(second.track.edit().clips), 1)
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
        def snapshot(revision, clips):
            values = copy.deepcopy(track._values)
            values["revision"] = revision
            values["document"]["clips"] = clips
            return build_namespace({"schema_version": 1, "revision": revision,
                "agent_kind": "track_copilot", "scope": {"score_id": "score"},
                "root": {"track": values, "nodes": track._nodes}}, workspace,
                host_call=track._host_call)
        cached_a = snapshot("a", {})
        cached_b = snapshot("b", {"other": {"name": "Wash", "graph": color().json(), "start": 0, "duration": 4, "seed": 0}})
        live_a = reconcile_facades(cached_a, None)
        old_edit = live_a.track.edit()
        live_b = reconcile_facades(cached_b, live_a)
        self.assertIs(live_b.track, live_a.track)
        self.assertEqual(live_b.track.revision, "b")
        self.assertIn("other", live_b.track.document["clips"])
        undone = reconcile_facades(cached_a, live_b)
        self.assertIs(undone.track, live_b.track)
        self.assertEqual(undone.track.revision, "a")
        self.assertEqual(dict(undone.track.document["clips"]), {})
        self.assertEqual(cached_a.track.revision, "a")
        self.assertEqual(cached_b.track.revision, "b")

    def test_clock_roundtrip_uses_detected_tempo_and_origin(self):
        track = self.track()
        for seconds in [-2., .5, 1.5, 2.5, 4., 8.]:
            self.assertAlmostEqual(track.seconds_at(track.beat_at(seconds)), seconds)
        self.assertEqual(track.beat_at(1.5), 0)
        self.assertEqual(track.beat_at(2.5), 1.5)

    def test_edits_and_windows_do_not_mutate_the_track(self):
        track = self.track()
        edit, stale = track.edit(), track.edit()
        graph = color()
        edit.add_clip(graph, name="Wash", id="clip", beats=(0, 4))
        window = edit.window(beats=(0, 4))
        self.assertEqual(len(track.clips), 0)
        edit.apply()
        self.assertEqual(len(track.clips), 1)
        self.assertEqual(track.document["clips"]["clip"]["graph"]["nodes"]["color1"]["kind"], "color")
        with self.assertRaises(TypeError):
            track.document["clips"]["extra"] = {}
        with self.assertRaises(AttributeError):
            window.start_s = 0

    def test_update_sets_selection_and_source_is_independent(self):
        track = self.track()
        edit = track.edit()
        graph = color()
        clip = edit.add_clip(graph, name="Wash", id="clip", beats=(0, 4))
        edit.update_clip(clip, selection="bars")
        value = edit.candidate["clips"]["clip"]
        self.assertEqual(value["selection"], {"expression": "bars"})
        self.assertEqual(value["name"], "Wash")
        candidate = edit.candidate
        candidate["clips"].clear()
        self.assertEqual(len(edit.clips), 1)
        source = edit.source()
        edit.replace_source(source)
        self.assertEqual(json.loads(source), edit.candidate)

    def test_colors_are_linear_rec2020_and_hex_is_srgb(self):
        # sRGB red is the red column of ITU-R BT.2087's matrix.
        for got, want in zip(GraphTrack.color("#ff0000"), [0.6274, 0.0691, 0.0164]):
            self.assertAlmostEqual(got, want, places=4)
        self.assertEqual(GraphTrack.color([1, 1, 1]), GraphTrack.color("#ffffff"))
        for value in ["#fff", [2, 0, 0]]:
            with self.assertRaises(TrackError):
                GraphTrack.color(value)
        # sRGB round-trips through the plot's display; a wider color stays in range.
        for srgb in [[1., .5, 0.], [.2, .4, .6]]:
            for got, want in zip(to_display(from_srgb(srgb)), srgb):
                self.assertAlmostEqual(got, want, places=6)
        self.assertTrue(((to_display([0., 1., 0.]) >= 0) & (to_display([0., 1., 0.]) <= 1)).all())

    def test_isolated_preview_preserves_clip_timing_and_the_complete_draft(self):
        # _preview() returns a bare score document (`{"clips": ...}`), matching
        # what RenderRequest.edit deserializes on the Rust side — never wrapped
        # in `{"candidate": ...}`, which is only the score_check/score_apply
        # wire shape.
        track = self.track()
        edit = track.edit()
        graph = color()
        first = edit.add_clip(graph, name="Wash", id="first", beats=(1, 4), selection="front")
        edit.add_clip(graph, name="Wash", id="second", beats=(2, 5), selection="rear", blend="add", z=1)
        original = edit.candidate
        preview = edit._preview(first)
        self.assertEqual(set(preview), {"clips"})
        self.assertEqual(preview["clips"], {"first": original["clips"]["first"]})
        preview["clips"].clear()
        self.assertEqual(edit.candidate, original)
        self.assertEqual(len(track.clips), 0)
        self.assertEqual(edit._preview(), original)
        with self.assertRaisesRegex(TrackError, "unknown preview clip"):
            edit._preview("missing")


if __name__ == "__main__":
    unittest.main()
