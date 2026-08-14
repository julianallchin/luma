# Genre model — provisioning and licensing

The `genre` preprocessor classifies every bar into the 400-style Discogs
taxonomy using **Discogs-EffNet** from the Essentia / MTG-UPF model zoo. Unlike
every other model in Luma, its weights are **not bundled**: they are downloaded
on first use, directly from MTG's server, checksum-pinned
(`genre_worker::ensure_model`).

## Licensing (read this first)

> All the models are available under the **CC BY-NC-ND 4.0** license for
> non-commercial use, and are also available under a proprietary license upon
> request.
>
> — <https://essentia.upf.edu/licensing_information.html>

That is Attribution + **NonCommercial** + **NoDerivatives**, which is stricter
than it first looks:

- **NC** — a paid or commercially distributed Luma build cannot ship these
  weights. Local and development use is fine.
- **ND** — redistributing a modified or converted copy is not permitted. Note
  the ONNX file itself is a conversion of the upstream TensorFlow model, so
  even re-hosting the converted file is legally awkward.
- **BY** — attribute MTG-UPF wherever genre output is surfaced to users.

Why auto-download is acceptable: the ONNX file at MTG's URL is **their own
hosted conversion** (the checksum pin below matches their server byte for
byte), and Luma fetches from the source rather than re-hosting — so Luma never
redistributes the weights, and the ND clause is not tripped. The NC clause is
carried by Luma being internal. **Before shipping genre detection in a
commercial build, obtain the proprietary license from MTG.**

## How the weights arrive

On the first `genre` run, `genre_worker::ensure_model` downloads
`discogs-effnet-bsdynamic-1.onnx` (~18 MB) from

    https://essentia.upf.edu/models/music-style-classification/discogs-effnet/

into the app config `models` directory:

- macOS: `~/Library/Application Support/com.luma.luma/models/`
- Linux: `~/.config/com.luma.luma/models/`
- Windows: `%APPDATA%\com.luma.luma\models\`

The download is SHA-256-pinned (`MODEL_SHA256` in `genre_worker.rs`) and lands
via `.part` + atomic rename, so a crashed or corrupt download can never be
mistaken for the model. If MTG republishes the checkpoint the pin fails closed:
verify the new file and bump the constant deliberately, because the worker's
label-order assertion is only known to hold for the pinned file. A manually
placed file is honored as-is and skips the download. Override the directory
with `LUMA_MODELS_DIR` (the Python worker honours it, and `genre_worker.rs`
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

Bars come from `workers::build_bar_boundaries` — the same function the bar
classifier uses — so `features.bars[i]` and `features.genres[i]` describe the
same audio.

## Tests

The patch→bar aggregation and the 5-bar median smoother are covered without the
weights:

```
python3 src-tauri/python/tests/test_genre_worker.py
```
