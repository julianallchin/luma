#!/usr/bin/env python3
"""Standalone test driver for the Luma persistent Python executor.

No pytest: plain asserts plus a tiny runner, because the app venv is not
guaranteed to have a test framework. Every test drives a real worker subprocess
over the real NDJSON protocol against a synthetic workspace.

Run:
    ~/Library/Caches/com.luma.luma/python-env/bin/python3 \\
        src-tauri/python/luma_exec/tests/run_tests.py

Coverage (design §21.5 + §21.6):
    variable persistence, binding refresh across revisions, `luma` repair after
    reassignment, last-expression display, stdout/stderr capture, fd-level
    native writes not corrupting framing, exceptions preserving prior stdout,
    figure capture/close, bounded reprs, f16 npy loading, stereo PCM shape,
    unavailable branches, read-only arrays, ping, SIGINT interruption, shutdown.
"""

from __future__ import annotations

import shutil
import sys
import tempfile
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import harness  # noqa: E402

REV1 = harness.manifest_rel("r-1")
REV2 = harness.manifest_rel("r-2")

TESTS: list = []
_STATE: dict = {}


def test(fn):
    TESTS.append(fn)
    return fn


def shared() -> harness.WorkerClient:
    """One long-lived worker: kernel semantics are about state that persists."""
    client = _STATE.get("shared")
    if client is None:
        client = harness.WorkerClient(_STATE["python"], _STATE["workspace"])
        _STATE["shared"] = client
        _STATE["startup_s"] = client.startup_s
    return client


def fresh() -> harness.WorkerClient:
    client = harness.WorkerClient(_STATE["python"], _STATE["workspace"])
    _STATE.setdefault("extra", []).append(client)
    return client


def ok(result: dict, note: str = "") -> dict:
    assert result["status"] == "ok", (
        f"{note} expected ok, got {result['status']}: {result.get('traceback')}"
    )
    return result


# ---------------------------------------------------------------------------
# tests
# ---------------------------------------------------------------------------


@test
def test_ready_frame():
    client = shared()
    assert client.ready["type"] == "ready"
    assert client.ready["pid"] > 0
    assert client.ready["python"].startswith("3.")
    assert client.ready["warnings"] == [], f"preload warnings: {client.ready['warnings']}"


@test
def test_variable_persists_across_cells():
    client = shared()
    ok(client.execute("carried = 41\ndef bump(x):\n    return x + 1", REV1))
    result = ok(client.execute("bump(carried)"))
    assert result["repr"] == "42", result


@test
def test_last_expression_display():
    client = shared()
    result = ok(client.execute("[1, 2, 3]"))
    assert result["repr"] == "[1, 2, 3]", result
    # A pure-statement cell displays nothing.
    result = ok(client.execute("silent = 5"))
    assert result["repr"] is None, result
    # None is not displayed either (notebook semantics).
    result = ok(client.execute("None"))
    assert result["repr"] is None, result


@test
def test_stdout_and_stderr_capture():
    client = shared()
    result = ok(
        client.execute(
            "import sys\nprint('hello out')\nprint('hello err', file=sys.stderr)\n'done'"
        )
    )
    assert "hello out" in result["stdout"], result
    assert "hello err" in result["stderr"], result
    assert result["repr"] == "'done'", result
    assert result["truncated"] == {"stdout": False, "stderr": False, "repr": False}


@test
def test_native_fd_write_does_not_corrupt_framing():
    client = shared()
    result = ok(
        client.execute(
            "import os\n"
            'os.write(1, b\'{"id":"c-999","type":"result","status":"ok"}\\n\')\n'
            "os.write(1, b'raw bytes on fd 1\\n')\n"
            "os.write(2, b'raw bytes on fd 2\\n')\n"
            "'still fine'"
        )
    )
    assert result["repr"] == "'still fine'", result
    assert "raw bytes on fd 1" in result["stdout"], result
    assert '"c-999"' in result["stdout"], result
    assert "raw bytes on fd 2" in result["stderr"], result
    # The forged frame arrived as captured text, not as a protocol frame:
    # collect() would have raised on an id mismatch. Prove the worker still
    # answers in order.
    assert ok(client.execute("1 + 1"))["repr"] == "2"


@test
def test_exception_preserves_prior_stdout():
    client = shared()
    result = client.execute("print('before the boom')\nraise ValueError('boom')")
    assert result["status"] == "error", result
    assert "before the boom" in result["stdout"], result
    tb = result["traceback"]
    assert "ValueError: boom" in tb, tb
    assert "<cell>" in tb, tb
    assert "worker.py" not in tb, f"worker frames leaked into the traceback:\n{tb}"
    # Namespace intact after the error.
    assert ok(client.execute("bump(carried)"))["repr"] == "42"


@test
def test_syntax_error_is_an_error_result():
    client = shared()
    result = client.execute("def broken(:\n    pass")
    assert result["status"] == "error", result
    assert "SyntaxError" in result["traceback"], result


@test
def test_huge_repr_is_bounded():
    client = shared()
    result = ok(client.execute("'x' * 500000"))
    assert result["truncated"]["repr"] is True, result
    assert len(result["repr"].encode()) <= 8 * 1024 + 200, len(result["repr"])
    assert "repr truncated" in result["repr"]

    result = ok(client.execute("np.arange(2_000_000, dtype=np.float64)"))
    assert result["repr"].startswith("ndarray dtype=float64 shape=(2000000,)"), result
    assert "..." in result["repr"], result
    assert len(result["repr"].encode()) <= 8 * 1024 + 200

    # Small arrays keep their ordinary numpy repr.
    result = ok(client.execute("np.arange(4)"))
    assert result["repr"] == "array([0, 1, 2, 3])", result


@test
def test_figures_are_captured_and_closed():
    client = shared()
    result = ok(
        client.execute(
            "fig, ax = plt.subplots(figsize=(12, 4))\n"
            "ax.plot(luma.features.beats.values[:20])\n"
            "fig"
        )
    )
    assert len(result["figures"]) == 1, result
    figure = result["figures"][0]
    assert figure["artifact_rel"].startswith("outputs/fig-"), figure
    assert figure["artifact_rel"].endswith(".png"), figure
    assert (Path(_STATE["workspace"]) / figure["artifact_rel"]).exists()
    assert figure["width"] == 1200 and figure["height"] == 400, figure
    # Saved on the app's panel grey (--background), not matplotlib's white: a
    # white rectangle in the dark chat panel is the bug this pins.
    from PIL import Image  # a matplotlib dependency, so always present here

    with Image.open(Path(_STATE["workspace"]) / figure["artifact_rel"]) as image:
        assert image.convert("RGB").getpixel((0, 0)) == (39, 39, 39), image
    # A figure last-expression is shown as an image, not as a repr.
    assert result["repr"] is None, result
    # Figures were closed, so the next cell starts clean.
    assert ok(client.execute("plt.get_fignums()"))["repr"] == "[]"


@test
def test_figure_cap_per_cell():
    client = shared()
    result = ok(client.execute("for _ in range(11):\n    plt.figure(figsize=(1, 1))"))
    assert len(result["figures"]) == 8, len(result["figures"])
    assert any("only the first 8" in w for w in result["warnings"]), result["warnings"]
    assert ok(client.execute("plt.get_fignums()"))["repr"] == "[]"


@test
def test_binding_refresh_preserves_user_variables():
    client = shared()
    ok(client.execute("survivor = 'i was here'", REV1))
    assert ok(client.execute("luma.track.title"))["repr"] == "'Synthetic One'"
    result = ok(client.execute("luma.track.title", REV2))
    assert result["repr"] == "'Synthetic Two'", result
    assert ok(client.execute("luma.meta.revision"))["repr"] == "'r-2'"
    assert ok(client.execute("survivor"))["repr"] == "'i was here'"
    assert ok(client.execute("bump(carried)"))["repr"] == "42"
    # Omitting manifest_rel reuses the last installed revision.
    assert ok(client.execute("luma.meta.revision"))["repr"] == "'r-2'"


@test
def test_reassigning_luma_is_repaired_next_cell():
    client = shared()
    ok(client.execute("luma = 'clobbered'\nluma"))
    assert ok(client.execute("luma.track.id"))["repr"] == "'track-synthetic'"
    ok(client.execute("del luma"))
    assert ok(client.execute("luma.meta.revision"))["repr"] == "'r-2'"


@test
def test_window_and_meta():
    client = shared()
    result = ok(client.execute("(luma.window.start_s, luma.window.end_s)"))
    assert result["repr"] == "(0.0, 30.0)", result
    assert ok(client.execute("luma.meta.agent_kind"))["repr"] == "'track_copilot'"
    assert ok(client.execute("luma.meta.scope.track_id"))["repr"] == "'track-synthetic'"


@test
def test_raw_le_tensor_and_axes():
    client = shared()
    beats = ok(client.execute("luma.features.beats"))["repr"]
    assert "f32[200]" in beats and "unit=s" in beats, beats
    assert ok(client.execute("luma.features.beats.values.dtype.str"))["repr"] == "'<f4'"
    assert ok(client.execute("float(luma.features.beats.values[-1])"))["repr"] == "100.0"
    # Event tensors expose their seconds through times_s.
    assert (
        ok(client.execute("bool(np.allclose(luma.features.beats.times_s, "
                          "luma.features.beats.values))"))["repr"]
        == "True"
    )
    assert (
        ok(client.execute("luma.features.beats.provenance['processor_version']"))["repr"]
        == "3"
    )


@test
def test_f16_npy_loads_without_upcast():
    client = shared()
    assert ok(client.execute("luma.features.mel.values.dtype.name"))["repr"] == "'float16'"
    assert ok(client.execute("luma.features.mel.values.shape"))["repr"] == "(40, 100)"
    assert (
        ok(client.execute("float(luma.features.mel.frequencies_hz[0])"))["repr"] == "20.0"
    )
    assert (
        ok(client.execute("float(luma.features.mel.times_s[10])"))["repr"]
        == "0.1"
    )
    # 100/4000 rounded to float16 — the point is that it is *not* upcast.
    assert (
        ok(client.execute(
            "bool(abs(float(luma.features.mel.values[1, 0]) - 0.025) < 1e-4) "
            "and luma.features.mel.values.itemsize == 2"
        ))["repr"]
        == "True"
    )


@test
def test_pcm_stereo_shape_and_sample_rate():
    client = shared()
    assert ok(client.execute("luma.audio.mix.values.shape"))["repr"] == "(1000, 2)"
    assert ok(client.execute("luma.audio.mix.sample_rate_hz"))["repr"] == "48000.0"
    # Interleaved row-major: channel 1 is the negation of channel 0.
    assert (
        ok(client.execute(
            "bool(np.allclose(luma.audio.mix.values[:, 0], "
            "-luma.audio.mix.values[:, 1]))"
        ))["repr"]
        == "True"
    )
    assert ok(client.execute("luma.audio.mix.channels"))["repr"] == "['l', 'r']"
    assert (
        ok(client.execute("float(luma.audio.mix.times_s[48000 // 1000])"))["repr"]
        == "0.001"
    )


@test
def test_graph_view_axes():
    client = shared()
    assert (
        ok(client.execute("luma.graph.run.views['view_signal_1'].primitive_ids"))["repr"]
        == "['p0', 'p1', 'p2']"
    )
    assert (
        ok(client.execute("luma.graph.run.views.view_signal_1.channels"))["repr"]
        == "['dimmer', 'strobe']"
    )
    assert (
        ok(client.execute(
            "float(luma.graph.run.views['view_signal_1'].times_s[0])"
        ))["repr"]
        == "4.0"
    )
    assert (
        ok(client.execute("sorted(luma.graph.run.views.keys())"))["repr"]
        == "['view_signal_1']"
    )


@test
def test_arrays_are_read_only():
    client = shared()
    assert (
        ok(client.execute("luma.features.beats.values.flags.writeable"))["repr"] == "False"
    )
    result = client.execute("luma.features.beats.values[0] = 1.0")
    assert result["status"] == "error", result
    assert "read-only" in result["traceback"], result["traceback"]
    # np.asarray works and computation on a copy is fine.
    assert ok(client.execute("float(np.asarray(luma.features.beats).sum() > 0)"))["repr"] == "1.0"


@test
def test_unavailable_branch():
    client = shared()
    result = ok(client.execute("luma.track.key"))
    assert "unavailable" in result["repr"] and "no key detection" in result["repr"], result
    result = client.execute("luma.track.key.values")
    assert result["status"] == "error", result
    assert "LumaUnavailableError" in result["traceback"], result["traceback"]
    assert "no key detection" in result["traceback"], result["traceback"]
    # Sub-paths of an unavailable branch stay unavailable rather than exploding.
    result = ok(client.execute("luma.audio.stems['drums']"))
    assert "stems have not been separated" in result["repr"], result


@test
def test_catalog():
    client = shared()
    result = ok(client.execute("luma.catalog()"))
    text = result["repr"]
    for needle in (
        "revision r-2",
        "luma.features.beats",
        "unavailable:",
        "no key detection",
        "axes:",
        "sr=48000",
    ):
        assert needle in text, f"catalog missing {needle!r}:\n{text}"


@test
def test_records_support_dict_and_attribute_access():
    client = shared()
    assert (
        ok(client.execute("sorted(luma.features.drum_onsets.keys())"))["repr"]
        == "['kick']"
    )
    assert (
        ok(client.execute("len(luma.features.drum_onsets['kick'].values)"))["repr"] == "50"
    )
    assert (
        ok(client.execute("len(luma.features.drum_onsets.kick.values)"))["repr"] == "50"
    )
    result = client.execute("luma.features.nope")
    assert result["status"] == "error", result
    assert "has no binding 'nope'" in result["traceback"], result["traceback"]


@test
def test_record_repr_is_compact_but_catalog_remains_explicit():
    client = shared()
    root = ok(client.execute("luma"))["repr"]
    assert root.startswith("<luma>"), root
    assert "luma binding revision" not in root, root

    result = ok(
        client.execute(
            "from luma_exec.bindings import LumaRecord\n"
            "many = LumaRecord({f'key_{i}': i for i in range(12)}, 'many')\n"
            "outer = LumaRecord({'many': many}, 'outer')\n"
            "print(repr(many))\n"
            "print(repr(outer))"
        )
    )
    text = result["stdout"]
    assert ".key_7" in text, text
    assert ".key_8" not in text, text
    assert "key_7, … (4 more)" in text, text
    assert "4 more; use .keys() or luma.catalog()" in text, text


@test
def test_synchronous_host_call_round_trip():
    client = fresh()
    seen = []

    def handler(method, payload):
        seen.append((method, payload))
        return {"answer": payload["value"] + 1}

    result = ok(
        client.execute(
            "reply = _luma_host_call('test.increment', {'value': 41})\n"
            "reply['answer']",
            REV1,
            host_handler=handler,
        )
    )
    assert result["repr"] == "42", result
    assert seen == [("test.increment", {"value": 41})]


@test
def test_venue_render_is_a_host_call_that_becomes_a_cell_figure():
    """A host-rendered PNG and a matplotlib figure share one figures list."""
    client = fresh()
    workspace = Path(_STATE["workspace"])

    def render(method, payload):
        assert method == "venue.render", method
        (workspace / "outputs").mkdir(parents=True, exist_ok=True)
        rel = "outputs/stage-protocol.png"
        (workspace / rel).write_bytes(b"\x89PNG\r\n\x1a\n")
        return {
            "artifactRel": rel,
            "width": payload["width"],
            "height": payload["height"],
            "view": payload["view"],
            "t": payload["t"],
        }

    result = ok(
        client.execute(
            "shot = luma.venue.render(view='overhead', t=3.0, width=64, height=48)\n"
            "plt.figure(figsize=(1, 1))\n"
            "(luma.venue.views[0], shot.view, shot.path.exists(), repr(shot))",
            REV1,
            host_handler=render,
        )
    )
    assert result["repr"] == (
        "('front', 'overhead', True, '<StageImage overhead t=3s 64x48>')"
    ), result
    figures = result["figures"]
    assert len(figures) == 2, figures
    assert figures[0] == {
        "artifact_rel": "outputs/stage-protocol.png",
        "width": 64,
        "height": 48,
    }, figures
    assert figures[1]["artifact_rel"].startswith("outputs/fig-"), figures


@test
def test_host_rejection_is_structured_and_kernel_survives():
    client = fresh()

    def reject(_method, _payload):
        raise harness.HostCallRejected("conflict", "the track changed")

    result = client.execute(
        "kept_after_rejection = 9\n_luma_host_call('track.apply', {})",
        REV1,
        host_handler=reject,
    )
    assert result["status"] == "error", result
    assert "LumaHostCallError" in result["traceback"], result
    assert "the track changed" in result["traceback"], result
    assert ok(client.execute("kept_after_rejection"))["repr"] == "9"


@test
def test_track_check_recognizes_worker_host_errors():
    """The file-launched worker and track facade must share one error class."""
    client = fresh()

    def reject(_method, _payload):
        raise harness.HostCallRejected("invalid_edit", "the candidate is invalid")

    result = ok(
        client.execute(
            "from luma_exec.track import Track\n"
            "track = Track({\n"
            "    'id': 'track-synthetic',\n"
            "    'title': 'Synthetic',\n"
            "    'duration_s': 100.0,\n"
            "    'revision': 'revision-1',\n"
            "    'editable': True,\n"
            "    'clips': [],\n"
            "}, host_call=_luma_host_call)\n"
            "checked = track.edit().check()\n"
            "(checked.ok, checked.errors)",
            REV1,
            host_handler=reject,
        )
    )
    assert result["repr"] == "(False, ('the candidate is invalid',))", result


@test
def test_host_call_payload_is_bounded_before_it_reaches_the_host():
    client = fresh()
    result = client.execute(
        "_luma_host_call('test.too_large', {'value': 'x' * (9 * 1024 * 1024)})",
        REV1,
    )
    assert result["status"] == "error", result
    assert "maximum is" in result["traceback"], result
    assert ok(client.execute("40 + 2"))["repr"] == "42"


@test
def test_ping():
    client = shared()
    frame = client.ping()
    assert frame["type"] == "pong", frame
    assert frame["pid"] == client.ready["pid"], frame


@test
def test_sigint_interrupts_cell_and_preserves_namespace():
    client = fresh()
    ok(client.execute("before_interrupt = 'kept'", REV1))
    client._counter += 1
    request_id = f"c-{client._counter}"
    client.send(
        {
            "id": request_id,
            "op": "exec",
            "code": "print('spinning')\nwhile True:\n    pass",
        }
    )
    time.sleep(0.5)
    client.interrupt()
    result = client.collect(request_id, timeout=30.0)
    assert result["status"] == "interrupted", result
    assert "KeyboardInterrupt" in (result["traceback"] or ""), result
    assert "spinning" in result["stdout"], result
    # The worker survived and kept its namespace.
    assert ok(client.execute("before_interrupt"))["repr"] == "'kept'"
    assert client.ping()["type"] == "pong"


@test
def test_sigint_between_cells_does_not_kill_the_worker():
    client = fresh()
    ok(client.execute("idle = 7", REV1))
    client.interrupt()
    time.sleep(0.3)
    assert client.ping()["type"] == "pong"
    assert ok(client.execute("idle"))["repr"] == "7"


@test
def test_shutdown():
    client = fresh()
    ok(client.execute("1", REV1))
    client._counter += 1
    client.send({"id": "bye", "op": "shutdown"})
    frame = client.next_frame(timeout=10.0)
    assert frame == {"id": "bye", "type": "goodbye"}, frame
    assert client.proc.wait(timeout=10) == 0


# ---------------------------------------------------------------------------
# runner
# ---------------------------------------------------------------------------


def run_stdlib_suites() -> int:
    """The focused suites next door, in the same command.

    `test_track.py` and `test_venue.py` drive the same facades this file drives
    through a worker, at a smaller radius and with no venv — which is exactly
    why they are easy to forget. They went unrun for a whole surface; one
    command is the fix.
    """
    import unittest

    loader = unittest.TestLoader()
    suite = unittest.TestSuite(
        loader.loadTestsFromName(name) for name in ("test_track", "test_venue")
    )
    print("\n--- stdlib suites (test_track, test_venue) ---")
    result = unittest.TextTestRunner(verbosity=1, stream=sys.stdout).run(suite)
    return len(result.failures) + len(result.errors)


def main() -> int:
    python, is_venv = harness.find_python()
    root = Path(tempfile.mkdtemp(prefix="luma-exec-tests-"))
    _STATE["python"] = python
    _STATE["workspace"] = harness.build_workspace(root / "workspace")

    print(f"interpreter: {python}{'  (app venv)' if is_venv else '  (fallback)'}")
    print(f"workspace:   {_STATE['workspace']}")
    print()

    passed, failed = 0, []
    try:
        for fn in TESTS:
            name = fn.__name__
            started = time.perf_counter()
            try:
                fn()
            except BaseException:
                failed.append((name, traceback.format_exc()))
                print(f"FAIL  {name}")
            else:
                passed += 1
                print(f"ok    {name}  ({(time.perf_counter() - started) * 1000:.0f} ms)")
    finally:
        for client in [_STATE.get("shared"), *(_STATE.get("extra") or [])]:
            if client is not None:
                client.close()
        shutil.rmtree(root, ignore_errors=True)

    stdlib_failures = run_stdlib_suites()

    print()
    if "startup_s" in _STATE:
        print(f"worker startup (spawn -> ready frame): {_STATE['startup_s'] * 1000:.0f} ms")
    for name, tb in failed:
        print(f"\n--- {name} ---\n{tb}")
    print(f"\n{passed} passed, {len(failed) + stdlib_failures} failed")
    return 1 if failed or stdlib_failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
