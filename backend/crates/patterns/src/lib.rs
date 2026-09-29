//! Typed, composable pattern graphs. No database, UI, or playback-device state.
//! A frame is evaluated in musical time over resolved independently controllable cells.
pub mod aim;
mod blend;
mod catalog;
mod clip_range;
mod clock;
mod color;
pub mod color_space;
mod curve;
mod envelope;
mod features;
mod field_ops;
mod forms;
mod graph;
mod inference;
mod mapping;
mod output;
mod prepared;
mod presets;
mod runtime;
mod score;
mod selection;
mod signals;
mod sources;
mod spatial;
mod tensor;
mod value;
mod value_noise;

pub use aim::{blend_aim, offset_aim, Aim, Turn};
pub use blend::{blend_light, blend_value, BlendMode};
pub use catalog::standard_library;
pub use clock::*;
pub use color::{ColorStop, Gradient};
pub use curve::{Curve, CurvePoint, Ease};
pub use envelope::Envelope;
pub use features::*;
pub use field_ops::FieldMath;
pub use forms::{
    axis_presets, blend_modes, input_order, is_form, noise_value, palette_steps, path_presets,
    progress_presets, shape_presets, steps_path, FormKind, NoiseSettings, FORMS, MAX_WIDTH,
};
pub use graph::*;
pub use mapping::*;
pub use output::*;
pub use prepared::*;
pub use presets::*;
pub use runtime::{EvaluatedValue, LightingSignal};
pub use score::*;
pub use selection::*;
pub use sources::*;
pub use spatial::*;
pub use tensor::{Channels, Signal, SignalType, Unit};
pub use value::*;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;
