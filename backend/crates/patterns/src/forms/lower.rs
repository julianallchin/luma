//! Source syntax is consumed here, once. Playback runs the normal graph DAG.
use super::{
    geometry::{self, default_axis, salt},
    FormKind, SourceOp,
};
use crate::*;
use ndarray::Array3;
use std::collections::BTreeMap;

#[derive(Clone)]
struct Events {
    owner: String,
    progress: Binding,
    present: Binding,
    index: Binding,
    spacing: Binding,
}
#[derive(Clone)]
struct Stream {
    values: [Binding; 3],
    events: Option<Events>,
}
fn number(v: f64) -> Binding {
    Value::Number(v).into()
}
fn output(node: &str, name: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: name.into(),
    }
}
struct Compiler<'a> {
    library: Library,
    nodes: BTreeMap<String, Node>,
    inputs: &'a BTreeMap<String, Value>,
    cells: Vec<Cell>,
    fixtures: Vec<String>,
    seed: u64,
    clocks: BTreeMap<(String, Vec<usize>), Events>,
}
impl Compiler<'_> {
    fn node(
        &mut self,
        op: SourceOp,
        inputs: Vec<(&str, Binding)>,
        outputs: &[&str],
    ) -> Vec<Binding> {
        let id = format!("source-{}", self.nodes.len());
        let definition = format!("__source/{id}");
        self.library
            .definitions
            .insert(definition.clone(), op.definition());
        self.nodes.insert(
            id.clone(),
            Node {
                definition,
                inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            },
        );
        outputs.iter().map(|k| output(&id, k)).collect()
    }
    fn scalar_node(&mut self, op: SourceOp, inputs: Vec<(&str, Binding)>) -> Binding {
        self.node(op, inputs, &["value"]).remove(0)
    }
    fn field(&self, values: Vec<f64>) -> Result<Binding> {
        if values.is_empty() {
            return Ok(number(0.));
        }
        if values.iter().all(|v| *v == values[0]) {
            return Ok(number(values[0]));
        }
        Ok(Value::Signal(Signal::new(
            Array3::from_shape_vec((values.len(), 1, 1), values).unwrap(),
            Unit::Number,
            Channels::Value,
            Some(self.fixtures.clone().into()),
        )?)
        .into())
    }
    fn latest(&mut self, stream: &Stream, ch: usize, inherited: Option<&Events>) -> Binding {
        if let Some(events) = &stream.events {
            if inherited.is_none_or(|e| e.owner != events.owner) {
                return self.scalar_node(
                    SourceOp::Latest,
                    vec![
                        ("value", stream.values[ch].clone()),
                        ("present", events.present.clone()),
                        ("index", events.index.clone()),
                    ],
                );
            }
        }
        stream.values[ch].clone()
    }
    fn numeric(
        &mut self,
        path: &str,
        value: &Value,
        heads: &[usize],
        events: Option<&Events>,
    ) -> Result<Binding> {
        let stream = self.source(path, value, heads, events, false)?;
        Ok(self.latest(&stream, 0, events))
    }
    fn clock(&mut self, path: &str, value: &Value, heads: &[usize]) -> Result<Binding> {
        let period = self.numeric(path, value, heads, None)?;
        Ok(self.scalar_node(SourceOp::ClockTable, vec![("period", period)]))
    }
    fn events(
        &mut self,
        path: &str,
        value: &Value,
        heads: &[usize],
        inherited: Option<&Events>,
    ) -> Result<Events> {
        let spec = match value {
            Value::Time(t) => t.events.as_ref(),
            Value::Random(r) => r.events.as_ref(),
            _ => None,
        };
        if spec.is_none() {
            if let Some(e) = inherited {
                return Ok(e.clone());
            }
        }
        if let Some(crate::Events::SameAs { same_as }) = spec {
            let leader = &self.inputs[same_as];
            return self.leader_events(same_as, leader, heads, inherited);
        }
        if let Some(e) = inherited.filter(|e| e.owner == path) {
            return Ok(e.clone());
        }
        if let Some(e) = self.clocks.get(&(path.into(), heads.to_vec())) {
            return Ok(e.clone());
        }
        let bindings = if let Some(crate::Events::Own(e)) = spec {
            let every = self.clock(&format!("{path}/every"), &e.every, heads)?;
            let life = if let Some(life) = &e.life {
                self.clock(&format!("{path}/life"), life, heads)?
            } else {
                every.clone()
            };
            self.node(
                SourceOp::Events,
                vec![
                    ("every", every),
                    ("life", life),
                    (
                        "once",
                        number((e.every.scalar_value() == Some(0.)) as u8 as f64),
                    ),
                    (
                        "hold",
                        number(
                            (e.every.scalar_value() == Some(0.) && e.life.is_none()) as u8 as f64,
                        ),
                    ),
                ],
                &["progress", "present", "index", "spacing"],
            )
        } else {
            vec![
                self.scalar_node(SourceOp::ClipProgress, vec![]),
                number(1.),
                number(0.),
                number(1.),
            ]
        };
        let e = Events {
            owner: path.into(),
            progress: bindings[0].clone(),
            present: bindings[1].clone(),
            index: bindings[2].clone(),
            spacing: bindings[3].clone(),
        };
        self.clocks.insert((path.into(), heads.to_vec()), e.clone());
        Ok(e)
    }
    fn leader_events(
        &mut self,
        path: &str,
        value: &Value,
        heads: &[usize],
        inherited: Option<&Events>,
    ) -> Result<Events> {
        match value {
            Value::Space(s) if s.offset.is_some() => {
                let geometry = geometry::resolve(&self.cells, self.seed, &s.axis, s.grain)?;
                let heads: Vec<_> = heads
                    .iter()
                    .map(|n| geometry.representatives[geometry.units[*n]])
                    .collect();
                self.leader_events(
                    &format!("{path}/offset"),
                    s.offset.as_ref().unwrap(),
                    &heads,
                    inherited,
                )
            }
            Value::Time(_) | Value::Random(_) => self.events(path, value, heads, inherited),
            _ => Err(Error(format!("{path} has no event clock"))),
        }
    }
    fn source(
        &mut self,
        path: &str,
        value: &Value,
        heads: &[usize],
        inherited: Option<&Events>,
        vector: bool,
    ) -> Result<Stream> {
        if let Some(v) = value.scalar_value() {
            return Ok(Stream {
                values: std::array::from_fn(|_| number(v)),
                events: None,
            });
        }
        let nested = |name: &str| format!("{path}/{name}");
        let (values, events) = match value {
            Value::Color(v) | Value::Vector(v) => (v.map(number), None),
            Value::Time(t) => {
                let e = self.events(path, value, heads, inherited)?;
                let phase = self.numeric(&nested("phase"), &t.phase, heads, Some(&e))?;
                let gain = self.numeric(&nested("gain"), &t.gain, heads, Some(&e))?;
                let v = self.node(
                    SourceOp::Time,
                    vec![
                        ("curve", value.clone().into()),
                        ("progress", e.progress.clone()),
                        ("phase", phase),
                        ("gain", gain),
                    ],
                    &["r", "g", "b"],
                );
                ([v[0].clone(), v[1].clone(), v[2].clone()], Some(e))
            }
            Value::Space(s) => {
                let geometry = geometry::resolve(&self.cells, self.seed, &s.axis, s.grain)?;
                let heads: Vec<_> = heads
                    .iter()
                    .map(|n| geometry.representatives[geometry.units[*n]])
                    .collect();
                let offset = if let Some(offset) = &s.offset {
                    self.source(&nested("offset"), offset, &heads, inherited, false)?
                } else {
                    Stream {
                        values: std::array::from_fn(|_| number(0.)),
                        events: inherited.cloned(),
                    }
                };
                let e = offset.events.as_ref().or(inherited);
                let gain = self.numeric(&nested("gain"), &s.gain, &heads, e)?;
                let width = self.numeric(&nested("width"), &s.width, &heads, e)?;
                let mut ports = vec![
                    ("settings", value.clone().into()),
                    (
                        "position",
                        self.field(heads.iter().map(|n| geometry.positions[*n]).collect())?,
                    ),
                    ("offset", offset.values[0].clone()),
                    ("gain", gain),
                    ("width", width),
                    ("spacing", e.map_or(number(1.), |e| e.spacing.clone())),
                    ("progress", e.map_or(number(0.), |e| e.progress.clone())),
                    ("vector", number(vector as u8 as f64)),
                ];
                for (ch, name) in ["x", "y", "z"].into_iter().enumerate() {
                    ports.push((
                        name,
                        self.field(
                            heads
                                .iter()
                                .map(|n| {
                                    let (share, toward) = geometry.leans[*n];
                                    let toward = geometry.mirrors[*n]
                                        .map_or(toward, |m| crate::aim::reflect(toward, m));
                                    let magnitude = s
                                        .curve
                                        .as_ref()
                                        .map_or(share, |c| c.sample(share.abs()) * share.signum());
                                    toward[ch] * magnitude
                                })
                                .collect(),
                        )?,
                    ));
                }
                let v = self.node(SourceOp::Space, ports, &["r", "g", "b"]);
                ([v[0].clone(), v[1].clone(), v[2].clone()], e.cloned())
            }
            Value::Random(r) => {
                let geometry = geometry::resolve(
                    &self.cells,
                    self.seed,
                    &default_axis(MappingSource::Order),
                    r.grain,
                )?;
                let heads: Vec<_> = heads
                    .iter()
                    .map(|n| geometry.representatives[geometry.units[*n]])
                    .collect();
                let e = self.events(path, value, &heads, inherited)?;
                let coverage = self.numeric(&nested("coverage"), &r.coverage, &heads, Some(&e))?;
                let level = self.numeric(&nested("level"), &r.level, &heads, Some(&e))?;
                let width = geometry.keys.iter().map(|k| k.len() + 1).max().unwrap_or(1);
                let mut key_bytes = Array3::zeros((1, geometry.keys.len().max(1), width));
                for (g, key) in geometry.keys.iter().enumerate() {
                    key_bytes[[0, g, 0]] = key.len() as f64;
                    for (b, byte) in key.bytes().enumerate() {
                        key_bytes[[0, g, b + 1]] = byte as f64;
                    }
                }
                let keys = Value::Signal(Signal::new(
                    key_bytes,
                    Unit::Number,
                    Channels::components(width)?,
                    None,
                )?)
                .into();
                let unit = self.field(heads.iter().map(|n| geometry.units[*n] as f64).collect())?;
                let v = self.scalar_node(
                    SourceOp::Random,
                    vec![
                        ("coverage", coverage),
                        ("level", level),
                        ("index", e.index.clone()),
                        ("unit", unit),
                        ("keys", keys),
                    ],
                );
                (std::array::from_fn(|_| v.clone()), Some(e))
            }
            Value::Noise(n) => {
                let geometry = geometry::resolve(
                    &self.cells,
                    self.seed,
                    &default_axis(MappingSource::Order),
                    n.grain,
                )?;
                let heads: Vec<_> = heads
                    .iter()
                    .map(|i| geometry.representatives[geometry.units[*i]])
                    .collect();
                let table = self.clock(&nested("speed"), &n.speed, &heads)?;
                let turns = self.scalar_node(SourceOp::Clock, vec![("table", table)]);
                let contrast = self.numeric(&nested("contrast"), &n.contrast, &heads, inherited)?;
                let low = self.numeric(&nested("low"), &n.range[0], &heads, inherited)?;
                let high = self.numeric(&nested("high"), &n.range[1], &heads, inherited)?;
                let scale = if let Some(s) = &n.scale {
                    self.numeric(&nested("scale"), s, &heads, inherited)?
                } else {
                    number(0.)
                };
                let key = n.key.as_deref().unwrap_or(path);
                let mut ports = vec![
                    ("settings", value.clone().into()),
                    ("turns", turns),
                    ("contrast", contrast),
                    ("low", low),
                    ("high", high),
                    ("scale", scale),
                    ("salt", number(salt(key))),
                    ("vector", number(vector as u8 as f64)),
                ];
                for (name, source) in [("x", MappingSource::U), ("y", MappingSource::V)] {
                    let g =
                        geometry::resolve(&self.cells, self.seed, &default_axis(source), n.grain)?;
                    ports.push((
                        name,
                        self.field(heads.iter().map(|n| g.positions[*n]).collect())?,
                    ));
                }
                for (ch, (lo, hi)) in [
                    ("seed0_lo", "seed0_hi"),
                    ("seed1_lo", "seed1_hi"),
                    ("seed2_lo", "seed2_hi"),
                ]
                .into_iter()
                .enumerate()
                {
                    let seeds: Vec<_> = heads
                        .iter()
                        .map(|n| {
                            super::noise::unit_seed(
                                self.seed,
                                &geometry.keys[geometry.units[*n]],
                                key,
                                ch,
                            )
                        })
                        .collect();
                    ports.push((
                        lo,
                        self.field(seeds.iter().map(|v| *v as u32 as f64).collect())?,
                    ));
                    ports.push((
                        hi,
                        self.field(seeds.iter().map(|v| (*v >> 32) as f64).collect())?,
                    ));
                }
                let v = self.node(SourceOp::Noise, ports, &["r", "g", "b"]);
                (
                    [v[0].clone(), v[1].clone(), v[2].clone()],
                    inherited.cloned(),
                )
            }
            Value::Audio(a) => {
                let id = format!("source-{}", self.nodes.len());
                self.nodes.insert(
                    id.clone(),
                    Node {
                        definition: "band_energy".into(),
                        inputs: BTreeMap::from([
                            ("low_hz".into(), number(a.from_hz)),
                            ("high_hz".into(), number(a.to_hz)),
                        ]),
                    },
                );
                let raw = output(&id, "value");
                let id = format!("source-{}", self.nodes.len());
                self.nodes.insert(
                    id.clone(),
                    Node {
                        definition: "clip_range".into(),
                        inputs: BTreeMap::from([("value".into(), raw.clone())]),
                    },
                );
                let gain = self.numeric(&nested("gain"), &a.gain, heads, inherited)?;
                let v = self.scalar_node(
                    SourceOp::Audio,
                    vec![
                        ("energy", raw),
                        ("minimum", output(&id, "minimum")),
                        ("maximum", output(&id, "maximum")),
                        ("gain", gain),
                        ("floor", number(a.floor)),
                        ("threshold", number(a.threshold)),
                    ],
                );
                (std::array::from_fn(|_| v.clone()), inherited.cloned())
            }
            _ => return Err(Error(format!("{path}: expected a source"))),
        };
        Ok(Stream { values, events })
    }
    fn vector(&mut self, path: &str, value: &Value, heads: &[usize]) -> Result<Binding> {
        let s = self.source(path, value, heads, None, true)?;
        let ports = ["r", "g", "b"]
            .into_iter()
            .enumerate()
            .map(|(ch, k)| (k, self.latest(&s, ch, None)))
            .collect();
        Ok(self.scalar_node(SourceOp::Vector, ports))
    }
}

pub(crate) fn lower(
    library: &Library,
    kind: FormKind,
    inputs: &BTreeMap<String, Value>,
    frame: Frame<'_>,
) -> Result<(Library, Definition)> {
    let mut cells = frame.cells.to_vec();
    cells.sort_by(|a, b| a.id.cmp(&b.id));
    let heads: Vec<_> = (0..cells.len()).collect();
    let mut c = Compiler {
        library: library.clone(),
        nodes: BTreeMap::new(),
        inputs,
        fixtures: cells.iter().map(|c| c.id.clone()).collect(),
        cells,
        seed: frame.seed,
        clocks: BTreeMap::new(),
    };
    let fade = c.numeric("fade", &inputs["fade"], &heads, None)?;
    let mut extra = None;
    let (name, value) = match kind {
        FormKind::Color => {
            let levels = c.source("brightness", &inputs["brightness"], &heads, None, false)?;
            let colors = c.source("color", &inputs["color"], &heads, None, false)?;
            let same =
                matches!((&levels.events,&colors.events),(Some(a),Some(b)) if a.owner==b.owner);
            let mut ports = vec![
                ("same_events", number(same as u8 as f64)),
                (
                    "color_index",
                    colors
                        .events
                        .as_ref()
                        .map_or(number(0.), |e| e.index.clone()),
                ),
                (
                    "color_present",
                    colors
                        .events
                        .as_ref()
                        .map_or(number(1.), |e| e.present.clone()),
                ),
                ("level", levels.values[0].clone()),
                ("fade", fade),
                (
                    "present",
                    levels
                        .events
                        .as_ref()
                        .map_or(number(1.), |e| e.present.clone()),
                ),
                (
                    "index",
                    levels
                        .events
                        .as_ref()
                        .map_or(number(0.), |e| e.index.clone()),
                ),
            ];
            for (ch, key) in ["r", "g", "b"].into_iter().enumerate() {
                ports.push((
                    key,
                    if same {
                        colors.values[ch].clone()
                    } else {
                        c.latest(&colors, ch, None)
                    },
                ));
            }
            ("color", c.scalar_node(SourceOp::Color, ports))
        }
        FormKind::Strobe => {
            let rate = c.numeric("rate", &inputs["rate"], &heads, None)?;
            (
                "strobe",
                c.scalar_node(SourceOp::Multiply, vec![("a", rate), ("b", fade)]),
            )
        }
        FormKind::Aim => {
            let direction = c.vector("direction", &inputs["direction"], &heads)?;
            let point = c.vector("point", &inputs["point"], &heads)?;
            let lean = c.vector("lean", &inputs["lean"], &heads)?;
            let yaw = c.numeric("horizontal", &inputs["horizontal"], &heads, None)?;
            let pitch = c.numeric("vertical", &inputs["vertical"], &heads, None)?;
            let Value::Mapping(axis) = &inputs["axis"] else {
                unreachable!()
            };
            let g = geometry::resolve(&c.cells, c.seed, axis, Grain::Head)?;
            let mut ports = vec![
                ("direction", direction),
                ("point", point),
                ("lean", lean),
                ("yaw", yaw),
                ("pitch", pitch),
                ("fade", fade),
                (
                    "point_mode",
                    number(matches!(&inputs["base"],Value::Choice(s) if s=="point") as u8 as f64),
                ),
                (
                    "mirror",
                    c.field(g.mirrors.iter().map(|v| v.is_some() as u8 as f64).collect())?,
                ),
            ];
            for (ch, (pos, mirror)) in [("px", "mx"), ("py", "my"), ("pz", "mz")]
                .into_iter()
                .enumerate()
            {
                ports.push((pos, c.field(c.cells.iter().map(|c| c.uvz[ch]).collect())?));
                ports.push((
                    mirror,
                    c.field(g.mirrors.iter().map(|m| m.unwrap_or([0.; 3])[ch]).collect())?,
                ));
            }
            let v = c.node(SourceOp::Aim, ports, &["value", "turn"]);
            extra = Some(v[1].clone());
            ("aim", v[0].clone())
        }
    };
    c.nodes.insert(
        "output".into(),
        Node {
            definition: "output".into(),
            inputs: BTreeMap::from([(name.into(), value)]),
        },
    );
    let mut outputs = BTreeMap::from([("lighting".into(), output("output", "lighting"))]);
    let mut types = BTreeMap::from([(
        "lighting".into(),
        Output {
            value_type: ValueType::Lighting,
            rate: Rate::Frame,
        },
    )]);
    if let Some(turn) = extra {
        outputs.insert(crate::aim::TURN_OUTPUT.into(), turn);
        types.insert(
            crate::aim::TURN_OUTPUT.into(),
            Output {
                value_type: ValueType::Signal(SignalType::new(
                    Unit::Number,
                    Channels::components(crate::aim::TURN_CHANNELS)?,
                )),
                rate: Rate::Frame,
            },
        );
    }
    Ok((
        c.library,
        Definition {
            name: "Source form".into(),
            inputs: BTreeMap::new(),
            outputs: types,
            body: Body::Graph(Graph {
                nodes: c.nodes,
                outputs,
            }),
        },
    ))
}
