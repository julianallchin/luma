# Genre model — provisioning

The `genre` preprocessor classifies every bar into the 400-style Discogs
taxonomy using **Discogs-EffNet** from the Essentia / MTG-UPF model zoo. Its
weights are **not bundled**: they are downloaded on first use, directly from
MTG's server, checksum-pinned (`genre::process::ensure_model`).

The weights are MTG-UPF's, published under CC BY-NC-ND 4.0
(<https://essentia.upf.edu/licensing_information.html>). Luma fetches the file
from MTG's server and does not re-host it.

## How the weights arrive

On the first `genre` run, `genre::process::ensure_model` downloads
`discogs-effnet-bsdynamic-1.onnx` (~18 MB) from

    https://essentia.upf.edu/models/music-style-classification/discogs-effnet/

into the app config `models` directory:

- macOS: `~/Library/Application Support/com.luma.luma/models/`
- Linux: `~/.config/com.luma.luma/models/`

The download is SHA-256-pinned (`MODEL_SHA256` in `preprocessing/workers/genre/process.rs`) and lands
via `.part` + atomic rename, so a crashed or corrupt download can never be
mistaken for the model. If MTG republishes the checkpoint the pin fails closed:
verify the new file and bump the constant deliberately, because the worker's
label-order assertion is only known to hold for the pinned file. A manually
placed file is honored as-is and skips the download. Override the directory
with `LUMA_MODELS_DIR` (the Python worker honours it, and `preprocessing/workers/genre/process.rs`
sets it from the resolved storage root).

If the download fails (offline, checksum drift), the preprocessor fails per
track with a message naming the URL and the manual path, the failure lands in
`preprocessing_failures` under the normal exponential backoff, and every other
node proceeds untouched — a library analyzed offline simply has no genre rows
yet.

## What gets stored

`track_genres.genres_json` holds, per bar, a sparse confidence-descending
top-8; `labels_json` holds the compacted list of style names the track actually
uses, which every `label_index` resolves against. See
`src/preprocessing/workers/genre.rs` and `python/genre_worker.py`.

Bars come from `workers::track_bar_boundaries` — the same function the bar
classifier uses — so `features.bars[i]` and `features.genres[i]` describe the
same audio.

## Tests

The patch→bar aggregation and the 5-bar median smoother are covered without the
weights:

```
python3 backend/python/tests/test_genre_worker.py
```
