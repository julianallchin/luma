#!/usr/bin/env python3
"""A failed analysis worker reports its traceback, not only the message.

The app saves a worker's stderr as the step's error, so the traceback is what
says where a failure came from. Each case runs a worker with its heavy
libraries replaced by stand-ins in which one call fails. Needs only numpy.

    python3 backend/python/tests/test_worker_failures.py
"""

import pathlib
import subprocess
import sys
import tempfile
import unittest

WORKERS = pathlib.Path(__file__).resolve().parents[1]

# Runs a worker with `beat_this`, `n2n`, `torch` and `transformers` replaced by
# stand-ins: every attribute and call works and is falsy, except the named
# call, which raises.
#   python -c RUN <failing call> <worker script> <worker args...>
RUN = r"""
import importlib.abc
import importlib.util
import runpy
import sys
import types

failing, script, *args = sys.argv[1:]


class Stand:
    def __init__(self, path):
        self.path = path

    def __getattr__(self, name):
        return Stand(f"{self.path}.{name}")

    def __call__(self, *args, **kwargs):
        if self.path == failing:
            raise RuntimeError(f"planted failure in {failing}")
        return Stand(f"{self.path}()")

    def __bool__(self):
        return False


class Library(types.ModuleType):
    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)
        return Stand(f"{self.__name__}.{name}")


class StandIns(importlib.abc.MetaPathFinder, importlib.abc.Loader):
    def find_spec(self, name, path, target=None):
        if name.split(".")[0] in {"beat_this", "n2n", "torch", "transformers"}:
            return importlib.util.spec_from_loader(name, self, is_package=True)
        return None

    def create_module(self, spec):
        return Library(spec.name)

    def exec_module(self, module):
        pass


sys.meta_path.insert(0, StandIns())
sys.argv = [script, *args]
runpy.run_path(script, run_name="__main__")
"""


class FailedWorkersReportTheirTraceback(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.dir = pathlib.Path(scratch.name)
        self.audio = self.file("song.wav")

    def file(self, name):
        path = self.dir / name
        path.touch()
        return str(path)

    def assert_reports_traceback(self, worker, failing, args, stdin=""):
        script = WORKERS / worker
        result = subprocess.run(
            [sys.executable, "-c", RUN, failing, str(script), *args],
            input=stdin,
            capture_output=True,
            text=True,
            cwd=self.dir,
        )
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("Traceback (most recent call last)", result.stderr)
        self.assertIn(f'{script}", line', result.stderr)
        self.assertIn(f"RuntimeError: planted failure in {failing}", result.stderr)

    def test_beats(self):
        self.assert_reports_traceback(
            "beat_worker.py", "beat_this.preprocessing.load_audio", [self.audio]
        )

    def test_features(self):
        self.assert_reports_traceback(
            "mert_worker.py",
            "transformers.AutoFeatureExtractor.from_pretrained",
            [
                "--fullmix", self.audio,
                "--drum", self.audio,
                "--out-fullmix", str(self.dir / "fullmix.npy"),
                "--out-drum", str(self.dir / "drum.npy"),
            ],
        )

    def test_drums(self):
        self.assert_reports_traceback(
            "n2n_worker.py",
            "n2n.infer.load_checkpoint",
            [self.audio, "--ckpt", self.file("n2n.pt"), "--mert", self.file("mert.npy")],
        )

    def test_sections(self):
        self.assert_reports_traceback(
            "classifier_worker.py",
            "torch.load",
            [self.file("mert.npy"), self.file("classifier.pt")],
            stdin="[[0.0, 2.0]]",
        )


if __name__ == "__main__":
    unittest.main()
