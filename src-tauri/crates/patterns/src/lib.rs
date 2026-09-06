//! Typed, composable pattern graphs. No database, UI, or playback-device state.
//! A frame is evaluated in musical time over resolved independently controllable cells.
mod blend;
mod catalog;
pub mod circle_fit;
mod clock;
mod color;
mod edit;
mod features;
mod field_ops;
mod graph;
mod mapping;
mod metrics;
mod output;
mod prepared;
mod recipes;
mod score;
mod selection;
mod signals;
mod spatial;
mod spatial_recipes;
mod value;

pub use blend::{blend_color, blend_value, BlendMode};
pub use catalog::standard_library;
pub use clock::*;
pub use color::{ColorStop, Gradient};
pub use edit::GraphEdit;
pub use features::*;
pub use field_ops::{FieldMath, ScalarKind};
pub use graph::*;
pub use mapping::*;
pub use metrics::FieldReduction;
pub use output::*;
pub use prepared::*;
pub use score::*;
pub use selection::*;
pub use signals::UnaryMath;
pub use spatial::*;
pub use value::*;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;
