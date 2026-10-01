//! The kernel graph a clip graph lowers onto: definitions of primitive
//! kernels in a [`Library`], and graphs of kernel nodes wired together.
//! [`crate::PreparedGraph`] flattens one into a batch program.
use crate::{Error, Result, Value, ValueType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rate {
    Fixed,
    Frame,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// Optional terminal sockets are unwritten until bound. Required inputs
    /// use their default during execution.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    pub name: String,
    pub description: String,
    pub value_type: ValueType,
    pub rate: Rate,
    pub default: Option<Value>,
}

/// A frame-rate input with no default.
pub(crate) fn port(name: &str, kind: ValueType, default: Option<Value>) -> Input {
    Input {
        optional: false,
        name: name.into(),
        description: name.into(),
        value_type: kind,
        rate: Rate::Frame,
        default,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub value_type: ValueType,
    pub rate: Rate,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    Value { value: Value },
    Input { input: String },
    Connection { node: String, output: String },
}
impl From<Value> for Binding {
    fn from(value: Value) -> Self {
        Self::Value { value }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Definition ID in the library.
    pub definition: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Binding>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub nodes: BTreeMap<String, Node>,
    pub outputs: BTreeMap<String, Binding>,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    /// A clip graph kernel. Kernels carry prepared tables; authored JSON
    /// cannot construct them.
    #[serde(skip)]
    Kernel(crate::clip_graph::Kernel),
    Output,
    BandEnergy,
}
impl Primitive {
    pub(crate) fn reads_track(self) -> bool {
        self == Self::BandEnergy
    }
    /// All other primitives are pure functions of inputs and the prepared
    /// head domain/seed, and may be folded when their inputs are constant.
    pub(crate) fn reads_time(self) -> bool {
        match self {
            Self::BandEnergy => true,
            Self::Kernel(kernel) => kernel.reads_time(),
            _ => false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Body {
    Primitive(Primitive),
    Graph(Graph),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub inputs: BTreeMap<String, Input>,
    pub outputs: BTreeMap<String, Output>,
    pub body: Body,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    pub definitions: BTreeMap<String, Definition>,
}

pub(crate) fn identity(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(Error("identity must contain 1–256 printable bytes".into()));
    }
    if id.starts_with('@') {
        return Err(Error(
            "identities starting with @ are reserved for editor controls".into(),
        ));
    }
    Ok(())
}
