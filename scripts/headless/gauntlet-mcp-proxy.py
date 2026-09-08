"""Transparent stdio recorder: unmodified MCP frames reach client and server."""
import base64
import hashlib
import json
import os
import pathlib
import select
import subprocess
import sys
import threading
import time
import uuid


def main():
    evidence = pathlib.Path(sys.argv[1])
    evidence.mkdir(parents=True, exist_ok=True)
    images = evidence / "images"
    images.mkdir(exist_ok=True)
    lock = threading.Lock()
    connection_id = str(uuid.uuid4())
    trace = (evidence / "mcp.jsonl").open("a")

    def record(direction, line):
        with lock:
            try:
                frame = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                frame = {"malformed": line.decode(errors="replace")}
            trace.write(json.dumps({"timeNs": time.time_ns(), "connectionId": connection_id, "direction": direction, "frame": frame}) + "\n")
            trace.flush()
            if direction == "response":
                for block in frame.get("result", {}).get("content", []):
                    if block.get("type") == "image" and block.get("data"):
                        try:
                            data = base64.b64decode(block["data"], validate=True)
                        except ValueError:
                            # The original invalid block still goes to the client and
                            # stays in the trace; recording must not hide a tool defect.
                            continue
                        suffix = {"image/png": "png", "image/jpeg": "jpg", "image/webp": "webp"}.get(block.get("mimeType"), "bin")
                        name = hashlib.sha256(data).hexdigest()
                        (images / f"{name}.{suffix}").write_bytes(data)

    with (evidence / "mcp.stderr.log").open("ab") as errors:
        child = subprocess.Popen(sys.argv[2:], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        stopped = threading.Event()

        def requests():
            pending = b""
            try:
                while not stopped.is_set():
                    if not select.select([sys.stdin.fileno()], [], [], 0.1)[0]:
                        continue
                    chunk = os.read(sys.stdin.fileno(), 65536)
                    if not chunk:
                        break
                    pending += chunk
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        line += b"\n"
                        record("request", line)
                        child.stdin.write(line)
                        child.stdin.flush()
            except BrokenPipeError:
                pass
            finally:
                child.stdin.close()

        reader = threading.Thread(target=requests)
        reader.start()
        try:
            for line in child.stdout:
                record("response", line)
                sys.stdout.buffer.write(line)
                sys.stdout.buffer.flush()
        finally:
            stopped.set()
            reader.join(timeout=1)
            if child.poll() is None:
                child.terminate()
            child.wait()
            record("exit", json.dumps({"exitCode": child.returncode}).encode())
            trace.close()
        return child.returncode


if __name__ == "__main__":
    sys.exit(main())
