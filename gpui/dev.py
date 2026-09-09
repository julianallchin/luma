#!/usr/bin/env python3
"""Rebuild on save; replace the running app only after a successful build."""

import json
import os
from pathlib import Path
import signal
import subprocess
import time


ROOT = Path(__file__).resolve().parent.parent
WATCH_PATHS = ["gpui", "backend", "resources", ".cargo", "rust-toolchain.toml"]
POLL_SECONDS = 0.3


def snapshot():
    # Git excludes build outputs and bundled runtimes, and discovers new files.
    paths = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard",
         "--", *WATCH_PATHS], cwd=ROOT,
    ).split(b"\0")
    files = {}
    for raw in paths:
        if not raw:
            continue
        path = ROOT / os.fsdecode(raw)
        if path.suffix == ".md":
            continue
        try:
            stat = path.stat()
        except FileNotFoundError:
            continue
        files[raw] = (stat.st_mtime_ns, stat.st_size)
    return files


def stop(process):
    if process is None:
        return
    if process.poll() is None:
        if os.name == "posix":
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass  # The app may have closed between poll() and the signal.
        else:
            process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            if os.name == "posix":
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            else:
                process.kill()
    process.wait()


def build():
    print("[dev] Building…", flush=True)
    process = subprocess.Popen(
        ["cargo", "+1.97.1", "build", "--manifest-path", "gpui/Cargo.toml",
         "-p", "luma-app", "--bin", "luma-app",
         "--message-format=json-render-diagnostics"],
        cwd=ROOT, stdout=subprocess.PIPE, text=True, start_new_session=True,
    )
    executable = None
    try:
        for line in process.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="", flush=True)
                continue
            if message.get("reason") == "compiler-artifact":
                if message["target"]["name"] == "luma-app" and message.get("executable"):
                    executable = message["executable"]
        if process.wait() == 0 and executable:
            return executable
        print("[dev] Build failed; keeping the current app. Save to retry.", flush=True)
        return None
    finally:
        stop(process)
        process.stdout.close()


def main():
    app = None
    try:
        previous = snapshot()
        pending = True
        changed_at = time.monotonic()
        print("[dev] Watching sources. Ctrl-C stops the watcher and its app.", flush=True)
        while True:
            current = snapshot()
            if current != previous:
                previous = current
                pending = True
                changed_at = time.monotonic()
            if pending and time.monotonic() - changed_at >= POLL_SECONDS:
                executable = build()
                current = snapshot()
                if current != previous:
                    # A save during compilation needs another build before launch.
                    previous = current
                    changed_at = time.monotonic()
                    continue
                pending = False
                if executable:
                    stop(app)
                    app = None
                    print("[dev] Starting app…", flush=True)
                    app = subprocess.Popen([executable], cwd=ROOT, start_new_session=True)
            time.sleep(POLL_SECONDS)
    except KeyboardInterrupt:
        print("\n[dev] Stopping.", flush=True)
    finally:
        stop(app)


if __name__ == "__main__":
    main()
