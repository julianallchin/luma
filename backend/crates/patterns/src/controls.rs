//! Live controls: named values 0–1 that a host sets while a graph plays,
//! from faders, pads, MIDI, OSC or any other input.
//!
//! A frame stays a pure function of its inputs, the current control values
//! among them. A graph reads them through a [`ControlSource`] bound with
//! [`crate::PreparedGraph::with_controls`]; with none bound, every control
//! reads 0.
use crate::*;
use std::collections::BTreeMap;

/// Where a prepared graph reads its controls.
pub trait ControlSource: std::fmt::Debug + Send + Sync {
    /// The control's value now, 0–1. A control the source does not know
    /// reads its own default, which the source chooses.
    fn value(&self, name: &str) -> f64;
}

/// The kernel library's `control`: the value of the graph's control at
/// `index`, the same at every head and time of one evaluation.
pub(crate) fn definition() -> Definition {
    Definition {
        name: "Control".into(),
        inputs: BTreeMap::from([(
            "index".into(),
            Input {
                rate: Rate::Fixed,
                ..crate::graph::port("Control index", ValueType::Number, None)
            },
        )]),
        outputs: BTreeMap::from([(
            "value".into(),
            Output {
                value_type: ValueType::Number,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Primitive(Primitive::Control),
    }
}
