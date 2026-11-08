//! One small graph per clip (spec: `docs/specs/clip-graphs.md`).
//!
//! A clip graph has nodes of 19 kinds. Exactly one node is an output
//! (`color`, `aim`, `strobe` or `laser`). Every node has one output wire. An input
//! holds a value, a wire (`{"node": id}`), or nothing; nothing is the only
//! default. A choice is a setting and is never wired.
//!
//! This module holds the types and their JSON, the node definitions
//! ([`definitions`]), the one type checker ([`check`]) and the one-line
//! summary ([`ClipGraph::summary`]).
mod check;
pub(crate) mod clock_table;
mod definitions;
pub mod grid;
mod heads;
pub(crate) mod kernels;
mod lower;
mod random;
mod summary;

pub use check::{blend_modes, check, check_clip, is_control_name, is_reserved, MAX_ID};
pub use kernels::Kernel;
pub(crate) use lower::lower;
pub use random::sample_random;

/// The output of a prepared clip graph that carries its lighting.
pub const OUTPUT: &str = "lighting";
pub use definitions::{
    definition, definitions, Definition, Geometry, InputDef, InputType, Produces, SettingDef, Unit,
    MAX_ELEMENTS,
};

use crate::{Curve, Error, Gradient, Result};
use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The graph JSON version this build reads and writes.
pub const VERSION: u32 = 3;
/// The most nodes one graph may have.
pub const MAX_NODES: usize = 64;

/// The kind of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Time,
    Space,
    Along,
    Random,
    Audio,
    Control,
    Button,
    Curve,
    Math,
    Value,
    Mirror,
    Shuffle,
    Group,
    Split,
    Figure,
    Color,
    Aim,
    Strobe,
    Laser,
}

impl Kind {
    /// Every kind, in menu order.
    pub const ALL: [Kind; 19] = [
        Kind::Time,
        Kind::Space,
        Kind::Along,
        Kind::Random,
        Kind::Audio,
        Kind::Control,
        Kind::Button,
        Kind::Curve,
        Kind::Math,
        Kind::Value,
        Kind::Mirror,
        Kind::Shuffle,
        Kind::Group,
        Kind::Split,
        Kind::Figure,
        Kind::Color,
        Kind::Aim,
        Kind::Strobe,
        Kind::Laser,
    ];

    /// The stored spelling, which is also the node id prefix.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Time => "time",
            Kind::Space => "space",
            Kind::Along => "along",
            Kind::Random => "random",
            Kind::Audio => "audio",
            Kind::Control => "control",
            Kind::Button => "button",
            Kind::Curve => "curve",
            Kind::Math => "math",
            Kind::Value => "value",
            Kind::Mirror => "mirror",
            Kind::Shuffle => "shuffle",
            Kind::Group => "group",
            Kind::Split => "split",
            Kind::Figure => "figure",
            Kind::Color => "color",
            Kind::Aim => "aim",
            Kind::Strobe => "strobe",
            Kind::Laser => "laser",
        }
    }

    /// The kind in sentence case, as a card title shows it ("Curve"). A
    /// `control` node is a fader to the person playing it.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Time => "Time",
            Kind::Space => "Space",
            Kind::Along => "Along",
            Kind::Random => "Random",
            Kind::Audio => "Audio",
            Kind::Control => "Fader",
            Kind::Button => "Button",
            Kind::Curve => "Curve",
            Kind::Math => "Math",
            Kind::Value => "Value",
            Kind::Mirror => "Mirror",
            Kind::Shuffle => "Shuffle",
            Kind::Group => "Group",
            Kind::Split => "Split",
            Kind::Figure => "Figure",
            Kind::Color => "Color",
            Kind::Aim => "Aim",
            Kind::Strobe => "Strobe",
            Kind::Laser => "Laser",
        }
    }

    /// Whether this kind is an output node: `color`, `aim`, `strobe` or
    /// `laser`.
    pub fn is_output(self) -> bool {
        matches!(self, Kind::Color | Kind::Aim | Kind::Strobe | Kind::Laser)
    }

    /// Whether a performer plays this kind: a `control` (a fader or a live
    /// color) or a `button`. Its value comes from the host's
    /// [`crate::ControlSource`].
    pub fn is_live(self) -> bool {
        matches!(self, Kind::Control | Kind::Button)
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// What a laser clip draws on each projector it selects: its figures, in
/// the order of the laser's `figures`, in the zone with id `zone` (empty:
/// the venue's first zone).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LaserHeads {
    pub figures: Vec<FigureHeads>,
    pub zone: String,
}

/// One figure of a laser clip: `count` elements of `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FigureHeads {
    pub kind: crate::laser::Kind,
    pub count: usize,
    /// A polygon's sides.
    pub sides: usize,
}

impl FigureHeads {
    /// What each element draws.
    pub fn outline(&self) -> crate::laser::Outline {
        self.kind.outline(self.sides)
    }
}

impl LaserHeads {
    /// Elements on each projector: every figure's, figure after figure.
    pub fn count(&self) -> usize {
        self.figures.iter().map(|figure| figure.count).sum()
    }

    /// Element `index` of a projector's as (figure, its index in the
    /// figure), or `None` past the last.
    pub fn place(&self, index: usize) -> Option<(usize, usize)> {
        let mut first = 0;
        for (f, figure) in self.figures.iter().enumerate() {
            if index < first + figure.count {
                return Some((f, index - first));
            }
            first += figure.count;
        }
        None
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
    /// where the input is a math node's values.
    fn from(raw: RawNode) -> Self {
        let definition = definition(raw.kind);
        let inputs = raw
            .inputs
            .into_iter()
            .map(|(name, input)| {
                let def = definition.input(&name);
                let input = match (def.map(|def| def.ty), input) {
                    (Some(InputType::Color), Input::Vector(rgb)) => Input::Color(rgb),
                    (Some(InputType::Values), Input::Vector(items)) => {
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

    /// What a card titles the node: its kind's label, or "Live color" for a
    /// `control` that holds a color.
    pub fn label(&self) -> &'static str {
        if self.is_live_color() {
            "Live color"
        } else {
            self.kind.label()
        }
    }

    /// Whether this is a `control` node that holds a color, a live color,
    /// rather than a number.
    pub fn is_live_color(&self) -> bool {
        self.kind == Kind::Control && self.setting("kind") == Some("color")
    }

    /// What the node's output wire carries: its kind's, except that a live
    /// color gives a value, a color, where a fader gives a coordinate.
    pub fn produces(&self) -> Produces {
        if self.is_live_color() {
            Produces::Value
        } else {
            definition(self.kind).output
        }
    }

    /// How a `button` node plays, from its settings; `None` for any other
    /// node. A setting the checker would refuse reads as its default.
    pub fn press(&self) -> Option<crate::press::Press> {
        use crate::press::{Adsr, Press, PressMode, Shape, Start, TimeUnit};
        if self.kind != Kind::Button {
            return None;
        }
        let plain = Adsr::default();
        let length = |name: &str, default: f64| {
            self.setting(name)
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.)
                .unwrap_or(default)
        };
        Some(Press {
            mode: match self.setting("mode") {
                Some("toggle") => PressMode::Toggle,
                _ => PressMode::Flash,
            },
            start: match self.setting("start") {
                Some("beat") => Start::Beat,
                Some("bar") => Start::Bar,
                _ => Start::Now,
            },
            envelope: Adsr {
                attack: length("attack", plain.attack),
                decay: length("decay", plain.decay),
                sustain: length("sustain", plain.sustain).min(1.),
                release: length("release", plain.release),
                unit: match self.setting("unit") {
                    Some("seconds") => TimeUnit::Seconds,
                    _ => TimeUnit::Beats,
                },
                shape: match self.setting("shape") {
                    Some("linear") => Shape::Linear,
                    Some("punchy") => Shape::Punchy,
                    Some("snap") => Shape::Snap,
                    _ => Shape::Smooth,
                },
            },
        })
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
/// of numbers and wires, a math node's values (`[{"node": "curve1"}, 0.5]`).
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Number(f64),
    Vector([f64; 3]),
    Color([f64; 3]),
    Points(Curve<f64>),
    Gradient(Gradient),
    Wire(String),
    /// Numbers and wires: a math node's values.
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

    /// What a performer can play on this graph, in node order: each fader,
    /// live color and button node, by node id. A Perform pattern's cell
    /// shows one of each, and a live input's place here says where its
    /// values sit among those its kernels read ([`LiveInputKind::width`]).
    pub fn live_inputs(&self) -> Vec<LiveInput> {
        self.nodes
            .iter()
            .filter_map(|(id, node)| {
                let kind = match node.kind {
                    Kind::Control if node.is_live_color() => LiveInputKind::Color,
                    Kind::Control => LiveInputKind::Fader,
                    Kind::Button => LiveInputKind::Button(node.press()?),
                    _ => return None,
                };
                Some(LiveInput {
                    node: id.clone(),
                    name: node.setting("name").unwrap_or(id).to_owned(),
                    kind,
                })
            })
            .collect()
    }

    /// The graph's pad, which turns a Perform pattern on and off: the button
    /// node the output's `alpha` depends on, wired straight in or through
    /// curves and math, or the presses of a time node (`time.on`) it
    /// reads. With several, the first by id, as [`ClipGraph::live_inputs`]
    /// orders them; the others stay buttons of the pattern. `None` when no
    /// button feeds `alpha`.
    pub fn pad(&self) -> Option<LiveInput> {
        let feeding = self.into_alpha(true);
        self.live_inputs().into_iter().find(|input| {
            matches!(input.kind, LiveInputKind::Button(_)) && feeding.contains(input.node.as_str())
        })
    }

    /// Whether the pad's envelope is in the output's `alpha`, so the
    /// pattern is dark while its pad is off. A pad that only a press clock
    /// reads starts events instead, which play out after it is let go.
    pub fn pad_gates(&self) -> bool {
        self.pad()
            .is_some_and(|pad| self.into_alpha(false).contains(pad.node.as_str()))
    }

    /// Every node the output's `alpha` reads, through any number of wires;
    /// with `presses` false, not through a time node's `on`, which reads a
    /// button's presses and not its value.
    fn into_alpha(&self, presses: bool) -> std::collections::BTreeSet<&str> {
        let mut feeding = std::collections::BTreeSet::new();
        let Some((_, output)) = self.output() else {
            return feeding;
        };
        let mut wires: Vec<&str> = output
            .inputs
            .get("alpha")
            .into_iter()
            .flat_map(Input::sources)
            .collect();
        while let Some(id) = wires.pop() {
            if feeding.insert(id) {
                if let Some(node) = self.nodes.get(id) {
                    wires.extend(
                        node.wires()
                            .filter(|(input, _)| presses || *input != "on")
                            .map(|(_, source)| source),
                    );
                }
            }
        }
        feeding
    }

    /// The graph with a pad ([`ClipGraph::pad`]), for a new Perform pattern:
    /// a toggle button node named `pad` that starts on the next beat,
    /// through a ramp curve into the output's `alpha`, times whatever `alpha`
    /// held before. A graph that has a pad already, or no output, is
    /// unchanged.
    #[must_use]
    pub fn with_pad(mut self) -> Self {
        let Some(output) = self.output().map(|(id, _)| id.to_owned()) else {
            return self;
        };
        if self.pad().is_some() {
            return self;
        }
        let button = self.next_id(Kind::Button);
        self.nodes.insert(
            button.clone(),
            Node::new(Kind::Button)
                .with_setting("name", "pad")
                .with_setting("mode", "toggle")
                .with_setting("start", "beat"),
        );
        let curve = self.next_id(Kind::Curve);
        self.nodes.insert(
            curve.clone(),
            Node::new(Kind::Curve).with_input("x", Input::wire(button)),
        );
        let held = self
            .nodes
            .get_mut(&output)
            .and_then(|node| node.inputs.remove("alpha"));
        let alpha = match held {
            None => Input::wire(curve),
            Some(held) => {
                let math = self.next_id(Kind::Math);
                self.nodes.insert(
                    math.clone(),
                    Node::new(Kind::Math)
                        .with_input("values", Input::List(vec![Input::wire(curve), held])),
                );
                Input::wire(math)
            }
        };
        if let Some(node) = self.nodes.get_mut(&output) {
            node.inputs.insert("alpha".into(), alpha);
        }
        self
    }

    /// The names the graph's fader, live color and button nodes go by,
    /// sorted and each once: in a score, the venue controls the graph reads.
    pub fn controls(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .nodes
            .values()
            .filter(|node| node.kind.is_live())
            .filter_map(|node| node.setting("name"))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The output node's kind.
    pub fn output_kind(&self) -> Option<Kind> {
        self.output().map(|(_, node)| node.kind)
    }

    /// A laser graph's figures and zone id, which a host needs to lay out
    /// the clip's heads before it prepares the graph. `None` for any other
    /// output.
    pub fn laser(&self) -> Option<LaserHeads> {
        let (_, node) = self.output().filter(|(_, node)| node.kind == Kind::Laser)?;
        let figures = node
            .inputs
            .get("figures")
            .into_iter()
            .flat_map(Input::sources)
            .filter_map(|id| self.nodes.get(id))
            .map(|figure| {
                let kind = crate::laser::Kind::from_setting(figure.setting("kind"));
                let whole = |name: &str| match figure.inputs.get(name).map(|i| self.resolve(i)) {
                    Some(Input::Number(value)) => Some(*value as usize),
                    _ => None,
                };
                FigureHeads {
                    kind,
                    count: whole("count")
                        .map_or(kind.default_count(), |count| count.clamp(1, MAX_ELEMENTS)),
                    sides: whole("sides").unwrap_or(crate::laser::MIN_SIDES),
                }
            })
            .collect();
        Some(LaserHeads {
            figures,
            zone: node.setting("zone").unwrap_or_default().to_string(),
        })
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
        let prepared = crate::PreparedGraph::lowered(
            &crate::standard_library(),
            &root,
            frame,
            self.live_inputs(),
        )?;
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

    /// What a curve, math or value node, or a live color, gives:
    /// `"number"`, `"vector"` or `"color"`. A math node gives the widest
    /// kind of its values (color over vector over number). A value node
    /// gives what the inputs it feeds ask of it ([`ClipGraph::asked`]), else
    /// what it holds. `None` for any other node.
    pub fn value_kind(&self, id: &str) -> Option<&'static str> {
        fn kind(graph: &ClipGraph, id: &str, depth: usize) -> Option<&'static str> {
            let node = graph.nodes.get(id)?;
            match node.kind {
                Kind::Control if node.is_live_color() => Some("color"),
                Kind::Curve => Some(match node.setting("kind") {
                    Some("vector") => "vector",
                    Some("color") => "color",
                    _ => "number",
                }),
                Kind::Math if depth < MAX_NODES => {
                    let rank = |k: &str| ["number", "vector", "color"].iter().position(|n| *n == k);
                    let widest = node
                        .inputs
                        .get("values")
                        .into_iter()
                        .flat_map(Input::sources)
                        .filter_map(|source| kind(graph, source, depth + 1))
                        .max_by_key(|k| rank(k));
                    Some(widest.unwrap_or("number"))
                }
                Kind::Value => Some(graph.asked(id).first().map(|(_, _, kind)| *kind).unwrap_or(
                    match node.inputs.get("value") {
                        Some(Input::Vector(_)) => "vector",
                        Some(Input::Color(_)) => "color",
                        _ => "number",
                    },
                )),
                _ => None,
            }
        }
        kind(self, id, 0)
    }

    /// What each input a value node feeds asks of it, in id order: (node,
    /// input, kind). A number, vector or color input asks its own type; a
    /// curve's low or high asks its curve's kind. A math node's values ask
    /// nothing, since a number and a vector both go into a product.
    pub fn asked(&self, value: &str) -> Vec<(&str, &str, &'static str)> {
        let mut asked = Vec::new();
        for id in self.ids_in_order() {
            let node = &self.nodes[id];
            for (input, source) in node.wires() {
                if source != value {
                    continue;
                }
                let Some(def) = definition(node.kind).input(input) else {
                    continue;
                };
                let kind = match def.ty {
                    InputType::Number => "number",
                    InputType::Vector => "vector",
                    InputType::Color => "color",
                    InputType::Bound => match node.setting("kind") {
                        Some("vector") => "vector",
                        _ => "number",
                    },
                    _ => continue,
                };
                asked.push((id, input, kind));
            }
        }
        asked
    }

    /// What `input` holds once a wire to a value node is followed: that
    /// node's value. Any other input is itself.
    pub fn resolve<'a>(&'a self, input: &'a Input) -> &'a Input {
        match input {
            Input::Wire(source) => match self.nodes.get(source) {
                Some(node) if node.kind == Kind::Value => node.inputs.get("value").unwrap_or(input),
                _ => input,
            },
            other => other,
        }
    }

    /// The events a time node with `every` or `on` gives, as a key: two
    /// time nodes whose `every` (or `on`) and `duration` inputs are equal
    /// share one set of events. `None` for a time node with neither, or
    /// another node.
    pub fn clock_key(&self, id: &str) -> Option<String> {
        let node = self.nodes.get(id).filter(|node| node.kind == Kind::Time)?;
        let duration = node.inputs.get("duration");
        if let Some(button) = node.inputs.get("on") {
            // A press with no duration lasts until the next.
            let life = duration.map_or_else(|| "next".into(), |d| self.input_key(d));
            return Some(format!("on{}/{life}", self.input_key(button)));
        }
        let every = node.inputs.get("every")?;
        Some(format!(
            "{}/{}",
            self.input_key(every),
            self.input_key(duration.unwrap_or(every))
        ))
    }

    /// Whether time node `id`'s events may overlap: it has a duration of
    /// its own, other than its every. One whose events each last until the
    /// next (an every alone, or presses with no duration) has one live event
    /// at a time, so a wire that follows it carries no event axis and meets
    /// any other clock freely (spec 5.3).
    pub fn overlaps(&self, id: &str) -> bool {
        if self.clock_key(id).is_none() {
            return false;
        }
        let node = &self.nodes[id];
        match (node.inputs.get("duration"), node.inputs.get("every")) {
            (None, _) => false,
            (Some(duration), Some(every)) if !node.inputs.contains_key("on") => {
                self.input_key(duration) != self.input_key(every)
            }
            (Some(_), _) => true,
        }
    }

    /// An input as a part of a clock key: a value node's value counts as
    /// itself, so equal numbers make one key.
    fn input_key(&self, input: &Input) -> String {
        match self.resolve(input) {
            Input::Number(value) => format!("{value:?}"),
            Input::Wire(source) => format!("@{source}"),
            other => format!("{other:?}"),
        }
    }

    /// The name of a clock key in messages: the first time node, by id
    /// order, that gives it.
    pub(crate) fn clock_name(&self, key: &str) -> String {
        self.ids_in_order()
            .into_iter()
            .find(|id| self.clock_key(id).as_deref() == Some(key))
            .unwrap_or(key)
            .to_string()
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

/// A fader, live color or button node of a graph, as a performer sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveInput {
    /// The node's id, which names its value for its pattern and so keeps a
    /// binding through a rename.
    pub node: String,
    /// What the node is called, as the cell labels it.
    pub name: String,
    pub kind: LiveInputKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LiveInputKind {
    Fader,
    /// A live color: linear Rec. 2020 rgb, each channel 0–1.
    Color,
    /// A button, played as its node says.
    Button(crate::press::Press),
}

impl LiveInputKind {
    /// How many numbers its value is: a color's three, else one.
    pub fn width(self) -> usize {
        match self {
            LiveInputKind::Color => 3,
            LiveInputKind::Fader | LiveInputKind::Button(_) => 1,
        }
    }
}
