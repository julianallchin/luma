#!/usr/bin/env python3
"""Check Supabase migrations against the expand/contract rule.

    check_migrations.py supabase/migrations/<file>.sql ...

Each file must be named `<14-digit version>_<snake_case name>.sql` with a
version no other file in its folder uses: `supabase db push` silently skips a
file named otherwise. It pushes every pending file over one connection, so a
`pg_temp` helper must be created with `create or replace`: the next file may
create one of the same name.

A migration that takes something away from a client (drops a table or
column, deletes or truncates rows, renames, changes a column's type, adds a
NOT NULL) must say which release stopped using it:

    -- contract: after 0.6.3

It may only run in a release newer than that one, so no install still on
0.6.3 or older can meet a server without the thing it uses. This script fails
when a listed file takes something away without the marker, or carries a
marker its release is not past yet. The release is backend/Cargo.toml's
version, the one the release workflow ships; a nightly counts as that
version's release.

The release workflow runs it on every migration about to be pushed, and CI
on every migration a pull request adds or changes. See deploy/README.md.
"""

import argparse
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]

MARKER = re.compile(r"^--\s*contract:\s*after\s+(\S+)\s*$", re.MULTILINE | re.IGNORECASE)
VERSION = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
FILE_NAME = re.compile(r"^(\d{14})_[a-z0-9_]+\.sql$")

# What an older client can trip over. Function bodies are left out before
# matching: a function that deletes rows when called deletes nothing when it
# is created.
TAKES_AWAY = re.compile(
    r"""
      \bdrop\s+(?:table|view|materialized\s+view|schema|type|column)\b
    | \bdelete\s+from\b
    | \btruncate\b
    | \brename\b
    | \balter\s+column\s+\S+\s+(?:set\s+data\s+)?type\b
    | \bset\s+not\s+null\b
    """,
    re.IGNORECASE | re.VERBOSE,
)
COMMENT = re.compile(r"--[^\n]*|/\*.*?\*/", re.DOTALL)
TEMP_FUNCTION = re.compile(r"\bcreate\s+function\s+pg_temp\.", re.IGNORECASE)
FUNCTION_BODY = re.compile(r"\bas\s+\$(\w*)\$.*?\$\1\$", re.DOTALL | re.IGNORECASE)


def parse_version(text: str):
    match = VERSION.match(text)
    return tuple(int(part) for part in match.groups()) if match else None


def crate_version() -> str:
    cargo = (REPO / "backend/Cargo.toml").read_text()
    return re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.MULTILINE).group(1)


def check(path: pathlib.Path, release: tuple) -> list[str]:
    name = FILE_NAME.match(path.name)
    if name is None:
        return ["not named <14-digit version>_<snake_case name>.sql, so `supabase db push` would skip it"]
    twins = [other.name for other in path.parent.glob(name.group(1) + "_*") if other != path]
    if twins:
        return [f"shares its version with {', '.join(twins)}"]

    sql = path.read_text()
    markers = MARKER.findall(sql)
    code = FUNCTION_BODY.sub("", COMMENT.sub("", sql))
    if TEMP_FUNCTION.search(code):
        return ["creates a pg_temp function without `or replace`; a later file pushed over the same connection may create it again"]
    taken = sorted({" ".join(match.group(0).lower().split()) for match in TAKES_AWAY.finditer(code)})

    if len(markers) > 1:
        return [f"{len(markers)} contract markers; keep one"]
    if not markers:
        if taken:
            return [
                f"takes something away ({', '.join(taken)}) but has no "
                "`-- contract: after <version>` naming the release whose clients stopped using it"
            ]
        return []
    after = parse_version(markers[0])
    if after is None:
        return [f"contract marker `{markers[0]}` is not a version like 0.6.3"]
    if release <= after:
        shown = ".".join(map(str, release))
        return [
            f"contract: after {markers[0]}, but this release is {shown}: it may only run "
            f"in a release newer than {markers[0]}"
        ]
    return []


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("files", nargs="*", type=pathlib.Path)
    parser.add_argument("--version", help="the release version (default: backend/Cargo.toml's)")
    args = parser.parse_args()

    shipped = args.version or crate_version()
    # A nightly's version is `0.6.3-nightly.<date>+<sha>`: it ships 0.6.3's clients.
    release = parse_version(shipped.split("-")[0].split("+")[0])
    if release is None:
        print(f"::error::release version {shipped} is not like 0.6.3", file=sys.stderr)
        return 1

    failed = False
    for path in args.files:
        for problem in check(path, release):
            failed = True
            print(f"::error file={path}::{path.name}: {problem}", file=sys.stderr)
    if not failed:
        print(f"{len(args.files)} migration(s) fit the contract rule for {shipped}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
