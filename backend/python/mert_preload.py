#!/usr/bin/env python3
"""Pre-fetch MERT-95M weights into HuggingFace cache.

Run at app startup so the classifier preprocessor doesn't pay a ~400 MB
download on first track import. Idempotent — `from_pretrained` reuses the
HF cache if files already exist, so subsequent runs return immediately.
"""

from __future__ import annotations

import sys

MODEL_ID = "m-a-p/MERT-v1-95M"


def main() -> int:
    # Imports inside main so a missing-deps error surfaces with a useful
    # message instead of a top-level ImportError before any logging.
    try:
        from transformers import AutoModel, Wav2Vec2FeatureExtractor
    except ImportError as exc:
        print(f"[mert-preload] transformers not installed: {exc}", file=sys.stderr, flush=True)
        return 1

    print(f"[mert-preload] ensuring {MODEL_ID} weights cached…", file=sys.stderr, flush=True)
    AutoModel.from_pretrained(MODEL_ID, trust_remote_code=True)
    Wav2Vec2FeatureExtractor.from_pretrained(MODEL_ID, trust_remote_code=True)
    print(f"[mert-preload] {MODEL_ID} ready", file=sys.stderr, flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
