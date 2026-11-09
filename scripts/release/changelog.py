#!/usr/bin/env python3
"""Print the `Changelog:` lines of the commits a release adds, one per line.

    changelog.py --commit <sha> --tag v0.6.3               # a stable release
    changelog.py --commit <sha> --previous previous.json   # a nightly

A stable release covers the commits since the previous stable tag. A nightly
covers those since the commit the previous nightly was built from, which its
latest.json records; when that file is missing, or names a commit this one
does not descend from, it falls back to the latest stable tag. Lines keep
`git log` order, newest first, and each appears once. The checkout needs the
whole history and its tags.
"""

import argparse
import json
import pathlib
import subprocess
import sys


def git(*args: str, cwd=None) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


def is_ancestor(base: str, commit: str, cwd=None) -> bool:
    return (
        subprocess.run(
            ["git", "merge-base", "--is-ancestor", base, commit],
            cwd=cwd,
            capture_output=True,
        ).returncode
        == 0
    )


def previous_tag(commit: str, exclude: str = "", cwd=None):
    """The newest stable `v*` tag `commit` contains, other than `exclude`."""
    tags = git("tag", "--merged", commit, "--list", "v*", "--sort=-v:refname", cwd=cwd)
    for tag in tags.split():
        # A tag with a suffix (`v0.7.0-rc.1`) is not a stable release.
        if tag != exclude and "-" not in tag:
            return tag
    return None


def previous_commit(manifest: pathlib.Path):
    """The commit a published latest.json was built from, if it says."""
    try:
        commit = json.loads(manifest.read_text()).get("commit")
    except (OSError, ValueError, AttributeError):
        return None
    return commit if isinstance(commit, str) and commit else None


def base(commit: str, tag: str = "", previous=None, cwd=None):
    """Where the release's range starts, or `None` for all of history."""
    if tag:
        return previous_tag(commit, exclude=tag, cwd=cwd)
    if previous is not None:
        built = previous_commit(previous)
        if built and is_ancestor(built, commit, cwd=cwd):
            return built
    return previous_tag(commit, cwd=cwd)


def lines(commit: str, since=None, cwd=None) -> list:
    """Every `Changelog:` trailer in `since..commit`, once, newest first."""
    revision = f"{since}..{commit}" if since else commit
    log = git("log", "--format=%(trailers:key=Changelog,valueonly)", revision, cwd=cwd)
    seen = []
    for line in log.splitlines():
        line = line.strip()
        if line and line not in seen:
            seen.append(line)
    return seen


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--commit", required=True, help="the commit being released")
    parser.add_argument("--tag", default="", help="the stable tag being released")
    parser.add_argument(
        "--previous",
        type=pathlib.Path,
        help="the channel's published latest.json, for a nightly",
    )
    args = parser.parse_args()
    for line in lines(args.commit, base(args.commit, args.tag, args.previous)):
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
