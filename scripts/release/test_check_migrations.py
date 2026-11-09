"""check_migrations.py's contract rule, against the release and the published stable.

    python3 -m unittest discover -s scripts/release
"""

import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

import check_migrations  # noqa: E402

DROP = "-- contract: after 0.6.3\nalter table venues drop column legacy;\n"


class ContractRule(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)

    def migration(self, sql: str) -> pathlib.Path:
        path = pathlib.Path(self.folder.name) / "20261109000000_drop_legacy.sql"
        path.write_text(sql)
        return path

    def test_a_contract_runs_only_in_a_release_past_its_marker(self):
        path = self.migration(DROP)
        self.assertTrue(check_migrations.check(path, (0, 6, 3)))
        self.assertEqual(check_migrations.check(path, (0, 6, 4)), [])

    def test_a_contract_waits_until_its_release_is_published(self):
        # dev is bumped to 0.6.4 while 0.6.3 is still a draft: the published
        # stable is 0.6.2 and still uses what the contract drops.
        path = self.migration(DROP)
        self.assertTrue(check_migrations.check(path, (0, 6, 4), published=(0, 6, 2)))
        self.assertEqual(check_migrations.check(path, (0, 6, 4), published=(0, 6, 3)), [])

    def test_a_published_bound_does_not_lift_the_release_bound(self):
        path = self.migration(DROP)
        self.assertTrue(check_migrations.check(path, (0, 6, 3), published=(0, 6, 3)))

    def test_an_additive_migration_needs_no_marker(self):
        path = self.migration("alter table venues add column notes text;\n")
        self.assertEqual(check_migrations.check(path, (0, 6, 4), published=(0, 6, 2)), [])

    def test_taking_away_without_a_marker_fails(self):
        path = self.migration("alter table venues drop column legacy;\n")
        self.assertTrue(check_migrations.check(path, (0, 6, 4), published=(0, 6, 4)))


if __name__ == "__main__":
    unittest.main()
