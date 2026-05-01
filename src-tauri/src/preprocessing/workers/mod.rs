//! Concrete preprocessor implementations.
//!
//! Each module wraps an existing python-call worker (`beat_worker`,
//! `stem_worker`, `root_worker`) in a [`Preprocessor`] impl. The python-call
//! layer is unchanged; this module owns DB persistence + scheduler wiring.

pub mod adtof;
pub mod beat_grid;
pub mod roots;
pub mod stems;
