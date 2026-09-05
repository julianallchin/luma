//! Typed, composable pattern graphs. No database, UI, or playback-device state.
//! A frame is evaluated in musical time over resolved independently controllable cells.
mod catalog;
pub mod circle_fit;
mod graph;
mod mapping;
mod score;
mod spatial;
mod value;

pub use catalog::standard_library;
pub use graph::*;
pub use mapping::*;
pub use score::*;
pub use spatial::*;
pub use value::*;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;
