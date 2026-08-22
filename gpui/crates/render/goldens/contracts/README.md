# Renderer contract goldens

Every PNG in this directory has a same-stem `luma.renderer-frame/1` sidecar
containing its complete scene, camera, render settings, fixture definitions,
clock, output size and fixed subframe count. Regenerate them with:

```sh
cargo run --manifest-path gpui/Cargo.toml --release -p luma-render \
  --bin render-contract-goldens
```

The suite intentionally contains no hard/soft-shadow comparison. The authored
renderer contract currently exposes only a `shadows: bool` toggle; shadow
filter softness is not controllable, so fabricating two labels over the same
pipeline would be false evidence.
