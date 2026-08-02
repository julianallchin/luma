"""Test harness: synthetic workspace builder + a real NDJSON protocol client.

No pytest — the app venv may not have it. `run_tests.py` is the single driver.
"""

from __future__ import annotations

import json
import os
import queue
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

import numpy as np

WORKER = Path(__file__).resolve().parent.parent / "worker.py"

PCM_VERSION = 2
PCM_SAMPLE_RATE = 48000
PCM_CHANNELS = 2
PCM_FRAMES = 1000


# ---------------------------------------------------------------------------
# synthetic workspace
# ---------------------------------------------------------------------------


def build_workspace(root: Path) -> Path:
    """Create `<root>/{inputs,scratch,outputs}` with two manifest revisions."""
    workspace = Path(root)
    inputs = workspace / "inputs"
    for sub in ("inputs", "scratch", "outputs"):
        (workspace / sub).mkdir(parents=True, exist_ok=True)

    beats = np.linspace(0.0, 100.0, 200).astype("<f4")
    beats.tofile(inputs / "beats.raw")

    kicks = np.linspace(0.5, 99.5, 50).astype("<f4")
    kicks.tofile(inputs / "kick.raw")

    mel = (np.arange(40 * 100, dtype=np.float32).reshape(40, 100) / 4000.0).astype(
        np.float16
    )
    np.save(inputs / "mel.npy", mel)

    frames = np.zeros((PCM_FRAMES, PCM_CHANNELS), dtype="<f4")
    frames[:, 0] = np.sin(np.arange(PCM_FRAMES) * 0.01)
    frames[:, 1] = -frames[:, 0]
    with open(inputs / "mix.pcm", "wb") as fh:
        fh.write(
            struct.pack(
                "<IIHQ",
                PCM_VERSION,
                PCM_SAMPLE_RATE,
                PCM_CHANNELS,
                PCM_FRAMES * PCM_CHANNELS,
            )
        )
        fh.write(frames.tobytes())

    view = np.random.default_rng(7).random((3, 10, 2)).astype("<f4")
    view.tofile(inputs / "view.raw")

    write_manifest(workspace, "r-1", title="Synthetic One")
    write_manifest(workspace, "r-2", title="Synthetic Two")
    return workspace


def manifest_rel(revision: str) -> str:
    return f"inputs/manifest-{revision}.json"


def write_manifest(workspace: Path, revision: str, title: str) -> str:
    rel = manifest_rel(revision)
    manifest = {
        "schema_version": 1,
        "revision": revision,
        "agent_kind": "track_copilot",
        "scope": {
            "track_id": "track-synthetic",
            "venue_id": None,
            "score_id": None,
            "pattern_id": None,
            "implementation_id": None,
            "window": {"start_s": 0.0, "end_s": 30.0},
        },
        "root": {
            "track": {
                "id": "track-synthetic",
                "title": title,
                "artist": "Test Fixture",
                "duration_s": 100.0,
                "bpm": 128.0,
                "revision": f"track-{revision}",
                "clips": [],
                "editable": True,
                "key": {
                    "$kind": "unavailable",
                    "reason": "no key detection exists in Luma",
                },
            },
            "audio": {
                "mix": {
                    "$kind": "tensor",
                    "artifact_id": "mix",
                    "dtype": "f32",
                    "shape": [PCM_FRAMES, PCM_CHANNELS],
                    "byte_offset": 18,
                    "unit": None,
                    "axes": [
                        {
                            "kind": "linear",
                            "name": "time",
                            "start": 0.0,
                            "step": 1.0 / PCM_SAMPLE_RATE,
                            "count": PCM_FRAMES,
                            "unit": "s",
                        },
                        {"kind": "labels", "name": "channel", "labels": ["l", "r"]},
                    ],
                    "provenance": {"source": "mix_pcm", "processor_version": 2},
                },
                "stems": {
                    "$kind": "unavailable",
                    "reason": "stems have not been separated for this track",
                },
            },
            "features": {
                "beats": {
                    "$kind": "tensor",
                    "artifact_id": "beats",
                    "dtype": "f32",
                    "shape": [200],
                    "byte_offset": 0,
                    "unit": "s",
                    "axes": [{"kind": "index", "name": "event", "count": 200}],
                    "provenance": {"source": "beat_this", "processor_version": 3},
                },
                "drum_onsets": {
                    "kick": {
                        "$kind": "tensor",
                        "artifact_id": "kick",
                        "dtype": "f32",
                        "shape": [50],
                        "byte_offset": 0,
                        "unit": "s",
                        "axes": [{"kind": "index", "name": "event", "count": 50}],
                        "provenance": {"source": "adtof"},
                    }
                },
                "mel": {
                    "$kind": "tensor",
                    "artifact_id": "mel",
                    "dtype": "f16",
                    "shape": [40, 100],
                    "byte_offset": 0,
                    "unit": None,
                    "axes": [
                        {
                            "kind": "coordinates",
                            "name": "frequency",
                            "values": [float(20 * (i + 1)) for i in range(40)],
                            "unit": "hz",
                        },
                        {
                            "kind": "linear",
                            "name": "time",
                            "start": 0.0,
                            "step": 0.01,
                            "count": 100,
                            "unit": "s",
                        },
                    ],
                    "provenance": {"source": "librosa.melspectrogram"},
                },
            },
            "graph": {
                "run": {
                    "views": {
                        "view_signal_1": {
                            "$kind": "tensor",
                            "artifact_id": "view",
                            "dtype": "f32",
                            "shape": [3, 10, 2],
                            "byte_offset": 0,
                            "unit": None,
                            "axes": [
                                {
                                    "kind": "labels",
                                    "name": "primitive",
                                    "labels": ["p0", "p1", "p2"],
                                },
                                {
                                    "kind": "linear",
                                    "name": "time",
                                    "start": 4.0,
                                    "step": 0.05,
                                    "count": 10,
                                    "unit": "s",
                                },
                                {
                                    "kind": "labels",
                                    "name": "channel",
                                    "labels": ["dimmer", "strobe"],
                                },
                            ],
                            "provenance": {"source": "graph_run"},
                        }
                    }
                }
            },
        },
        "artifacts": {
            "beats": {
                "kind": "tensor",
                "encoding": "raw_le",
                "rel_path": "inputs/beats.raw",
                "byte_len": 800,
                "content_hash": None,
            },
            "kick": {
                "kind": "tensor",
                "encoding": "raw_le",
                "rel_path": "inputs/kick.raw",
                "byte_len": 200,
                "content_hash": None,
            },
            "mel": {
                "kind": "tensor",
                "encoding": "npy",
                "rel_path": "inputs/mel.npy",
                "byte_len": (workspace / "inputs" / "mel.npy").stat().st_size,
                "content_hash": None,
            },
            "mix": {
                "kind": "tensor",
                "encoding": "pcm_f32",
                "rel_path": "inputs/mix.pcm",
                "byte_len": 18 + PCM_FRAMES * PCM_CHANNELS * 4,
                "content_hash": None,
                "sample_rate_hz": PCM_SAMPLE_RATE,
                "channels": PCM_CHANNELS,
            },
            "view": {
                "kind": "tensor",
                "encoding": "raw_le",
                "rel_path": "inputs/view.raw",
                "byte_len": 3 * 10 * 2 * 4,
                "content_hash": None,
            },
        },
    }
    (workspace / rel).write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return rel


# ---------------------------------------------------------------------------
# protocol client
# ---------------------------------------------------------------------------


class WorkerTimeout(RuntimeError):
    pass


class HostCallRejected(RuntimeError):
    def __init__(self, code: str, message: str):
        self.code = code
        super().__init__(message)


class WorkerClient:
    """Drives a real worker subprocess over the real NDJSON protocol."""

    def __init__(self, python: str, workspace: Path):
        self.workspace = Path(workspace)
        scratch = self.workspace / "scratch"
        env = {
            "PATH": "/usr/bin:/bin",
            "PYTHONUNBUFFERED": "1",
            "MPLBACKEND": "Agg",
            "HOME": str(scratch),
            "MPLCONFIGDIR": str(scratch / ".matplotlib"),
            "TMPDIR": str(scratch),
        }
        self.proc = subprocess.Popen(
            [python, str(WORKER), "--workspace", str(self.workspace)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(scratch),
            env=env,
            start_new_session=True,
        )
        self._frames: "queue.Queue[dict[str, Any] | None]" = queue.Queue()
        self._reader = threading.Thread(target=self._read_frames, daemon=True)
        self._reader.start()
        self._stderr_chunks: list[bytes] = []
        self._err_reader = threading.Thread(target=self._read_stderr, daemon=True)
        self._err_reader.start()
        self._counter = 0
        started = time.perf_counter()
        self.ready = self.next_frame(timeout=120.0)
        self.startup_s = time.perf_counter() - started
        if self.ready.get("type") != "ready":
            raise RuntimeError(f"expected ready frame, got {self.ready}")

    # -- plumbing -------------------------------------------------------

    def _read_frames(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                self._frames.put(json.loads(line))
            except Exception:
                self._frames.put({"type": "unparseable", "raw": line.decode("utf-8", "replace")})
        self._frames.put(None)

    def _read_stderr(self) -> None:
        assert self.proc.stderr is not None
        for chunk in iter(lambda: self.proc.stderr.read(4096), b""):
            self._stderr_chunks.append(chunk)

    def raw_stderr(self) -> str:
        return b"".join(self._stderr_chunks).decode("utf-8", "replace")

    def next_frame(self, timeout: float = 60.0) -> dict[str, Any]:
        try:
            frame = self._frames.get(timeout=timeout)
        except queue.Empty:
            raise WorkerTimeout(
                f"no frame within {timeout}s (stderr: {self.raw_stderr()!r})"
            ) from None
        if frame is None:
            raise WorkerTimeout(f"worker exited (stderr: {self.raw_stderr()!r})")
        return frame

    def send(self, request: dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(request) + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    # -- operations -----------------------------------------------------

    def execute(
        self,
        code: str,
        manifest_rel: str | None = None,
        timeout: float = 60.0,
        host_handler=None,
    ) -> dict[str, Any]:
        self._counter += 1
        request_id = f"c-{self._counter}"
        request: dict[str, Any] = {"id": request_id, "op": "exec", "code": code}
        if manifest_rel is not None:
            request["manifest_rel"] = manifest_rel
        self.send(request)
        return self.collect(request_id, timeout=timeout, host_handler=host_handler)

    def collect(
        self,
        request_id: str,
        timeout: float = 60.0,
        host_handler=None,
    ) -> dict[str, Any]:
        stdout, stderr = "", ""
        deadline = time.time() + timeout
        while True:
            frame = self.next_frame(timeout=max(0.1, deadline - time.time()))
            if frame.get("id") != request_id:
                raise AssertionError(f"frame for another request: {frame}")
            if frame.get("type") == "started":
                continue
            if frame.get("type") == "stream":
                if frame["stream"] == "stdout":
                    stdout += frame["text"]
                else:
                    stderr += frame["text"]
                continue
            if frame.get("type") == "host_call":
                call_id = frame.get("call_id")
                try:
                    if host_handler is None:
                        raise HostCallRejected(
                            "unavailable", "host calls are not available for this cell"
                        )
                    value = host_handler(frame.get("method"), frame.get("payload"))
                    response = {
                        "id": request_id,
                        "op": "host_response",
                        "call_id": call_id,
                        "ok": True,
                        "value": value,
                    }
                except HostCallRejected as exc:
                    response = {
                        "id": request_id,
                        "op": "host_response",
                        "call_id": call_id,
                        "ok": False,
                        "error": {"code": exc.code, "message": str(exc)},
                    }
                self.send(response)
                continue
            if frame.get("type") == "result":
                frame["stdout"] = stdout
                frame["stderr"] = stderr
                return frame
            raise AssertionError(f"unexpected frame: {frame}")

    def ping(self, timeout: float = 10.0) -> dict[str, Any]:
        self._counter += 1
        request_id = f"c-{self._counter}"
        self.send({"id": request_id, "op": "ping"})
        return self.next_frame(timeout=timeout)

    def interrupt(self) -> None:
        os.killpg(os.getpgid(self.proc.pid), 2)

    def close(self) -> None:
        try:
            self._counter += 1
            self.send({"id": f"c-{self._counter}", "op": "shutdown"})
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()
            try:
                self.proc.wait(timeout=5)
            except Exception:
                pass


def find_python() -> tuple[str, bool]:
    """Return (interpreter, is_app_venv). Prefers the Luma app venv."""
    candidate = Path.home() / "Library/Caches/com.luma.luma/python-env/bin/python3"
    if candidate.exists():
        probe = subprocess.run(
            [str(candidate), "-c", "import numpy"], capture_output=True
        )
        if probe.returncode == 0:
            return str(candidate), True
    return sys.executable, False
