# Contributing to Luma

Thanks for your interest in contributing to Luma! Here's how to get started.

## Getting Started

1. Fork the repository
2. Create a feature branch (`git checkout -b my-feature`)
3. Make your changes
4. Run checks: `cargo +1.97.1 check --manifest-path gpui/Cargo.toml --workspace --all-targets`
5. Commit and push your branch
6. Open a pull request against `main`

## Code Style

- **Rust**: Formatted by `cargo fmt`, linted by `cargo clippy`
- Git LFS hooks live in `.githooks/`; enable them with `git config core.hooksPath .githooks`.
- Format only Rust files you changed; this checkout may be shared with other contributors.

## Contributor License Agreement

By submitting a pull request or otherwise contributing to this project, you agree that:

1. Your contribution is submitted under the terms of the [PolyForm Shield License 1.0.0](LICENSE.md).
2. You grant Julian Allchin a perpetual, worldwide, non-exclusive, royalty-free, irrevocable license to use, reproduce, modify, distribute, sublicense, and relicense your contribution, including under different license terms.
3. You represent that you have the right to grant this license (i.e., the contribution is your original work, or you have permission from the copyright holder).

This CLA ensures the project can evolve its licensing in the future if needed, while your contributions remain credited and available under the current license.
