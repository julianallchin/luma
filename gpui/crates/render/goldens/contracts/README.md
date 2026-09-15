# Renderer contract goldens

Every PNG in this directory has a same-stem `luma.renderer-frame/1` sidecar
containing its complete scene, camera, render settings, fixture definitions,
clock, output size and fixed subframe count. Regenerate them with:

```sh
cargo run --manifest-path gpui/Cargo.toml --release -p luma-render \
  --bin render-contract-goldens
```

`sun-shadow-hard` and `sun-shadow-soft` are the same sun scene with
`shadow_softness` set to 0 and 3.
