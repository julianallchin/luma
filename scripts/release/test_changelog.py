"""changelog.py's ranges and manifest.py's merge of releases' changes.

    python3 -m unittest discover -s scripts/release
"""

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

import changelog  # noqa: E402
import manifest  # noqa: E402


class Repository:
    """A throwaway repository with commits made to order."""

    def __init__(self, path: pathlib.Path):
        self.path = path
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "tag.gpgsign", "false")

    def git(self, *args: str) -> str:
        return changelog.git(*args, cwd=self.path)

    def commit(self, subject: str, *trailers: str) -> str:
        message = subject
        if trailers:
            message += "\n\n" + "\n".join(f"Changelog: {line}" for line in trailers)
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD").strip()

    def lines(self, commit: str, tag: str = "", previous=None) -> list:
        since = changelog.base(commit, tag, previous, cwd=self.path)
        return changelog.lines(commit, since, cwd=self.path)


class ChangelogTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="luma-changelog-test-")
        self.repo = Repository(pathlib.Path(self.directory.name))
        self.repo.commit("start", "Shipped in the first release")
        self.repo.git("tag", "v0.1.0")

    def tearDown(self):
        self.directory.cleanup()

    def manifest_for(self, commit: str) -> pathlib.Path:
        path = pathlib.Path(self.directory.name) / "previous.json"
        path.write_text(json.dumps({"version": "0.1.0-nightly", "commit": commit}))
        return path

    def test_a_stable_release_covers_the_commits_since_the_previous_tag(self):
        self.repo.commit("refactor the mixer")
        self.repo.commit("add low quality", "New Low quality for laptops")
        head = self.repo.commit("add rename", "Right-click a score to rename it")
        self.repo.git("tag", "v0.2.0")
        self.assertEqual(
            self.repo.lines(head, tag="v0.2.0"),
            ["Right-click a score to rename it", "New Low quality for laptops"],
        )

    def test_a_release_candidate_tag_is_not_a_previous_release(self):
        self.repo.commit("add low quality", "New Low quality for laptops")
        self.repo.git("tag", "v0.2.0-rc.1")
        head = self.repo.commit("add rename", "Right-click a score to rename it")
        self.repo.git("tag", "v0.2.0")
        self.assertEqual(
            self.repo.lines(head, tag="v0.2.0"),
            ["Right-click a score to rename it", "New Low quality for laptops"],
        )

    def test_a_line_appears_once_and_trailer_case_does_not_matter(self):
        self.repo.commit("add a", "Clearer sync status")
        self.repo.git("commit", "-q", "--allow-empty", "-m", "add b\n\nchangelog: Clearer sync status")
        head = self.repo.commit("add c", "Members can edit shared venues")
        self.assertEqual(
            self.repo.lines(head, tag="v0.2.0"),
            ["Members can edit shared venues", "Clearer sync status"],
        )

    def test_a_nightly_covers_the_commits_since_the_previous_nightly(self):
        self.repo.commit("add a", "Clearer sync status")
        built = self.repo.commit("add b", "Members can edit shared venues")
        head = self.repo.commit("add c", "Right-click a score to rename it")
        self.assertEqual(
            self.repo.lines(head, previous=self.manifest_for(built)),
            ["Right-click a score to rename it"],
        )

    def test_a_nightly_falls_back_to_the_last_tag(self):
        self.repo.commit("add a", "Clearer sync status")
        head = self.repo.commit("add b", "Members can edit shared venues")
        missing = pathlib.Path(self.directory.name) / "missing.json"
        expected = ["Members can edit shared venues", "Clearer sync status"]
        # No previous manifest, and one naming a commit this one lacks.
        self.assertEqual(self.repo.lines(head, previous=missing), expected)
        self.assertEqual(self.repo.lines(head, previous=self.manifest_for("0" * 40)), expected)

    def test_an_empty_range_has_no_lines(self):
        head = self.repo.commit("add a", "Clearer sync status")
        self.assertEqual(self.repo.lines(head, previous=self.manifest_for(head)), [])


class MergeChangesTest(unittest.TestCase):
    def entry(self, n: int) -> dict:
        return {"version": f"0.{n}.0", "commit": f"c{n}", "lines": [f"line {n}"]}

    def test_this_release_comes_first_and_older_ones_follow(self):
        previous = {"changes": [self.entry(2), self.entry(1)]}
        merged = manifest.merge_changes(self.entry(3), previous)
        self.assertEqual([change["version"] for change in merged], ["0.3.0", "0.2.0", "0.1.0"])

    def test_only_the_last_releases_are_kept(self):
        previous = {"changes": [self.entry(n) for n in range(20, 0, -1)]}
        merged = manifest.merge_changes(self.entry(21), previous, keep=10)
        self.assertEqual(len(merged), 10)
        self.assertEqual(merged[0]["version"], "0.21.0")
        self.assertEqual(merged[-1]["version"], "0.12.0")

    def test_a_rerun_replaces_its_earlier_build(self):
        rerun = dict(self.entry(2), lines=["line 2, again"])
        merged = manifest.merge_changes(rerun, {"changes": [self.entry(2), self.entry(1)]})
        self.assertEqual(merged, [rerun, self.entry(1)])

    def test_a_previous_manifest_without_changes_adds_nothing(self):
        for previous in (None, {}, {"notes": "https://example.com/releases/tag/v0.1.0"}, []):
            self.assertEqual(manifest.merge_changes(self.entry(1), previous), [self.entry(1)])


if __name__ == "__main__":
    unittest.main()
