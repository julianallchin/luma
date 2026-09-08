"""Fail fast on real MCP boot/binding/kernel failures before a paid model turn."""
import argparse
import json
import pathlib
import select
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", required=True)
    parser.add_argument("--venue", required=True)
    parser.add_argument("--track")
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("server", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    server = args.server[1:] if args.server[:1] == ["--"] else args.server
    if not server:
        parser.error("server command is required after --")
    proxy = pathlib.Path(__file__).with_name("gauntlet-mcp-proxy.py")
    child = subprocess.Popen([sys.executable, str(proxy), args.evidence, *server],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    report = {"ok": False}
    next_id = 0

    def request(method, params):
        nonlocal next_id
        next_id += 1
        child.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params}) + "\n")
        child.stdin.flush()
        deadline = time.monotonic() + args.timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([child.stdout], [], [], remaining)[0]:
                raise RuntimeError(f"{method} timed out after {args.timeout}s")
            line = child.stdout.readline()
            if not line:
                raise RuntimeError(f"MCP process closed stdout during {method}")
            frame = json.loads(line)
            if frame.get("id") != next_id:
                continue
            if frame.get("error"):
                raise RuntimeError(f"{method}: {frame['error']}")
            result = frame.get("result", {})
            if result.get("isError"):
                raise RuntimeError(f"{method}: {result}")
            return result

    try:
        request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                               "clientInfo": {"name": "gauntlet-preflight", "version": "1"}})
        child.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        child.stdin.flush()
        request("tools/list", {})
        binding = {"venue_id": args.venue}
        if args.track:
            binding["track_id"] = args.track
        request("tools/call", {"name": "open", "arguments": binding})
        request("tools/call", {"name": "python", "arguments": {"code": "print(luma.venue.describe())"}})
        report["ok"] = True
    except Exception as error:
        report["error"] = str(error)
    finally:
        child.stdin.close()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.terminate()
            child.wait(timeout=5)
        report["proxyExitCode"] = child.returncode
        trace = pathlib.Path(args.evidence) / "mcp.jsonl"
        if trace.exists():
            for line in trace.read_text().splitlines():
                event = json.loads(line)
                if event["direction"] == "exit":
                    report["serverExitCode"] = event["frame"]["exitCode"]
    if child.returncode != 0:
        report["ok"] = False
    print(json.dumps(report))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
