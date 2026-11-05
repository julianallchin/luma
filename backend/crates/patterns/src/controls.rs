//! Live inputs: the values 0–1 a performer plays while a graph plays, from
//! its fader and button nodes ([`crate::clip_graph::LiveInput`]).
//!
//! A frame stays a pure function of its inputs, the current live values
//! among them. A graph reads them through a [`ControlSource`] bound with
//! [`crate::PreparedGraph::with_controls`]; with none bound, every one reads
//! 0. The graph names each input by its node; the host decides what that
//! node reads: a venue's control by the node's name, or a value of the
//! pattern the graph belongs to.
use crate::*;
use std::collections::BTreeMap;

/// Where a prepared graph reads its fader and button nodes.
pub trait ControlSource: std::fmt::Debug + Send + Sync {
    /// The input's value now, 0–1. A value the source does not know reads
    /// its own default, which the source chooses.
    fn value(&self, input: &crate::clip_graph::LiveInput) -> f64;
}

/// The kernel library's `control`: the value of the graph's live input at
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
