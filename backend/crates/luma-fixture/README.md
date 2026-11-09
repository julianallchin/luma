# luma-fixture

Reads a `luma-fixture/0` spec (`<id>.luma.json`, from the
[fixtures library](https://github.com/lumashow/fixtures)) and builds what Luma
draws and drives: a node tree of meshes, the joints that move it, the canonical
cells, the sources and the lenses, in housing space. The crate docs (`src/lib.rs`)
define housing space and the one transform into it.

The geometry is a port of the library's Python reference builder
(`tools/reference_builder/build.py` and `nodes.py`). It depends on serde and
glam only, so the backend and the `gpui/` crates can both depend on it, as
they do on `fixture-kinematics`.

## Tests

```sh
cargo test --manifest-path backend/Cargo.toml -p luma-fixture
```

`tests/golden.rs` builds the 50 sample specs in `harness/fixture-specs/` and
checks each one's box, cell count, cell-centre hash and joint pivots against
`harness/goldens/fixture-builds.json`. Regenerate it with
`LUMA_UPDATE_GOLDENS=1`, then review the diff.

## fixture-parity

Holds the port to the reference builder over a fixtures checkout. It runs the
reference builder with the checkout's `.venv` (through `reference_dump.py`,
beside it) the first time and caches its answers in `--reference`; delete that
file when the reference builder changes.

```sh
cargo build --release --manifest-path backend/Cargo.toml -p luma-fixture
nice -n 19 backend/target/release/fixture-parity --fixtures ~/github/fixtures \
    --reference /tmp/fixture-reference.jsonl --report /tmp/fixture-parity.txt
```

`--only a,b` keeps specs whose id contains any of the substrings,
`--shippable` keeps those the library's gate passed, `--jobs N` sets the
reference builder's processes (default 4). It prints pass/fail/error counts
for all specs and for shippable ones, the most common failures, the worst
disagreement among passes and the build times; `--report` adds every failure
with its reasons.

A fixture passes when, posed at rest in housing space, every node's box and
the total box agree to 1 mm or 0.5% of the fixture's size, the cell count and
each cell's emitter count are equal, each cell centre agrees to 1 mm, and each
joint's pivot (1 mm) and axis agree.
