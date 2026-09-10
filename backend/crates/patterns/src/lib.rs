//! Typed, composable pattern graphs. No database, UI, or playback-device state.
//! A frame is evaluated in musical time over resolved independently controllable cells.
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
mod graph;
mod inference;
mod mapping;
mod metrics;
pub mod migration;
pub mod oklab;
mod output;
mod point_fields;
mod prepared;
mod runtime;
mod score;
mod selection;
mod signals;
mod spatial;
mod tensor;
mod value;
mod value_noise;

pub use blend::{blend_color, blend_value, BlendMode};
pub use catalog::standard_library;
pub use clock::*;
pub use color::{ColorStop, Gradient};
pub use edit::GraphEdit;
pub use envelope::{Envelope, EnvelopeCurve};
pub use event_tensor::{chase_signal, pulse_signal, ChaseShape, EventTimes, Events};
pub use features::*;
mod event_targets;
pub use event_targets::EventTargets;
mod event_timing;
pub use event_timing::TrackTiming;
pub use field_ops::{FieldMath, ScalarKind};
pub use graph::*;
pub use mapping::*;
pub use metrics::FieldReduction;
pub use output::*;
pub use prepared::*;
pub use runtime::{EvaluatedValue, LightingSignal};
pub use score::*;
pub use selection::*;
pub use signals::UnaryMath;
pub use spatial::*;
pub use tensor::{Channels, Signal, SignalType, Unit};
pub use value::*;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;
