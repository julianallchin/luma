#!/usr/bin/env python3
"""Persistent Python execution kernel for one Luma agent thread (contract C2).

One worker owns one durable user namespace. It speaks newline-delimited JSON
over pipes and knows nothing about tracks, patterns, scores, venues, Tauri, or
SQLite — only the binding-manifest schema and this protocol.

CLI:
    worker.py --workspace <abs dir>

    cwd is expected to be <workspace>/scratch; env is host-controlled.

Startup:
    1. `proto_fd = os.dup(1)` — protocol frames go there and ONLY there.
    2. fd 1 and fd 2 are `dup2`'d onto capture pipes, so *native* writes are
       caught too (design §14.5); reader threads drain them forever into 32 KiB
       bounded buffers so a chatty C extension can never block on a full pipe.
    3. matplotlib is forced to Agg before pyplot is imported (§14.7).
    4. numpy / scipy / scipy.signal / librosa / matplotlib.pyplot are preloaded
       into the user namespace (§7.2); librosa's lazy submodules are touched so
       the cost lands at startup, not in the agent's first cell. A missing
       optional library degrades to a warning in the `ready` frame.

Requests (stdin, one JSON object per line):
    {"id":"c-1","op":"exec","code":"...","manifest_rel":"inputs/manifest-r-x.json"}
        `manifest_rel` is optional; omitting it reuses the last installed
        revision. Parsed manifests are cached by revision id.
    {"id":"c-2","op":"ping"}
    {"id":"c-3","op":"shutdown"}

Frames (protocol fd, one JSON object per line):
    {"type":"ready","pid":123,"python":"3.12.13","warnings":[...],"startup_ms":900}
    {"id":"c-1","type":"stream","stream":"stdout"|"stderr","text":"..."}
    {"id":"c-1","type":"result","status":"ok"|"error"|"interrupted",
     "repr":...|null,"traceback":...|null,
     "figures":[{"artifact_rel":"outputs/fig-<uuid>.png","width":..,"height":..}],
     "truncated":{"stdout":false,"stderr":false,"repr":false},
     "duration_ms":123,"warnings":[...]}
    {"id":"c-2","type":"pong","pid":123}
    {"id":"c-3","type":"goodbye"}
    {"id":null,"type":"error","message":"..."}      malformed request line

Interruption: the host sends SIGINT to the process group. The handler raises
KeyboardInterrupt only while a cell is running (yielding a terminal result with
status "interrupted", namespace intact); between cells the signal is ignored so
a late cancellation can neither kill the worker nor hit the next cell.
"""

from __future__ import annotations

import argparse
import ast
import builtins
import json
import os
import signal
import sys
import threading
import time
import traceback
from pathlib import Path
from typing import Any

# Clamp RLIMIT_NOFILE before any heavy import. Some hosts hand children an
# absurd soft limit (Bun sets it to i64::MAX); joblib/loky's fork path closes
# fds in a loop bounded by that limit, so an inherited huge value turns the
# librosa preload into an effectively infinite spin in a forked child.
try:
    import resource

    _soft, _hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    _cap = 65536 if _hard == resource.RLIM_INFINITY else min(_hard, 65536)
    if _soft > _cap:
        resource.setrlimit(resource.RLIMIT_NOFILE, (_cap, _hard))
except (ImportError, ValueError, OSError):
    pass

# Allow `python worker.py` (no package context) as well as `-m luma_exec.worker`.
if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from luma_exec import bindings, display, figures  # noqa: E402

#: Per-stream capture cap, per contract C2.
STREAM_LIMIT_BYTES = 32 * 1024

#: Written to fd 1 / fd 2 after a cell to prove the reader threads have drained
#: everything the cell produced. Stripped from the captured text.
SYNC_TOKEN = b"\x1e\x1eLUMA-SYNC\x1e\x1e"
SYNC_TIMEOUT_S = 2.0

CELL_FILENAME = "<cell>"


# ---------------------------------------------------------------------------
# fd-level output capture
# ---------------------------------------------------------------------------


class StreamCapture:
    """Drains one capture pipe into a bounded buffer, forever.

    Draining never stops at the cap: bytes past the limit are counted and
    dropped so that native code writing to a full pipe can never deadlock.
    """

    def __init__(self, read_fd: int, name: str, limit: int = STREAM_LIMIT_BYTES):
        self._fd = read_fd
        self.name = name
        self._limit = limit
        self._lock = threading.Lock()
        self._chunks: list[bytes] = []
        self._size = 0
        self._dropped = 0
        self._synced = threading.Event()
        self._thread = threading.Thread(
            target=self._run, name=f"capture-{name}", daemon=True
        )
        self._thread.start()

    def _run(self) -> None:
        carry = b""
        hold = len(SYNC_TOKEN) - 1
        while True:
            try:
                data = os.read(self._fd, 65536)
            except OSError:
                return
            if not data:
                return
            buf = carry + data
            while True:
                index = buf.find(SYNC_TOKEN)
                if index < 0:
                    break
                self._append(buf[:index])
                buf = buf[index + len(SYNC_TOKEN) :]
                self._synced.set()
            if len(buf) > hold:
                self._append(buf[:-hold] if hold else buf)
                carry = buf[-hold:] if hold else b""
            else:
                carry = buf

    def _append(self, data: bytes) -> None:
        if not data:
            return
        with self._lock:
            room = self._limit - self._size
            if room > 0:
                take = data[:room]
                self._chunks.append(take)
                self._size += len(take)
                data = data[room:]
            self._dropped += len(data)

    def sync(self) -> None:
        """Flush the pipe: write a sentinel and wait for the reader to see it."""
        self._synced.clear()
        try:
            os.write(1 if self.name == "stdout" else 2, SYNC_TOKEN)
        except OSError:
            return
        self._synced.wait(SYNC_TIMEOUT_S)

    def take(self) -> tuple[str, bool]:
        """Return (text, truncated) and reset the buffer."""
        with self._lock:
            data = b"".join(self._chunks)
            dropped = self._dropped
            self._chunks = []
            self._size = 0
            self._dropped = 0
        text = data.decode("utf-8", "replace")
        if dropped:
            text += f"\n… [{self.name} truncated, {dropped} bytes dropped]"
        return text, dropped > 0


# ---------------------------------------------------------------------------
# worker
# ---------------------------------------------------------------------------


class Worker:
    def __init__(self, workspace: Path):
        self.workspace = Path(workspace).resolve()
        self.namespace: dict[str, Any] = {
            "__name__": "__main__",
            "__builtins__": builtins,
        }
        self._manifests: dict[str, bindings.LumaNamespace] = {}
        self._manifest_rel_to_revision: dict[str, str] = {}
        self._current: bindings.LumaNamespace | None = None
        self._executing = False
        self._plt: Any = None
        self._proto_lock = threading.Lock()

        # --- fd isolation (design §14.5) --------------------------------
        self._proto_fd = os.dup(1)
        self._proto = os.fdopen(self._proto_fd, "w", buffering=1, encoding="utf-8")
        out_r, out_w = os.pipe()
        err_r, err_w = os.pipe()
        os.dup2(out_w, 1)
        os.dup2(err_w, 2)
        os.close(out_w)
        os.close(err_w)
        self.stdout = StreamCapture(out_r, "stdout")
        self.stderr = StreamCapture(err_r, "stderr")
        for stream in (sys.stdout, sys.stderr):
            try:
                stream.reconfigure(line_buffering=True)
            except Exception:  # pragma: no cover - non-standard stream objects
                pass

    # -- protocol -------------------------------------------------------

    def send(self, frame: dict[str, Any]) -> None:
        line = json.dumps(frame, ensure_ascii=False, default=str)
        with self._proto_lock:
            self._proto.write(line + "\n")
            self._proto.flush()

    # -- startup --------------------------------------------------------

    def preload(self) -> list[str]:
        """Import the analysis stack into the user namespace (design §7.2)."""
        warnings: list[str] = []

        try:
            import matplotlib

            matplotlib.use("Agg")
            import matplotlib.pyplot as plt

            self._plt = plt
            self.namespace["matplotlib"] = matplotlib
            self.namespace["plt"] = plt
        except Exception as exc:  # noqa: BLE001
            warnings.append(f"matplotlib unavailable: {exc}")

        try:
            import numpy as np

            self.namespace["np"] = np
            self.namespace["numpy"] = np
        except Exception as exc:  # noqa: BLE001
            warnings.append(f"numpy unavailable: {exc}")

        try:
            import scipy
            import scipy.signal

            self.namespace["scipy"] = scipy
        except Exception as exc:  # noqa: BLE001
            warnings.append(f"scipy unavailable: {exc}")

        try:
            import librosa

            self.namespace["librosa"] = librosa
            # librosa uses lazy_loader; touch the submodules an agent is likely
            # to reach for so the import cost lands here, not in cell 1.
            for attr in ("onset", "feature", "beat", "util", "effects", "core"):
                getattr(librosa, attr, None)
        except Exception as exc:  # noqa: BLE001
            warnings.append(f"librosa unavailable: {exc}")

        self.namespace["LumaUnavailableError"] = bindings.LumaUnavailableError
        return warnings

    # -- bindings -------------------------------------------------------

    def install_luma(self, manifest_rel: str | None) -> None:
        """(Re)install `luma` into the namespace, even if the user clobbered it."""
        if manifest_rel:
            revision = self._manifest_rel_to_revision.get(manifest_rel)
            namespace = self._manifests.get(revision) if revision else None
            if namespace is None:
                namespace = bindings.load_manifest(self.workspace, manifest_rel)
                revision = namespace.revision or manifest_rel
                self._manifests[revision] = namespace
                self._manifest_rel_to_revision[manifest_rel] = revision
            self._current = namespace
        if self._current is not None:
            self.namespace["luma"] = self._current

    # -- execution ------------------------------------------------------

    def run(self, request: dict[str, Any]) -> None:
        request_id = request.get("id")
        started = time.perf_counter()
        warnings: list[str] = []

        status = "ok"
        repr_text: str | None = None
        repr_truncated = False
        traceback_text: str | None = None

        try:
            self.install_luma(request.get("manifest_rel"))
        except Exception as exc:  # noqa: BLE001
            self._emit_streams(request_id)
            self.send(
                {
                    "id": request_id,
                    "type": "result",
                    "status": "error",
                    "repr": None,
                    "traceback": f"{type(exc).__name__}: {exc}",
                    "figures": [],
                    "truncated": {"stdout": False, "stderr": False, "repr": False},
                    "duration_ms": int((time.perf_counter() - started) * 1000),
                    "warnings": ["binding manifest could not be installed"],
                }
            )
            return

        code = request.get("code") or ""
        try:
            body, last_expr = _split_cell(code)
        except SyntaxError as exc:
            self._emit_streams(request_id)
            self.send(
                {
                    "id": request_id,
                    "type": "result",
                    "status": "error",
                    "repr": None,
                    "traceback": "".join(traceback.format_exception_only(type(exc), exc)),
                    "figures": [],
                    "truncated": {"stdout": False, "stderr": False, "repr": False},
                    "duration_ms": int((time.perf_counter() - started) * 1000),
                    "warnings": [],
                }
            )
            return

        value: Any = None
        self._executing = True
        try:
            if body.body:
                exec(compile(body, CELL_FILENAME, "exec"), self.namespace)
            if last_expr is not None:
                value = eval(compile(last_expr, CELL_FILENAME, "eval"), self.namespace)
        except KeyboardInterrupt as exc:
            status = "interrupted"
            traceback_text = _format_traceback(exc)
        except BaseException as exc:  # noqa: BLE001 - a cell must never kill the loop
            status = "error"
            traceback_text = _format_traceback(exc)
        finally:
            self._executing = False

        # Figures first: rendering can print, and the agent should see that.
        figure_list: list[dict[str, Any]] = []
        try:
            figure_list, figure_warnings = figures.collect(self.workspace, self._plt)
            warnings.extend(figure_warnings)
        except Exception as exc:  # noqa: BLE001
            warnings.append(f"figure capture failed: {exc}")

        if status == "ok" and value is not None and not _is_figure(value, self._plt):
            repr_text, repr_truncated = display.render(value)

        stdout_truncated, stderr_truncated = self._emit_streams(request_id)

        self.send(
            {
                "id": request_id,
                "type": "result",
                "status": status,
                "repr": repr_text,
                "traceback": traceback_text,
                "figures": figure_list,
                "truncated": {
                    "stdout": stdout_truncated,
                    "stderr": stderr_truncated,
                    "repr": repr_truncated,
                },
                "duration_ms": int((time.perf_counter() - started) * 1000),
                "warnings": warnings,
            }
        )

    def _emit_streams(self, request_id: Any) -> tuple[bool, bool]:
        """Flush Python buffers + capture pipes and emit `stream` frames."""
        for stream in (sys.stdout, sys.stderr):
            try:
                stream.flush()
            except Exception:  # pragma: no cover
                pass
        self.stdout.sync()
        self.stderr.sync()
        truncated = []
        for capture in (self.stdout, self.stderr):
            text, was_truncated = capture.take()
            truncated.append(was_truncated)
            if text:
                self.send(
                    {
                        "id": request_id,
                        "type": "stream",
                        "stream": capture.name,
                        "text": text,
                    }
                )
        return truncated[0], truncated[1]

    def discard_streams(self) -> None:
        """Drop whatever is buffered (used for preload chatter, which is nobody's)."""
        for stream in (sys.stdout, sys.stderr):
            try:
                stream.flush()
            except Exception:  # pragma: no cover
                pass
        self.stdout.sync()
        self.stderr.sync()
        self.stdout.take()
        self.stderr.take()

    # -- loop -----------------------------------------------------------

    def serve(self, startup_ms: int, warnings: list[str]) -> int:
        self._install_sigint()
        self.send(
            {
                "type": "ready",
                "pid": os.getpid(),
                "python": "%d.%d.%d" % sys.version_info[:3],
                "warnings": warnings,
                "startup_ms": startup_ms,
            }
        )

        while True:
            try:
                line = sys.stdin.readline()
            except KeyboardInterrupt:
                # A SIGINT that arrived between cells must not kill the worker.
                continue
            except Exception:  # pragma: no cover - stdin died
                return 1
            if not line:
                return 0
            line = line.strip()
            if not line:
                continue
            try:
                request = json.loads(line)
            except Exception as exc:  # noqa: BLE001
                self.send(
                    {"id": None, "type": "error", "message": f"bad request line: {exc}"}
                )
                continue

            op = request.get("op")
            try:
                if op == "exec":
                    self.run(request)
                elif op == "ping":
                    self.send(
                        {"id": request.get("id"), "type": "pong", "pid": os.getpid()}
                    )
                elif op == "shutdown":
                    self.send({"id": request.get("id"), "type": "goodbye"})
                    return 0
                else:
                    self.send(
                        {
                            "id": request.get("id"),
                            "type": "error",
                            "message": f"unknown op {op!r}",
                        }
                    )
            except KeyboardInterrupt:
                continue

    def _install_sigint(self) -> None:
        def handler(signum: int, frame: Any) -> None:
            if self._executing:
                raise KeyboardInterrupt
            # Between cells: a late cancellation is dropped on the floor rather
            # than killing the read loop or hitting the next cell.

        try:
            signal.signal(signal.SIGINT, handler)
        except ValueError:  # pragma: no cover - not the main thread
            pass


# ---------------------------------------------------------------------------
# cell helpers
# ---------------------------------------------------------------------------


def _split_cell(code: str) -> tuple[ast.Module, ast.Expression | None]:
    """Split a cell into leading statements and an optional final expression."""
    module = ast.parse(code, filename=CELL_FILENAME, mode="exec")
    last_expr: ast.Expression | None = None
    if module.body and isinstance(module.body[-1], ast.Expr):
        node = module.body.pop()
        last_expr = ast.Expression(body=node.value)
        ast.copy_location(last_expr, node)
    return module, last_expr


def _format_traceback(exc: BaseException) -> str:
    """Format a traceback that starts at the agent's own cell frame."""
    tb = exc.__traceback__
    cursor = tb
    while cursor is not None and cursor.tb_frame.f_code.co_filename != CELL_FILENAME:
        cursor = cursor.tb_next
    text = "".join(traceback.format_exception(type(exc), exc, cursor or tb))
    clamped, _ = display.clamp(text)
    return clamped


def _is_figure(value: Any, plt: Any) -> bool:
    if plt is None:
        return False
    try:
        return isinstance(value, plt.Figure)
    except Exception:  # pragma: no cover
        return False


# ---------------------------------------------------------------------------
# entry point
# ---------------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Luma persistent Python executor")
    parser.add_argument("--workspace", required=True, help="absolute workspace dir")
    args = parser.parse_args(argv)

    started = time.perf_counter()
    worker = Worker(Path(args.workspace))
    warnings = worker.preload()
    # Preload chatter (e.g. library deprecation warnings) belongs to nobody.
    worker.discard_streams()
    startup_ms = int((time.perf_counter() - started) * 1000)
    return worker.serve(startup_ms, warnings)


if __name__ == "__main__":
    raise SystemExit(main())
