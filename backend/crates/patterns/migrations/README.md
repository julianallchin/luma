# Frozen score migration inputs

`v2-library.json` is the built-in catalog exported from commit
`7cfc6e8faf6ee583e7b50830d1878cbde12a8270` before the signal/event redesign.
It is immutable migration data, not a second evaluator. Decode old references
against this catalog; do not infer their former interfaces from the live picker.

Export: run `serde_json::to_string(&luma_patterns::standard_library())` in an
isolated checkout of that commit. The compact JSON is deterministic (sorted map
keys). No authored score, venue, database or personal information is included.

`capture_v2.rs` generates `tests/fixtures/v2-samples.json`. Copy it into the old
crate's `examples/` directory and run it there. These samples came from the old
engine in an isolated temporary checkout, not the replacement tensor evaluator.
They cover all 18 complete built-ins with defaults and a second set of color,
grid, delay and angle overrides, over nine fixtures and nine sample times.

`v2-event-recipes.json` is part of the v2-to-v3 transformation. It replaces
legacy cyclic Chase/Pulse/Dissolve recipes with event operations while retaining
their exposed argument keys and defaults. It is conversion data, separate from
the live numerical recipes in `src/recipes.json`.

`migration::upgrade_v2` returns a pure v3 candidate. Opening an owned score applies
it through the authored-history seam as one revision; history deserialization
continues to validate the original version and preserve its hash. Read-only
playback converts a copy. Unknown source content fails without modifying it,
and newly introduced catalog names cannot overwrite an authored helper.
