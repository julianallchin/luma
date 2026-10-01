//! One small graph per clip (spec: `docs/specs/clip-graphs.md`).
//!
//! A clip graph has nodes of 13 kinds. Exactly one node is an output
//! (`color`, `aim` or `strobe`). Every node has one output wire. An input
//! holds a value, a wire (`{"node": id}`), or nothing; nothing is the only
//! default. A choice is a setting and is never wired.
//!
//! This module holds the types and their JSON, the node definitions
//! ([`definitions`]), the one type checker ([`check`]) and the one-line
//! summary ([`ClipGraph::summary`]).
mod check;
pub(crate) mod clock_table;
mod definitions;
mod heads;
pub(crate) mod kernels;
mod lower;
mod noise;
mod summary;

pub use check::{blend_modes, check, check_clip};
pub use kernels::Kernel;
pub(crate) use lower::lower;
pub use noise::sample_noise;

/// The output of a prepared clip graph that carries its lighting.
pub const OUTPUT: &str = "lighting";
pub use definitions::{
    definition, definitions, Definition, InputDef, InputType, Produces, SettingDef, Unit,
};

use crate::{Curve, Error, Gradient, Result};
use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The graph JSON version this build reads and writes.
pub const VERSION: u32 = 2;
/// The most nodes one graph may have.
pub const MAX_NODES: usize = 64;

/// The kind of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Clock,
    Time,
    Space,
    Noise,
    Audio,
    Curve,
    Mirror,
    Shuffle,
    Group,
    Split,
    Color,
    Aim,
    Strobe,
}

impl Kind {
    /// Every kind, in menu order.
    pub const ALL: [Kind; 13] = [
        Kind::Clock,
        Kind::Time,
        Kind::Space,
        Kind::Noise,
        Kind::Audio,
        Kind::Curve,
        Kind::Mirror,
        Kind::Shuffle,
        Kind::Group,
        Kind::Split,
        Kind::Color,
        Kind::Aim,
        Kind::Strobe,
    ];

    /// The stored spelling, which is also the node id prefix.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Clock => "clock",
            Kind::Time => "time",
            Kind::Space => "space",
            Kind::Noise => "noise",
            Kind::Audio => "audio",
            Kind::Curve => "curve",
            Kind::Mirror => "mirror",
            Kind::Shuffle => "shuffle",
            Kind::Group => "group",
            Kind::Split => "split",
            Kind::Color => "color",
            Kind::Aim => "aim",
            Kind::Strobe => "strobe",
        }
    }

    /// The kind in sentence case, as a card title shows it ("Curve").
    pub fn label(self) -> &'static str {
        match self {
            Kind::Clock => "Clock",
            Kind::Time => "Time",
            Kind::Space => "Space",
            Kind::Noise => "Noise",
            Kind::Audio => "Audio",
            Kind::Curve => "Curve",
            Kind::Mirror => "Mirror",
            Kind::Shuffle => "Shuffle",
            Kind::Group => "Group",
            Kind::Split => "Split",
            Kind::Color => "Color",
            Kind::Aim => "Aim",
            Kind::Strobe => "Strobe",
        }
    }

    /// Whether this kind is an output node: `color`, `aim` or `strobe`.
    pub fn is_output(self) -> bool {
        matches!(self, Kind::Color | Kind::Aim | Kind::Strobe)
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// A clip's graph: nodes keyed by id (`curve2`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipGraph {
    pub version: u32,
    pub nodes: BTreeMap<String, Node>,
}

impl Default for ClipGraph {
    fn default() -> Self {
        Self {
            version: VERSION,
            nodes: BTreeMap::new(),
        }
    }
}

/// One node. An empty input is absent from `inputs`. A setting that is
/// absent reads as its default ([`Node::setting`]); writers still store
/// every setting ([`Node::new`] fills them).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawNode")]
pub struct Node {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, Input>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    kind: Kind,
    #[serde(default)]
    settings: BTreeMap<String, String>,
    #[serde(default)]
    inputs: BTreeMap<String, Input>,
}

impl From<RawNode> for Node {
    /// A bare 3-array reads as a vector; the definition turns it into a
    /// color where the input is a color, and into a list of three numbers
    /// where the input takes a list.
    fn from(raw: RawNode) -> Self {
        let definition = definition(raw.kind);
        let inputs = raw
            .inputs
            .into_iter()
            .map(|(name, input)| {
                let def = definition.input(&name);
                let input = match (def.map(|def| def.ty), input) {
                    (Some(InputType::Color), Input::Vector(rgb)) => Input::Color(rgb),
                    (Some(InputType::Number), Input::Vector(items))
                        if def.is_some_and(InputDef::takes_list) =>
                    {
                        Input::List(items.into_iter().map(Input::Number).collect())
                    }
                    (_, input) => input,
                };
                (name, input)
            })
            .collect();
        Node {
            kind: raw.kind,
            settings: raw.settings,
            inputs,
        }
    }
}

impl Node {
    /// A node of `kind` with every setting at its default and every input
    /// empty.
    pub fn new(kind: Kind) -> Self {
        Node {
            kind,
            settings: definition(kind)
                .settings
                .iter()
                .map(|(name, setting)| (name.to_string(), setting.default.to_string()))
                .collect(),
            inputs: BTreeMap::new(),
        }
    }

    pub fn with_input(mut self, name: &str, input: impl Into<Input>) -> Self {
        self.inputs.insert(name.into(), input.into());
        self
    }

    /// Sets `name`. Setting `space.kind` also sets `wrap` to that kind's
    /// default, so a caller that wants another wrap sets it after the kind.
    pub fn with_setting(mut self, name: &str, value: &str) -> Self {
        self.settings.insert(name.into(), value.into());
        if self.kind == Kind::Space && name == "kind" {
            let wrap = if value == "angle" { "yes" } else { "no" };
            self.settings.insert("wrap".into(), wrap.into());
        }
        self
    }

    /// The setting's stored value, or its default. `space.wrap` defaults
    /// to `yes` for `angle` and `no` otherwise.
    pub fn setting(&self, name: &str) -> Option<&str> {
        if let Some(value) = self.settings.get(name) {
            return Some(value);
        }
        if self.kind == Kind::Space && name == "wrap" {
            return Some(if self.setting("kind") == Some("angle") {
                "yes"
            } else {
                "no"
            });
        }
        definition(self.kind)
            .setting(name)
            .map(|setting| setting.default)
    }

    /// The wires into this node, a list's one by one: (input, source node
    /// id).
    pub fn wires(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inputs
            .iter()
            .flat_map(|(name, input)| input.sources().map(move |source| (name.as_str(), source)))
    }
}

/// What an input holds. JSON: a number, a 3-array (vector or color, by the
/// definition), `{"points": …}`, `{"stops": …}`, `{"node": id}`, or a list
/// of numbers and wires whose values multiply (`[{"node": "curve1"}, 0.5]`).
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Number(f64),
    Vector([f64; 3]),
    Color([f64; 3]),
    Points(Curve<f64>),
    Gradient(Gradient),
    Wire(String),
    /// Numbers and wires, multiplied. Only on a number input with range
    /// 0–1 ([`InputDef::takes_list`]).
    List(Vec<Input>),
}

impl Input {
    pub fn wire(node: impl Into<String>) -> Self {
        Input::Wire(node.into())
    }
    /// The source node id, when this is a wire.
    pub fn source(&self) -> Option<&str> {
        match self {
            Input::Wire(source) => Some(source),
            _ => None,
        }
    }
    /// Every source node id: the wire's, or each wire of a list.
    pub fn sources(&self) -> impl Iterator<Item = &str> {
        let items: &[Input] = match self {
            Input::List(items) => items,
            one => std::slice::from_ref(one),
        };
        items.iter().filter_map(Input::source)
    }
}

impl From<f64> for Input {
    fn from(value: f64) -> Self {
        Input::Number(value)
    }
}
impl From<Curve<f64>> for Input {
    fn from(points: Curve<f64>) -> Self {
        Input::Points(points)
    }
}
impl From<Gradient> for Input {
    fn from(gradient: Gradient) -> Self {
        Input::Gradient(gradient)
    }
}

impl Serialize for Input {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Input::Number(value) => value.serialize(serializer),
            Input::Vector(value) | Input::Color(value) => value.serialize(serializer),
            Input::Points(points) => points.serialize(serializer),
            Input::Gradient(gradient) => gradient.serialize(serializer),
            Input::Wire(node) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("node", node)?;
                map.end()
            }
            Input::List(items) => items.serialize(serializer),
        }
    }
}

const INPUT_SHAPES: &str = r#"a number, [u, v, z], [r, g, b], {"points": [[0, 0], [1, 1]]}, {"stops": [{"t": 0, "color": [0, 0, 0]}]}, {"node": "time1"} or a list such as [{"node": "curve1"}, 0.5]"#;

impl<'de> Deserialize<'de> for Input {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        use serde_json::Value as Json;
        let json = Json::deserialize(deserializer)?;
        let wrong = || de::Error::custom(format!("an input is {INPUT_SHAPES}; got {json}"));
        match &json {
            Json::Number(number) => number.as_f64().map(Input::Number).ok_or_else(wrong),
            Json::Array(items) => {
                let numbers: Vec<f64> = items.iter().filter_map(Json::as_f64).collect();
                if let Ok(triple) = <[f64; 3]>::try_from(numbers) {
                    if items.len() == 3 {
                        return Ok(Input::Vector(triple));
                    }
                }
                items
                    .iter()
                    .map(|item| match item {
                        Json::Number(_) | Json::Object(_) => {
                            match Input::deserialize(item).map_err(de::Error::custom)? {
                                item @ (Input::Number(_) | Input::Wire(_)) => Ok(item),
                                _ => Err(wrong_list(&json)),
                            }
                        }
                        _ => Err(wrong_list(&json)),
                    })
                    .collect::<std::result::Result<_, _>>()
                    .map(Input::List)
            }
            Json::Object(map) if map.contains_key("node") => match (map.len(), &map["node"]) {
                (1, Json::String(node)) => Ok(Input::Wire(node.clone())),
                _ => Err(de::Error::custom(format!(
                    r#"a wire is {{"node": "time1"}} and nothing else; got {json}"#
                ))),
            },
            Json::Object(map) if map.contains_key("points") => serde_json::from_value(json.clone())
                .map(Input::Points)
                .map_err(de::Error::custom),
            Json::Object(map) if map.contains_key("stops") => serde_json::from_value(json.clone())
                .map(Input::Gradient)
                .map_err(de::Error::custom),
            _ => Err(wrong()),
        }
    }
}

fn wrong_list<E: de::Error>(json: &serde_json::Value) -> E {
    E::custom(format!(
        r#"a list holds numbers and wires, such as [{{"node": "curve1"}}, 0.5]; a vector or a color has three numbers, such as [0, 0.766, -0.643]; got {json}"#
    ))
}

impl ClipGraph {
    /// A graph of `nodes`, at the current version.
    pub fn new(nodes: impl IntoIterator<Item = (String, Node)>) -> Self {
        Self {
            version: VERSION,
            nodes: nodes.into_iter().collect(),
        }
    }

    /// Reads a graph. Only the JSON shape is checked here; [`check`] does
    /// the rest.
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|error| Error(format!("graph: {error}")))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a clip graph serializes")
    }

    /// The type check of spec section 3.1, rules 1–5.
    pub fn check(&self) -> Result<()> {
        check(self)
    }

    /// The output node, when there is one. With several (an error the
    /// checker reports), the first by id.
    pub fn output(&self) -> Option<(&str, &Node)> {
        self.nodes
            .iter()
            .find(|(_, node)| node.kind.is_output())
            .map(|(id, node)| (id.as_str(), node))
    }

    /// The output node's kind.
    pub fn output_kind(&self) -> Option<Kind> {
        self.output().map(|(_, node)| node.kind)
    }

    /// A free id for a new node of `kind`: `<kind><n>` with the lowest
    /// unused `n` from 1.
    pub fn next_id(&self, kind: Kind) -> String {
        (1..)
            .map(|n| format!("{}{n}", kind.name()))
            .find(|id| !self.nodes.contains_key(id))
            .expect("a free id")
    }

    /// Where each head of `frame.cells` falls on coordinate node `id` at
    /// `frame.beat`, as playback computes it. With several live events a
    /// head reads the first. An editor reads this to mark the heads along a
    /// curve's x.
    pub fn coordinate_at_heads(
        &self,
        id: &str,
        frame: crate::Frame<'_>,
    ) -> Result<Vec<Option<f64>>> {
        frame.validate()?;
        self.check()?;
        let root = lower::lower_coordinate(self, id, frame)?;
        let prepared = crate::PreparedGraph::lowered(&crate::standard_library(), &root, frame)?;
        let out = prepared.evaluate_batch(&[frame.beat])?;
        let x = out[lower::COORDINATE]
            .signal()
            .ok_or_else(|| Error("a coordinate gives a value per head".into()))?;
        // A per-head signal lists its heads; a broadcast one has one row.
        Ok(frame
            .cells
            .iter()
            .map(|cell| {
                let n = x
                    .fixtures()
                    .map_or(Some(0), |ids| ids.iter().position(|f| f == &cell.id))?;
                Some(x.values()[[n, 0, 0]])
            })
            .collect())
    }

    /// Node ids by their letters, then by their number, so `curve2` comes
    /// before `curve10`.
    pub fn ids_in_order(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.nodes.keys().map(String::as_str).collect();
        ids.sort_by_key(|id| {
            let split = id.find(|c: char| c.is_ascii_digit()).unwrap_or(id.len());
            let (prefix, number) = id.split_at(split);
            (prefix, number.parse::<u64>().unwrap_or(u64::MAX), *id)
        });
        ids
    }
}
