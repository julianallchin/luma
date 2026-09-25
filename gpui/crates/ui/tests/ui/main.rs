//! The `luma-ui` integration tests, as one binary: the vendored compositor's
//! code compiled as it is and run on a real GPU.
//!
//! `cargo test -p luma-ui --test ui <filter>`.
#![cfg(target_os = "linux")]

mod backdrop;
mod hdr;
