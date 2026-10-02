"""Python snapshot/time semantics; the Rust checker owns graph validation."""
import copy
import io
import json
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec.color import from_srgb, to_display
from luma_exec.clip import color
from luma_exec.score import GraphTrack
from luma_exec.track import BarGrid, TrackError


class ScoreTests(unittest.TestCase):
    def test_new_clips_get_nonoverlapping_layers_unless_explicitly_chosen(self):
        track = self.track()
        edit = track.edit()
        graph = color()
        first = edit.add_clip(graph, name="Wash", id="first", at=("1", "2"))
        second = edit.add_clip(graph, name="Wash", id="second", at=("1.3", "2.3"))
        adjacent = edit.add_clip(graph, name="Wash", id="adjacent", at=("2.3", "3"))
        explicit = edit.add_clip(graph, name="Wash", id="explicit", at=("1", "2"), z=0)
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
        edit.add_clip(graph, name="Wash", id="around", at=("1", "3"), z=0)
        edit.add_clip(graph, name="Wash", id="before", at=("3", "4"), z=1)
        edit.add_clip(graph, name="Wash", id="after", at=("4", "5"), z=1)
        edit.add_clip(graph, name="Wash", id="other", at=("1", "5"), z=2)
        with redirect_stdout(io.StringIO()) as out:
            edit.add_clip(graph, name="Wash", id="placed", at=("1.3", "2"), z=0)
            edit.update_clip("placed", at=("3.3", "4.3"), z=1)
        # Each cut says what it took away.
        self.assertEqual(out.getvalue().splitlines(), [
            "cut: clip Wash (around) at z 0 is now 1.1.1-1.3.1 and 2.1.1-3.1.1",
            "cut: clip Wash (before) at z 1 is now 3.1.1-3.3.1",
            "cut: clip Wash (after) at z 1 is now 4.3.1-5.1.1"])
        spans = {id: (clip["start"], clip["start"] + clip["duration"], clip["z_index"])
                 for id, clip in edit.candidate["clips"].items()}
        # Split in two, trimmed at each end, and left alone in another z.
        tail = next(id for id in spans if id not in {"around", "before", "after", "other", "placed"})
        self.assertEqual(spans.pop(tail), (4, 8, 0))
        self.assertEqual(spans, {"around": (0, 2, 0), "before": (8, 10, 1), "after": (14, 16, 1),
                                 "other": (0, 16, 2), "placed": (10, 14, 1)})
        # A clip that only touches another cuts nothing.
        edit.add_clip(graph, name="Wash", id="touching", at=("5", "6"), z=1)
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
        edit.add_clip(graph, name="Wash", id="clip", at=("1", "1.2"))
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
            self.assertAlmostEqual(track.seconds_at(track.position_at(seconds)), seconds)
        # The beat origin (1.5 s) is bar 1 beat 1; 2.5 s is one and a half beats on.
        self.assertEqual(track.position_at(1.5), "1.1.1")
        self.assertEqual(track.position_at(2.5), "1.2.3")
        self.assertEqual(track.seconds_at("1.2.3"), 2.5)

    def test_positions_count_from_one_and_ranges_exclude_their_end(self):
        track = self.track()
        grid = BarGrid([0, 4], 4)
        for text in ["1.1.1", "17.2.3", "17.4.4.5", "0.3.1", "-1.1.2"]:
            self.assertEqual(grid.format(grid.parse(text)), text)
        self.assertEqual(grid.parse("17"), grid.parse("17.1.1"))
        self.assertEqual(grid.parse("17.2"), grid.parse("17.2.1"))
        for text in ["17.0", "17.5", "17.1.0", "17.1.5", "beat 3", "17-18"]:
            with self.assertRaises(TrackError):
                grid.parse(text)
        edit = track.edit()
        clip = edit.add_clip(color(), name="Wash", id="a", at=("1.2", "2"))
        self.assertEqual(clip.at, ("1.2.1", "2.1.1"))
        # bars= and at= agree, and clip.at places a clip on the same range.
        self.assertEqual(edit.add_clip(color(), name="Wash", id="b", bars=(2, 3)).at, ("2.1.1", "3.1.1"))
        self.assertEqual(edit.add_clip(color(), name="Wash", id="c", at=clip.at, z=5).at, clip.at)
        with self.assertRaises(TrackError):
            edit.add_clip(color(), name="Wash", at=("2", "2"))
        window = edit.window(at=("1", "3"))
        self.assertEqual((window.at, window.bars), (("1.1.1", "3.1.1"), (1, 3)))
        self.assertEqual({clip.id for clip in window.clips}, {"a", "b", "c"})
        self.assertEqual({clip.id for clip in edit.window(bars=(2, 3)).clips}, {"b"})

    def test_bars_start_at_the_detected_downbeats_as_on_the_ruler(self):
        # Bar 2 is three beats long: the downbeats say so, not the meter.
        grid = BarGrid([0, 4, 7, 11], 4)
        self.assertEqual([grid.start(bar) for bar in range(1, 6)], [0, 4, 7, 11, 15])
        self.assertEqual(grid.format(6.5), "2.3.3")
        self.assertEqual(grid.format(7), "3.1.1")
        self.assertEqual(grid.format(6.9999), "3.1.1")
        self.assertEqual(grid.parse("3"), 7)
        with self.assertRaises(TrackError):
            grid.parse("2.4")
        self.assertEqual(grid.bar_number(5.5), 2.5)
        self.assertEqual((grid.format(-1), grid.format(20)), ("0.4.1", "6.2.1"))
        track = self.track()
        object.__setattr__(track, "_downbeats", (1.5, 3.0))  # a two-beat bar 1
        self.assertEqual(track.position_at(3.0), "2.1.1")
        self.assertEqual(track.edit().add_clip(color(), name="Wash", bars=(2, 3)).at, ("2.1.1", "3.1.1"))

    def test_diff_is_a_short_plan_with_one_line_per_clip(self):
        track = self.track()
        setup = track.edit()
        setup.add_clip(color(), name="Wash", id="wash", at=("1", "5"), z=0, selection="front")
        setup.add_clip(color(), name="Gone", id="gone", at=("5", "6"), z=0)
        setup.add_clip(color(), name="Moved", id="moved", at=("6", "7"), z=0)
        setup.apply()
        edit = track.edit()
        self.assertEqual(edit.diff(), "0 added, 0 removed, 0 changed")
        with redirect_stdout(io.StringIO()):
            edit.add_clip(color(), name="Hit", id="hit", at=("2", "3"), z=0)
        edit.remove_clip("gone")
        edit.update_clip("moved", at=("7", "8"), z=1)
        plan = edit.diff()
        self.assertEqual(repr(plan), str(plan))
        self.assertEqual(plan.splitlines(), [
            "1 added, 1 removed, 2 changed",
            "~ Wash 1.1.1-5.1.1 z 0 front: split by Hit → 2 clips (1.1.1-2.1.1, 3.1.1-5.1.1)",
            "+ Hit 2.1.1-3.1.1 z 0 all",
            "- Gone 5.1.1-6.1.1 z 0 all",
            "~ Moved 7.1.1-8.1.1 z 1 all: at 6.1.1-7.1.1 → 7.1.1-8.1.1; z 0 → 1"])
        full = edit.diff(full=True)
        self.assertEqual(set(full), {"added", "removed", "changed"})
        self.assertIn("hit", full["added"])
        self.assertEqual(full["removed"]["gone"]["name"], "Gone")

    def test_diff_names_the_graph_input_that_changed(self):
        track = self.track()
        setup = track.edit()
        setup.add_clip(color(color=(1, 0, 0)), name="Wash", id="wash", at=("1", "2"))
        setup.apply()
        edit = track.edit()
        edit.update_clip("wash", graph=color(color=(0, 0, 1)))
        self.assertEqual(edit.diff().splitlines()[1],
                         "~ Wash 1.1.1-2.1.1 z 0 all: color1 color (1, 0, 0) → (0, 0, 1)")

    def test_edits_and_windows_do_not_mutate_the_track(self):
        track = self.track()
        edit, stale = track.edit(), track.edit()
        graph = color()
        edit.add_clip(graph, name="Wash", id="clip", at=("1", "2"))
        window = edit.window(at=("1", "2"))
        self.assertEqual(len(track.clips), 0)
        edit.apply()
        self.assertEqual(len(track.clips), 1)
        self.assertEqual(track.document["clips"]["clip"]["graph"]["nodes"]["color1"]["kind"], "color")
        with self.assertRaises(TypeError):
            track.document["clips"]["extra"] = {}
        with self.assertRaises(AttributeError):
            window.start_s = 0

    def test_a_window_past_the_track_end_is_clamped_and_says_so(self):
        track = self.track()
        object.__setattr__(track, "duration_s", 3.5)
        with redirect_stdout(io.StringIO()) as out:
            window = track.window(at=("1", "1.4"))
        self.assertEqual(window.end_s, 3.5)
        self.assertEqual(out.getvalue(), "window: clamped to the end of the track: 3.500 s\n")
        with self.assertRaises(TrackError):
            track.window(at=("1.3.3.4", "1.4"))

    def test_update_sets_selection_and_source_is_independent(self):
        track = self.track()
        edit = track.edit()
        graph = color()
        clip = edit.add_clip(graph, name="Wash", id="clip", at=("1", "2"))
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
        for got, want in zip(GraphTrack.from_srgb("#ff0000"), [0.6274, 0.0691, 0.0164]):
            self.assertAlmostEqual(got, want, places=4)
        self.assertEqual(GraphTrack.from_srgb([1, 1, 1]), GraphTrack.from_srgb("#ffffff"))
        for value in ["#fff", [2, 0, 0]]:
            with self.assertRaises(TrackError):
                GraphTrack.from_srgb(value)
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
        first = edit.add_clip(graph, name="Wash", id="first", at=("1.2", "2"), selection="front")
        edit.add_clip(graph, name="Wash", id="second", at=("1.3", "2.2"), selection="rear", blend="add", z=1)
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
