# Genre model — provisioning and licensing

The `genre` preprocessor classifies every bar into the 400-style Discogs
taxonomy using **Discogs-EffNet** from the Essentia / MTG-UPF model zoo. Unlike
every other model in Luma, its weights are **not bundled and not downloaded**.
You install them by hand.

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

This is why nothing is committed and nothing is auto-downloaded: Luma never
redistributes the weights, so the license question stays entirely between the
user and MTG. **Before shipping genre detection in a commercial build, obtain
the proprietary license from MTG.** Until then, treat this node as dev-only.

## Installing the weights

1. Download **Discogs-Effnet** in ONNX, dynamic-batch form from
   <https://essentia.upf.edu/models.html>. The file is
   `discogs-effnet-bsdynamic-1.onnx` (~18 MB) and emits both the 400-style
   activations and a 1280-d embedding; Luma uses only the activations.
2. Drop it in the app config `models` directory:
   - macOS: `~/Library/Application Support/com.luma.luma/models/`
   - Linux: `~/.config/com.luma.luma/models/`
   - Windows: `%APPDATA%\com.luma.luma\models\`
3. Restart. Reconcile-on-startup queues every track for the `genre` node.

Override the location with `LUMA_MODELS_DIR` (the Python worker honours it, and
`genre_worker.rs` sets it from the resolved storage root).

Until the file exists, the preprocessor fails per track with a message naming
the exact path, the failure lands in `preprocessing_failures` under the normal
exponential backoff, and every other node proceeds untouched — a library
analyzed without the model simply has no genre rows.

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
