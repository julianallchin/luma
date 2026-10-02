//! One definition per node kind (spec section 2): its inputs with type,
//! unit and range, its settings with their options, and the wire it gives.
//! The UI, Python and the checker all read these.
use super::{Input, Kind};
use serde::ser::{SerializeMap, SerializeStruct, Serializer};
use serde::Serialize;

/// The type of an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InputType {
    /// A number, or a wire from a number curve.
    Number,
    /// A vector `[u, v, z]`, or a wire from a vector curve.
    Vector,
    /// A color `[r, g, b]`, or a wire from a color curve.
    Color,
    /// A curve shape. Value only.
    Points,
    /// A gradient. Value only.
    Gradient,
    /// A wire from `mirror`, `shuffle`, `group` or `split`.
    Heads,
    /// A wire from a `time` node: its events.
    Time,
    /// A wire from `time`, `space`, `noise` or `audio`.
    Coordinate,
    /// A math node's items: two or more numbers and value wires.
    Values,
    /// `curve.low` and `curve.high`: a number or a vector by the curve's
    /// kind, in the unit and range of the input the curve feeds.
    Bound,
    /// `value.value`: a number or three numbers. Its type and unit are those
    /// of the inputs the value node feeds. Value only.
    Constant,
}

impl InputType {
    /// Whether the input takes a wire only.
    pub fn is_wire_only(self) -> bool {
        matches!(
            self,
            InputType::Heads | InputType::Time | InputType::Coordinate
        )
    }
    /// Whether the input takes a value only.
    pub fn is_value_only(self) -> bool {
        matches!(
            self,
            InputType::Points | InputType::Gradient | InputType::Constant
        )
    }
}

/// The unit of a number or vector input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Share,
    Beats,
    Degrees,
    Metres,
    Hz,
    Uvz,
    Rgb,
    Heads,
}

impl Unit {
    pub fn name(self) -> &'static str {
        match self {
            Unit::Share => "share",
            Unit::Beats => "beats",
            Unit::Degrees => "degrees",
            Unit::Metres => "metres",
            Unit::Hz => "hz",
            Unit::Uvz => "uvz",
            Unit::Rgb => "rgb",
            Unit::Heads => "heads",
        }
    }
}

/// The three kinds of geometric input every node shares. A node never
/// makes up its own, and the checker validates a geometric input through
/// its kind. `aim.point` alone is in metres: the one place a clip names a
/// spot in the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Geometry {
    /// A direction on the stage, `(u, v, z)` with no unit: only its way
    /// counts, never its length. Never zero.
    Direction,
    /// A selection number: one place along the node's own direction or
    /// ruler, 0–1 of the selection before any fold (per span after a
    /// split).
    Number,
    /// A selection point: `(u, v, z)`, each 0–1 of the selection's box
    /// before any fold (per span after a split); (0.5, 0.5, 0.5) is the
    /// middle.
    Point,
}

impl Geometry {
    /// Why `value` is not a valid vector of this kind, or `None`.
    pub fn refuse(self, value: [f64; 3]) -> Option<&'static str> {
        match self {
            Geometry::Direction if value.iter().all(|c| *c == 0.) => {
                Some("a direction that is not zero")
            }
            Geometry::Point if value.iter().any(|c| !(0. ..=1.).contains(c)) => {
                Some("a point in the selection with each of u, v and z 0–1")
            }
            _ => None,
        }
    }
}

/// What a node's output wire carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Produces {
    Heads,
    Coordinate,
    /// A number, vector or color: by a curve's `kind` setting, the widest
    /// of a math node's values, or what a value node's inputs ask of it.
    Value,
    /// An output node: no wire.
    Output,
}

/// One input of a node.
#[derive(Clone, Debug, PartialEq)]
pub struct InputDef {
    pub ty: InputType,
    pub unit: Option<Unit>,
    /// The value a new node starts with. `None` is empty.
    pub default: Option<Input>,
    /// Inclusive bounds; `None` on a side is unbounded.
    pub range: Option<[Option<f64>; 2]>,
    /// The lower bound is excluded: the value must be above it.
    pub above_min: bool,
    /// A direction, selection number or selection point; `None` for any
    /// other input.
    pub geometry: Option<Geometry>,
    /// The input refuses a wire that varies over heads; it takes a value
    /// or a wire over time only.
    pub time_only: bool,
    /// A Python example value for error messages, such as `0.2`.
    pub example: &'static str,
}

impl InputDef {
    fn new(ty: InputType, unit: Option<Unit>, example: &'static str) -> Self {
        InputDef {
            ty,
            unit,
            default: None,
            range: None,
            above_min: false,
            geometry: None,
            time_only: false,
            example,
        }
    }
    fn range(mut self, min: f64, max: f64) -> Self {
        self.range = Some([Some(min), Some(max)]);
        self
    }
    fn above(mut self, min: f64) -> Self {
        self.range = Some([Some(min), None]);
        self.above_min = true;
        self
    }
    fn at_least(mut self, min: f64) -> Self {
        self.range = Some([Some(min), None]);
        self
    }
    /// A direction: `(u, v, z)`, never zero.
    fn direction(example: &'static str) -> Self {
        InputDef {
            geometry: Some(Geometry::Direction),
            ..InputDef::new(InputType::Vector, Some(Unit::Uvz), example)
        }
    }
    /// A selection number: one place along the node's ruler, 0–1.
    fn selection_number(example: &'static str) -> Self {
        InputDef {
            geometry: Some(Geometry::Number),
            ..InputDef::new(InputType::Number, Some(Unit::Share), example).range(0., 1.)
        }
    }
    /// A selection point, `(u, v, z)` each 0–1.
    fn selection_point(example: &'static str) -> Self {
        InputDef {
            geometry: Some(Geometry::Point),
            ..InputDef::new(InputType::Vector, Some(Unit::Share), example).range(0., 1.)
        }
    }
    /// Whether the input is a direction.
    pub fn is_direction(&self) -> bool {
        self.geometry == Some(Geometry::Direction)
    }
    fn time_only(mut self) -> Self {
        self.time_only = true;
        self
    }

    /// Whether `value` is inside the range.
    pub fn in_range(&self, value: f64) -> bool {
        let Some([min, max]) = self.range else {
            return true;
        };
        let above = match min {
            Some(min) if self.above_min => value > min,
            Some(min) => value >= min,
            None => true,
        };
        above && max.is_none_or(|max| value <= max)
    }
}

impl Serialize for InputDef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_struct("InputDef", 8)?;
        map.serialize_field("type", &self.ty)?;
        if self.unit.is_some() {
            map.serialize_field("unit", &self.unit)?;
        }
        map.serialize_field("default", &self.default)?;
        if matches!(self.ty, InputType::Number) || self.range.is_some() {
            map.serialize_field("range", &self.range)?;
        }
        if self.above_min {
            map.serialize_field("above_min", &true)?;
        }
        if let Some(geometry) = self.geometry {
            map.serialize_field("geometry", &geometry)?;
        }
        if self.time_only {
            map.serialize_field("axes", &["T"])?;
        }
        map.end()
    }
}

/// A setting: one of `options`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SettingDef {
    pub options: &'static [&'static str],
    pub default: &'static str,
}

/// A node kind's definition. Inputs and settings are in display order.
#[derive(Clone, Debug, PartialEq)]
pub struct Definition {
    pub kind: Kind,
    pub output: Produces,
    pub inputs: Vec<(&'static str, InputDef)>,
    pub settings: Vec<(&'static str, SettingDef)>,
}

impl Definition {
    pub fn input(&self, name: &str) -> Option<&InputDef> {
        self.inputs
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, def)| def)
    }
    pub fn setting(&self, name: &str) -> Option<&SettingDef> {
        self.settings
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, def)| def)
    }
}

struct Ordered<'a, T>(&'a [(&'static str, T)]);
impl<T: Serialize> Serialize for Ordered<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl Serialize for Definition {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_struct("Definition", 4)?;
        map.serialize_field("kind", &self.kind)?;
        map.serialize_field("output", &self.output)?;
        map.serialize_field("inputs", &Ordered(&self.inputs))?;
        map.serialize_field("settings", &Ordered(&self.settings))?;
        map.end()
    }
}

/// Every definition, in [`Kind::ALL`] order.
pub fn definitions() -> &'static [Definition] {
    static DEFINITIONS: std::sync::OnceLock<Vec<Definition>> = std::sync::OnceLock::new();
    DEFINITIONS.get_or_init(|| Kind::ALL.into_iter().map(build).collect())
}

/// The definition of `kind`.
pub fn definition(kind: Kind) -> &'static Definition {
    definitions()
        .iter()
        .find(|definition| definition.kind == kind)
        .expect("every kind has a definition")
}

fn build(kind: Kind) -> Definition {
    use InputType as T;
    let number = |unit, example| InputDef::new(T::Number, Some(unit), example);
    let vector = |unit, example| InputDef::new(T::Vector, Some(unit), example);
    let wire = |ty, example| InputDef::new(ty, None, example);
    let share = |example| number(Unit::Share, example).range(0., 1.);
    let heads = || ("heads", wire(T::Heads, "group()"));
    let (output, inputs, settings): (Produces, Vec<(&'static str, InputDef)>, Vec<_>) = match kind {
        Kind::Color => (
            Produces::Output,
            vec![
                (
                    "color",
                    InputDef::new(T::Color, Some(Unit::Rgb), "(1, 1, 1)").range(0., 1.),
                ),
                ("brightness", share("1")),
                ("alpha", share("1")),
            ],
            vec![],
        ),
        Kind::Aim => (
            Produces::Output,
            vec![
                heads(),
                ("direction", InputDef::direction("(0, 0.766, -0.643)")),
                ("point", vector(Unit::Metres, "(0, 3, 0)")),
                ("yaw", number(Unit::Degrees, "0").range(-180., 180.)),
                ("pitch", number(Unit::Degrees, "0").range(-180., 180.)),
                ("alpha", share("1")),
            ],
            vec![(
                "base",
                SettingDef {
                    options: &["direction", "point", "away"],
                    default: "direction",
                },
            )],
        ),
        Kind::Strobe => (
            Produces::Output,
            vec![("rate", share("0.5")), ("alpha", share("1"))],
            vec![],
        ),
        Kind::Time => (
            Produces::Coordinate,
            vec![
                ("every", number(Unit::Beats, "1").above(0.)),
                ("duration", number(Unit::Beats, "2").above(0.)),
                ("delay", number(Unit::Beats, "0.5")),
                ("phase", number(Unit::Degrees, "90")),
            ],
            vec![],
        ),
        Kind::Space => (
            Produces::Coordinate,
            vec![
                heads(),
                ("direction", InputDef::direction("(1, 0, 0)").time_only()),
                (
                    "centre",
                    InputDef::selection_point("(0.3, 0.5, 0.5)").time_only(),
                ),
                ("at", InputDef::selection_number("0.5")),
                ("shift", number(Unit::Share, "0.25")),
                ("scale", number(Unit::Share, "0.5").at_least(0.)),
            ],
            vec![
                (
                    "kind",
                    SettingDef {
                        options: &["line", "order", "radial", "angle"],
                        default: "line",
                    },
                ),
                (
                    "wrap",
                    SettingDef {
                        options: &["no", "yes"],
                        default: "no",
                    },
                ),
            ],
        ),
        Kind::Noise => (
            Produces::Coordinate,
            vec![
                heads(),
                ("period", number(Unit::Beats, "4").above(0.)),
                ("scale", number(Unit::Share, "0.5").above(0.)),
                ("contrast", share("0")),
            ],
            vec![],
        ),
        Kind::Audio => (
            Produces::Coordinate,
            vec![
                (
                    "low_hz",
                    number(Unit::Hz, "60").range(20., 20000.).time_only(),
                ),
                (
                    "high_hz",
                    number(Unit::Hz, "300").range(20., 20000.).time_only(),
                ),
            ],
            vec![],
        ),
        Kind::Math => (
            Produces::Value,
            vec![("values", wire(T::Values, "[curve1, curve2]"))],
            vec![(
                "op",
                SettingDef {
                    options: &["*", "+", "-", "max", "min"],
                    default: "*",
                },
            )],
        ),
        Kind::Value => (
            Produces::Value,
            vec![("value", wire(T::Constant, "0.5"))],
            vec![],
        ),
        Kind::Curve => (
            Produces::Value,
            vec![
                ("x", wire(T::Coordinate, "time()")),
                ("shape", wire(T::Points, "\"Ramp up\"")),
                ("low", wire(T::Bound, "0")),
                ("high", wire(T::Bound, "1")),
                ("gradient", wire(T::Gradient, "\"Rainbow\"")),
            ],
            vec![(
                "kind",
                SettingDef {
                    options: &["number", "vector", "color"],
                    default: "number",
                },
            )],
        ),
        Kind::Mirror => (
            Produces::Heads,
            vec![
                heads(),
                ("direction", InputDef::direction("(1, 0, 0)").time_only()),
                ("at", InputDef::selection_number("0.5").time_only()),
            ],
            vec![],
        ),
        Kind::Shuffle => (
            Produces::Heads,
            vec![heads(), ("time", wire(T::Time, "time(every=1)"))],
            vec![],
        ),
        Kind::Group => (
            Produces::Heads,
            vec![
                ("heads", wire(T::Heads, "split()")),
                ("size", number(Unit::Heads, "2").at_least(1.).time_only()),
            ],
            vec![],
        ),
        Kind::Split => (
            Produces::Heads,
            vec![("heads", wire(T::Heads, "mirror()"))],
            vec![(
                "by",
                SettingDef {
                    options: &["fixture", "group"],
                    default: "fixture",
                },
            )],
        ),
    };
    Definition {
        kind,
        output,
        inputs,
        settings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every geometric input uses one of the shared kinds: a vector is a
    /// direction or a selection point (aim.point alone is metres), every
    /// `direction` is a direction, every `at` a selection number and every
    /// `centre` a selection point.
    #[test]
    fn every_geometric_input_uses_a_shared_kind() {
        for definition in definitions() {
            for (name, def) in &definition.inputs {
                let label = format!("{}.{name}", definition.kind.name());
                if (definition.kind, *name) == (Kind::Aim, "point") {
                    assert_eq!(def.unit, Some(Unit::Metres), "{label}");
                    assert_eq!(def.geometry, None, "{label}");
                    continue;
                }
                if def.ty == InputType::Vector {
                    assert!(
                        def.geometry.is_some(),
                        "{label} is a vector of no shared kind"
                    );
                }
                let expected = match *name {
                    "direction" => Some(Geometry::Direction),
                    "at" => Some(Geometry::Number),
                    "centre" => Some(Geometry::Point),
                    "normal" | "center" | "offset" | "origin" => {
                        panic!("{label}: use direction, at or centre")
                    }
                    _ => None,
                };
                assert_eq!(def.geometry, expected, "{label}");
                match def.geometry {
                    Some(Geometry::Direction) => {
                        assert_eq!(def.ty, InputType::Vector, "{label}");
                        assert_eq!(def.unit, Some(Unit::Uvz), "{label}");
                    }
                    Some(kind) => {
                        let ty = if kind == Geometry::Number {
                            InputType::Number
                        } else {
                            InputType::Vector
                        };
                        assert_eq!(def.ty, ty, "{label}");
                        assert_eq!(def.unit, Some(Unit::Share), "{label}");
                        assert_eq!(def.range, Some([Some(0.), Some(1.)]), "{label}");
                    }
                    None => {}
                }
            }
        }
    }
}
