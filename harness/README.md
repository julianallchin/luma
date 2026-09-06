# Native UI and renderer harness

GPUI component fixtures live in `gpui/crates/harness/src/fixtures.rs` and use the shared controls from `gpui/crates/ui`. Fonts in `harness/fonts/` are embedded by the native app and must remain here.

```sh
cargo +1.97.1 run --manifest-path gpui/Cargo.toml -p luma-gpui-harness -- --list
cargo +1.97.1 run --manifest-path gpui/Cargo.toml -p luma-gpui-harness -- --fixture button
cargo +1.97.1 run --manifest-path gpui/Cargo.toml -p luma-render --bin render-goldens -- single-mover
```

`goldens/` contains numerical contracts and historical reference images consumed by native tests. The former React capture pages and scripts have been removed. To compare existing capture directories, install the local tools with `cd harness && bun install`, then run `bun compare-shots.mjs DIR_A DIR_B`.
