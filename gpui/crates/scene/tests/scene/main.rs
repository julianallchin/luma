//! The `luma-scene` integration tests, as one binary.
//!
//! `cargo test -p luma-scene --test scene <filter>`; a filter such as
//! `venue_poses::` runs one file.

mod goldens;
mod venue_build;
mod venue_poses;
