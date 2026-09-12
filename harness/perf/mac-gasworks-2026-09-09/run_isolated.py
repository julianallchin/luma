#!/usr/bin/env python3
"""Run one profiler, optionally suspending a known competing app until it exits.

The process is resumed on success, failure, timeout, Ctrl-C, or SIGTERM.
This removes one named competitor; it does not establish machine-wide idleness.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess


def now():
    return datetime.now(timezone.utc).isoformat()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pause-pid", type=int)
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("a command and a positive timeout are required")
    if args.pause_pid is not None and (
        args.pause_pid < 2 or args.pause_pid in (os.getpid(), os.getppid())
    ):
        parser.error("refusing to suspend the runner or its parent")
    executable = Path(shutil.which(command[0]) or command[0]).resolve(strict=True)
    record = {
        "command": command,
        "executable": str(executable),
        "executable_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
        "started_at_utc": now(),
        "renderer_environment": {k: v for k, v in os.environ.items() if k.startswith((
            "LUMA_HAZE_", "LUMA_FOG_", "LUMA_INTERVAL_", "LUMA_PROFILE_", "LUMA_SURFACE_",
        ))},
        "isolation": "Only the explicitly named process is suspended; other system work may remain.",
    }
    # Claim the evidence path before changing another process's state.
    with args.metadata.open("x") as output:
        json.dump(record, output, indent=2)
    resumed = False
    suspended = False

    def terminate(_signum, _frame):
        raise SystemExit("runner terminated")

    signal.signal(signal.SIGTERM, terminate)
    try:
        if args.pause_pid is not None:
            status = subprocess.check_output(
                ["ps", "-p", str(args.pause_pid), "-o", "stat=,command="], text=True,
            ).strip()
            record["competing_process"] = {"pid": args.pause_pid, "before": status}
            if "T" not in status.split()[0]:
                os.kill(args.pause_pid, signal.SIGSTOP)
                suspended = True
        result = subprocess.run(command, timeout=args.timeout, check=False)
        record["exit_code"] = result.returncode
        if result.returncode:
            raise SystemExit(result.returncode)
    except BaseException as error:
        record["error"] = str(error)
        raise
    finally:
        if suspended:
            try:
                os.kill(args.pause_pid, signal.SIGCONT)
                resumed = True
            except ProcessLookupError:
                record["competing_process_exited"] = True
        record.update(finished_at_utc=now(), suspended=suspended, resumed=resumed)
        args.metadata.write_text(json.dumps(record, indent=2) + "\n")


if __name__ == "__main__":
    main()
