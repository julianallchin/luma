"""Python snapshot/time semantics; real graph validation is covered by Rust cells."""
import copy
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec.color import from_srgb, to_display
from luma_exec.score import GraphTrack, _typed
from luma_exec.track import TrackError


class ScoreTests(unittest.TestCase):
    def test_gradient_literals_keep_opacity_and_leave_schema_validation_to_the_core(self):
        stops = {"stops": [{"t": 0, "color": "#ff0000", "alpha": .2},
                            {"t": 1, "color": [0., 0., 1.], "alpha": 0.}]}
        original = copy.deepcopy(stops)
        typed = _typed("gradient", stops)["value"]
        self.assertEqual([stop["alpha"] for stop in typed["stops"]], [.2, 0.])
        self.assertEqual(typed["stops"][0]["color"], from_srgb([1., 0., 0.]))
        self.assertEqual(stops, original)
        # Unknown fields must reach Rust's deny_unknown_fields checks, not vanish.
        stops["stops"][0]["opacity"] = .5
        self.assertIn("opacity", _typed("gradient", stops)["value"]["stops"][0])
        self.assertEqual(_typed("gradient", {"stops": []}), {"type": "gradient", "value": {"stops": []}})

    def test_seed_controls_keep_every_bit_in_json(self):
        for seed in [0, (1 << 53) + 1, (1 << 64) - 2, (1 << 64) - 1]:
            expected = {"type": "seed", "value": str(seed)}
            for value in [seed, str(seed), {"type": "seed", "value": seed}]:
                self.assertEqual(json.loads(json.dumps(_typed("seed", value))), expected)
        for value in [-1, 1 << 64, 1.5, True, "1.5", "NaN", "", None]:
            with self.assertRaises(TrackError):
                _typed("seed", value)

    def test_signal_sockets_accept_numbers_colors_and_unit_literals(self):
        signal = {"signal": {"unit": None, "channels": None}}
        self.assertEqual(_typed(signal, .25), {"type": "number", "value": .25})
        self.assertEqual(_typed(signal, "#ff0000"), {"type": "color", "value": from_srgb([1., 0., 0.])})
        angle = {"signal": {"unit": "degrees", "channels": "value"}}
        self.assertEqual(_typed(angle, 90), {"type": "degrees", "value": 90})
        seconds = {"signal": {"unit": "seconds", "channels": "value"}}
        self.assertEqual(_typed(seconds, .25), {"type": "seconds", "value": .25})
        self.assertEqual(_typed(signal, {"type": "seconds", "value": .25}),
                         {"type": "seconds", "value": .25})
        with self.assertRaises(TrackError):
            _typed(signal, {"type": "boundary", "value": "wrap"})

    def test_signal_sockets_accept_source_events(self):
        signal = {"signal": {"unit": None, "channels": None}}
        curve = {"points": [[0, 0, "ease-in"], [0.5, 1, "hold"], [1, 0]]}
        self.assertEqual(_typed(signal, {"type": "time", "value": curve}),
                         {"type": "time", "value": curve})
        repeat = {"type": "time", "value": {**curve, "events": {"every": {"type": "beats", "value": 2}}}}
        random = {"type": "random", "value": {"coverage": {"type": "proportion", "value": .5}}}
        for source in [repeat, random]:
            self.assertEqual(_typed(signal, source), source)
        with self.assertRaises(TrackError):
            _typed(signal, {"type": "hit", "value": curve})

    def test_signal_sockets_reject_an_envelope_tag_with_a_clear_message(self):
        # The core's Envelope value type has no signal_type(), so it can never
        # satisfy a signal socket; a curve there must be tagged "time".
        signal = {"signal": {"unit": None, "channels": None}}
        with self.assertRaisesRegex(TrackError, "not 'envelope'|time.*random"):
            _typed(signal, {"type": "envelope", "value": {"points": [[0, 0], [1, 1]]}})

    def test_a_bare_curve_dict_on_a_signal_socket_fails_locally_not_in_rust(self):
        # Regression: an untagged dict used to fall through to a numeric
        # wrapper and only fail later in Rust with "invalid type: map,
        # expected f64". It must now raise a clear TrackError in Python.
        signal = {"signal": {"unit": None, "channels": None}}
        with self.assertRaises(TrackError):
            _typed(signal, {"points": [[0, 1], [1, 0]]})

    def test_mapping_choices_and_structured_mirror_values(self):
        self.assertEqual(_typed("mapping", "vector")["value"]["source"],
                         {"kind": "vector", "direction": [1.0, 0.0, 1.0]})
        self.assertEqual(_typed("mapping", "major_axis")["value"]["source"],
                         {"kind": "major_axis", "toward": [0.0, 0.0, 1.0]})
        mapping = {"source": {"kind": "vector", "direction": [1.0, 0.0, 2.0]},
                   "mirror": {"normal": [1.0, 0.0, 0.0], "offset": .25},
                   "reverse": True, "per_group": True}
        self.assertEqual(_typed("mapping", mapping), {"type": "mapping", "value": mapping})

    def test_new_clips_get_nonoverlapping_layers_unless_explicitly_chosen(self):
        track = self.track()
        edit = track.edit()
        graph = "color@1"
        first = edit.add_clip(graph, id="first", beats=(0, 4))
        second = edit.add_clip(graph, id="second", beats=(2, 6))
        adjacent = edit.add_clip(graph, id="adjacent", beats=(6, 8))
        explicit = edit.add_clip(graph, id="explicit", beats=(0, 4), z=0)
        self.assertEqual((first.z, second.z, adjacent.z, explicit.z), (0, 1, 0, 0))
        edit.apply()
        saved = self.calls[-1][1]["candidate"]["clips"]
        self.assertEqual(saved["second"]["z_index"], 1)
        self.assertEqual(saved["explicit"]["z_index"], 0)

    def test_an_unknown_form_passes_tagged_inputs_to_the_native_validator(self):
        edit = self.track().edit()
        hit = {"type": "hit", "value": {"points": [[0, 1], [1, 0]]}}
        clip = edit.add_clip("old.form@1", id="old", beats=(0, 4), inputs={"brightness": hit})
        self.assertEqual(clip.graph, "old.form@1")
        self.assertEqual(edit.candidate["clips"]["old"]["inputs"], {"brightness": hit})
        with self.assertRaisesRegex(TrackError, "tagged value"):
            edit.add_clip("old.form@1", id="bare", beats=(0, 4), inputs={"alpha": 0.5})

    def test_a_space_source_passes_on_a_signal_socket(self):
        space = {"type": "space", "value": {"axis": {"source": {"kind": "u"}}, "curve": {"points": [[0, 0], [1, 1]]}}}
        self.assertEqual(_typed({"signal": {"unit": "proportion"}}, space), space)

    def track(self):
        nodes = {"color@1": {"name": "Color", "inputs": {
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
        graph = "color@1"
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
        def snapshot(revision, clips):
            values = copy.deepcopy(track._values)
            values["revision"] = revision
            values["document"]["clips"] = clips
            return build_namespace({"schema_version": 1, "revision": revision,
                "agent_kind": "track_copilot", "scope": {"score_id": "score"},
                "root": {"track": values, "nodes": track._nodes}}, workspace,
                host_call=track._host_call)
        cached_a = snapshot("a", {})
        cached_b = snapshot("b", {"other": {"graph": "color@1", "start": 0, "duration": 4, "seed": 0}})
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
        graph = "color@1"
        edit.add_clip(graph, id="clip", beats=(0, 4), inputs={"width": .8})
        window = edit.window(beats=(0, 4))
        self.assertEqual(len(track.clips), 0)
        self.assertEqual(edit.apply(), "next")
        self.assertEqual(stale.base_revision, "base")
        self.assertEqual(window._revision, "base")
        self.assertEqual(len(track.clips), 1)
        self.assertEqual(track.document["clips"]["clip"]["graph"], "color@1")
        with self.assertRaises(TypeError):
            track.document["clips"]["extra"] = {}
        with self.assertRaises(AttributeError):
            window.start_s = 0

    def test_override_reset_sets_selection_and_source_is_independent(self):
        track = self.track()
        edit = track.edit()
        graph = "color@1"
        clip = edit.add_clip(graph, id="clip", beats=(0, 4), inputs={"width": .8})
        edit.update_clip(clip, selection="bars", inputs={"width": None})
        value = edit.candidate["clips"]["clip"]
        self.assertEqual(value["selection"], {"expression": "bars"})
        self.assertEqual(value["inputs"]["width"], {"type": "proportion", "value": .25})
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

    def test_rich_values_keep_their_units(self):
        self.assertEqual(_typed("color", "#ff0000"), {"type": "color", "value": from_srgb([1., 0., 0.])})
        self.assertEqual(_typed("envelope", [[0, 1], [1, 0]])["value"], {"points": [[0, 1], [1, 0]]})
        self.assertEqual(_typed("gradient", [(0, "#ff0000"), (1, "#0000ff")])["value"],
                         {"stops": [{"t": 0, "color": from_srgb([1., 0., 0.])},
                                    {"t": 1, "color": from_srgb([0., 0., 1.])}]})
        with self.assertRaises(ValueError):
            _typed("beats", {"type": "proportion", "value": .5})

    def test_isolated_preview_preserves_clip_timing_and_the_complete_draft(self):
        # _preview() returns a bare score document (`{"clips": ...}`), matching
        # what RenderRequest.edit deserializes on the Rust side — never wrapped
        # in `{"candidate": ...}`, which is only the score_check/score_apply
        # wire shape.
        track = self.track()
        edit = track.edit()
        graph = "color@1"
        first = edit.add_clip(graph, id="first", beats=(1, 4), selection="front")
        edit.add_clip(graph, id="second", beats=(2, 5), selection="rear", blend="add", z=1)
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
