"""Focused stdlib tests for worker.py's traceback formatting.

Run directly with either the bundled environment or an ordinary Python::

    python3 backend/python/luma_exec/tests/test_worker.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

PACKAGE_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(PACKAGE_ROOT))

from luma_exec.worker import CELL_FILENAME, _format_traceback  # noqa: E402


def _raise_from_a_real_file():
    """A stand-in for a luma_exec facade frame: a real file, not the cell."""
    raise ValueError("boom from a helper frame")


class FormatTracebackTests(unittest.TestCase):
    def test_only_cell_frames_and_the_exception_line_survive(self):
        code = compile(
            "def wrapper():\n    _raise_from_a_real_file()\nwrapper()\n",
            CELL_FILENAME,
            "exec",
        )
        try:
            exec(code, {"_raise_from_a_real_file": _raise_from_a_real_file})
        except ValueError as exc:
            text = _format_traceback(exc)
        else:
            self.fail("expected the cell to raise")
        self.assertIn(CELL_FILENAME, text)
        self.assertIn("ValueError: boom from a helper frame", text)
        # The helper's own file (standing in for score.py/venue.py/worker.py)
        # must not leak, absolute path or otherwise.
        self.assertNotIn(__file__, text)
        self.assertNotIn("test_worker.py", text)

    def test_an_error_raised_entirely_outside_the_cell_still_shows_the_message(self):
        # No <cell> frame at all reaches this point (e.g. a bug in a host
        # capability called off the agent's own call stack). The traceback
        # should still surface the exception itself, just with no frames.
        try:
            _raise_from_a_real_file()
        except ValueError as exc:
            text = _format_traceback(exc)
        else:
            self.fail("expected to raise")
        self.assertIn("ValueError: boom from a helper frame", text)
        self.assertNotIn(__file__, text)
        self.assertNotIn(CELL_FILENAME, text)

    def test_chained_causes_do_not_leak_internal_frames(self):
        code = compile(
            "try:\n"
            "    _raise_from_a_real_file()\n"
            "except ValueError as cause:\n"
            "    raise RuntimeError('wrapped') from cause\n",
            CELL_FILENAME,
            "exec",
        )
        try:
            exec(code, {"_raise_from_a_real_file": _raise_from_a_real_file})
        except RuntimeError as exc:
            text = _format_traceback(exc)
        else:
            self.fail("expected the cell to raise")
        self.assertIn("RuntimeError: wrapped", text)
        self.assertNotIn(__file__, text)
        self.assertNotIn("boom from a helper frame", text)


if __name__ == "__main__":
    unittest.main()
