# Vendored GPUI snapshot

This directory contains the 23-package GPUI dependency closure used by Luma.
It is a `cargo vendor`-normalized snapshot of Zed base `32a0e813`, with the
following local commits applied:

- `c4e9293096`: backdrop blur, horizontal edge fade, and macOS glass fixes
- `2b23084c75`: cached Gaussian kernels for filtered dialog content

The workspace keeps its upstream Git dependency declarations so
`gpui-component` and Luma resolve one GPUI type identity; `[patch]` redirects
the three public roots into this closed path graph. To refresh the snapshot,
check out the recorded fork revision, vendor the GPUI package closure into a
scratch directory, preserve the upstream `crates/*` and `tooling/perf`
hierarchy, then replace this directory and verify with:

```sh
CARGO_NET_OFFLINE=true cargo metadata --locked
CARGO_NET_OFFLINE=true cargo check -p luma-ui --locked
```

Upstream license files and Cargo package metadata are preserved alongside the
source. `zlog`, `ztracing`, and `ztracing_macro` declare GPL-3.0-or-later; this
was already part of the remote GPUI dependency graph and must remain in the
application's distribution-license review.
