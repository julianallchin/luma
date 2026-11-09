#!/usr/bin/env python3
"""Write the updater manifest, latest.json, for one release.

    manifest.py --version 0.6.0 --commit <sha> --base-url <release download url> \
        --changes changelog.txt --previous previous.json \
        aarch64-apple-darwin=dist/Luma-aarch64.app.tar.gz \
        x86_64-unknown-linux-gnu=dist/luma-linux-x86_64.tar.gz > latest.json

Each archive needs its signature, `<archive>.sig`, beside it. The format is
documented in gpui/crates/update/src/manifest.rs, which reads it.

`--changes` is this release's changelog, one line each, as changelog.py
prints it. `notes` carries those lines as one string, for installs that read
nothing else. `changes` lists them per release for the last KEEP releases:
this one first, then the entries of the channel's previous latest.json
(`--previous`), so an install that skipped releases can show everything
since its own.
"""

import argparse
import datetime
import json
import pathlib
import sys

# How many releases' changes one manifest carries.
KEEP = 10


def merge_changes(entry: dict, previous: object, keep: int = KEEP) -> list:
    """`entry` ahead of the previous manifest's entries, `keep` in all.

    An earlier build of the same commit or version (a rerun) is replaced. A
    previous manifest that is missing, or has no `changes`, adds nothing.
    """
    older = previous.get("changes") if isinstance(previous, dict) else None
    if not isinstance(older, list):
        older = []
    kept = [
        change
        for change in older
        if isinstance(change, dict)
        and change.get("commit") != entry["commit"]
        and change.get("version") != entry["version"]
    ]
    return [entry, *kept][:keep]


def read_previous(path):
    """The previous latest.json, or `None` when there is none to read."""
    if path is None:
        return None
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--base-url", required=True, help="where the archives are downloaded from")
    parser.add_argument("--changes", type=pathlib.Path, help="this release's changelog lines")
    parser.add_argument(
        "--previous", type=pathlib.Path, help="the channel's published latest.json, if any"
    )
    parser.add_argument("assets", nargs="+", metavar="PLATFORM=ARCHIVE")
    args = parser.parse_args()

    platforms = {}
    for asset in args.assets:
        platform, sep, archive = asset.partition("=")
        if not sep:
            parser.error(f"expected PLATFORM=ARCHIVE, got {asset!r}")
        archive = pathlib.Path(archive)
        signature = archive.with_name(archive.name + ".sig")
        if not signature.is_file():
            parser.error(f"{signature} is missing; sign {archive} first")
        platforms[platform] = {
            "signature": signature.read_text().strip(),
            "url": f"{args.base_url.rstrip('/')}/{archive.name}",
        }

    lines = []
    if args.changes is not None:
        lines = [line.strip() for line in args.changes.read_text().splitlines() if line.strip()]
    entry = {"version": args.version, "commit": args.commit, "lines": lines}

    manifest = {
        "version": args.version,
        "notes": "\n".join(f"- {line}" for line in lines),
        "changes": merge_changes(entry, read_previous(args.previous)),
        "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "commit": args.commit,
        "platforms": platforms,
    }
    json.dump(manifest, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
