#!/usr/bin/env python3
"""Write the updater manifest, latest.json, for one release.

    manifest.py --version 0.6.0 --commit <sha> --base-url <release download url> \
        darwin-aarch64=dist/Luma-aarch64.app.tar.gz \
        linux-x86_64=dist/luma-linux-x86_64.tar.gz > latest.json

Each archive needs its `<archive>.sig` beside it (`tauri signer sign` writes
that). The format is Tauri's static manifest plus a `commit` field — see
gpui/crates/update/src/manifest.rs, which reads it.
"""

import argparse
import datetime
import json
import pathlib
import sys


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--base-url", required=True, help="where the archives are downloaded from")
    parser.add_argument("--notes", default="")
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

    manifest = {
        "version": args.version,
        "notes": args.notes,
        "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "commit": args.commit,
        "platforms": platforms,
    }
    json.dump(manifest, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
