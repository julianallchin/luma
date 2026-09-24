//! Typed, composable pattern graphs. No database, UI, or playback-device state.
//! A frame is evaluated in musical time over resolved independently controllable cells.
pub mod aim;
mod blend;
mod catalog;
pub mod circle_fit;
mod clip_range;
mod clock;
mod color;
mod edit;
mod envelope;
mod event_tensor;
mod features;
mod field_ops;
mod forms;
mod graph;
mod inference;
mod mapping;
mod metrics;
pub mod oklab;
mod output;
mod point_fields;
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

pub use aim::{blend_aim, Aim};
pub use blend::{blend_light, blend_value, BlendMode};
pub use catalog::standard_library;
pub use clock::*;
pub use color::{ColorStop, Gradient};
pub use edit::GraphEdit;
pub use envelope::{Envelope, EnvelopeCurve};
pub use event_tensor::{EventTimes, Events};
pub use features::*;
mod event_targets;
pub use event_targets::EventTargets;
mod event_timing;
pub use event_timing::TrackTiming;
pub use field_ops::FieldMath;
pub use forms::{
    axis_presets, input_order, is_form, palette_steps, path_presets, replace_only, shape_presets,
    steps_path, FORMS, MAX_WIDTH,
};
pub use graph::*;
pub use mapping::*;
pub use metrics::FieldReduction;
pub use output::*;
pub use prepared::*;
pub use presets::*;
pub use runtime::{EvaluatedValue, LightingSignal};
pub use score::*;
pub use selection::*;
pub use signals::UnaryMath;
pub use sources::*;
pub use spatial::*;
pub use tensor::{Channels, Signal, SignalType, Unit};
pub use value::*;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;
