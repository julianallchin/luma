//! Reading an old clip graph as a product of parts. The graphs in stored
//! scores are a small set of shapes: a color times masks, where each mask is
//! a constant, a curve over the clip, an audio level, or one moving look
//! (a pulse, a chase, a dissolve …). This module names those parts and their
//! values; `super` decides which form holds them.
use luma_patterns::{
    AudioInput, AudioSource, Binding, Body, Boundary, Drum, Envelope, EnvelopeCurve, Gradient,
    Graph, Library, MappingSource, MappingSpec, Score, Value,
};
use std::collections::BTreeMap;

/// Score-local helper graphs that are copies of shipped recipes. They open
/// up into the clip graph so the recipe inside them is read like any other.
/// `dissolve_mask/signals` stays closed: it is read as one dissolve.
const OPEN: [&str; 3] = [
    "chase_mask/signals",
    "drum_mask/signals",
    "pulse_mask/signals",
];

/// What starts events.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Trigger {
    /// `repeat` beats apart, counted from the track start (grid aligned) or
    /// the clip start, then shifted by `delay`.
    Periodic {
        repeat: f64,
        grid_aligned: bool,
        delay: f64,
    },
    /// The track's analyzed onsets of one drum.
    Drum(Drum),
    /// One event that spans the clip.
    Clip,
}

/// A stroke across the heads. Its center is `center.0 + center.1 · path(p)`
/// in the old mapping's coordinates, before that mapping's `reverse`; `p` is
/// the event's progress over `travel` beats.
#[derive(Clone, Debug)]
pub(super) struct Stroke {
    pub trigger: Trigger,
    pub travel: f64,
    pub path: Envelope,
    pub center: (f64, f64),
    pub width: f64,
    pub shape: Envelope,
    pub mapping: MappingSpec,
    pub wrap: bool,
    /// The old graph showed one stroke at a time: each event cut the last.
    pub single: bool,
    /// A fade over each event's life, multiplied into the stroke.
    pub fade: Option<Envelope>,
    /// Events before the clip are not the old look's: the start stays.
    pub keep_start: bool,
    pub notes: Vec<String>,
}

/// One moving or colored part.
#[derive(Clone, Debug)]
pub(super) enum Look {
    /// A gradient read once over the clip.
    ColorOverTime(Gradient),
    /// Full-saturation hue turning once every `repeat` beats.
    Rainbow {
        repeat: f64,
    },
    /// The gradient read at the track's chord root; dark without a chord.
    Harmony(Gradient),
    /// `colors` read at `curve(t / every)`, `t` counted from the clip start.
    ColorTime {
        colors: Gradient,
        curve: Envelope,
        every: f64,
    },
    /// A gradient along a mapping.
    Space {
        colors: Gradient,
        mapping: MappingSpec,
    },
    Pulse {
        trigger: Trigger,
        duration: f64,
        shape: Envelope,
    },
    Dissolve {
        trigger: Trigger,
        /// Beats; ignored for [`Trigger::Clip`], whose event is the clip.
        duration: f64,
        coverage: Envelope,
        brightness: Envelope,
        softness: f64,
    },
    RandomHeads {
        trigger: Trigger,
        density: f64,
        shuffle: bool,
    },
    Noise {
        period: f64,
        scale: f64,
        shape: Envelope,
    },
    Stroke(Box<Stroke>),
}

impl Look {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::ColorOverTime(_) => "color over time",
            Self::Rainbow { .. } => "rainbow",
            Self::Harmony(_) => "harmony",
            Self::ColorTime { .. } => "color over time",
            Self::Space { .. } => "spatial gradient",
            Self::Pulse { .. } => "pulse",
            Self::Dissolve { .. } => "dissolve",
            Self::RandomHeads { .. } => "random heads",
            Self::Noise { .. } => "noise",
            Self::Stroke(_) => "chase",
        }
    }

    /// The look sets the color, not only where and when there is light.
    pub(super) fn colored(&self) -> bool {
        matches!(
            self,
            Self::ColorOverTime(_)
                | Self::Rainbow { .. }
                | Self::Harmony(_)
                | Self::ColorTime { .. }
                | Self::Space { .. }
        )
    }
}

/// The energy of a band of one audio source, scaled so that its lowest
/// output is `floor`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Audio {
    pub source: AudioInput,
    pub low_hz: f64,
    pub high_hz: f64,
    pub floor: f64,
}

#[derive(Clone, Debug)]
pub(super) enum Factor {
    Color([f64; 3]),
    Scalar(f64),
    /// A curve over the clip's progress.
    Curve(Envelope),
    Audio(Audio),
    /// On only while the audio level is above a threshold: the band and
    /// the threshold, when the level is a remapped band energy.
    Gate(Option<(Audio, f64)>),
    Look(Look),
    /// A gate that the look already has: a stroke's own "active", or the
    /// presence of a chord.
    Implied,
    /// Why the reading may not render like the old graph.
    Note(String),
}

/// The parts that the output node multiplies.
pub(super) struct Parsed {
    pub color: Option<Vec<Factor>>,
    pub strobe: Option<Vec<Factor>>,
    pub aims: bool,
}

pub(super) struct Parser<'a> {
    graph: Graph,
    library: &'a Library,
}

type Found<T> = Result<T, String>;

impl<'a> Parser<'a> {
    /// The clip's graph with its inputs bound to the clip's values and the
    /// copied recipe helpers opened up.
    pub(super) fn new(
        score: &Score,
        graph_id: &str,
        inputs: &BTreeMap<String, Value>,
        library: &'a Library,
    ) -> Found<Self> {
        let definition = score
            .definitions
            .get(graph_id)
            .ok_or_else(|| format!("{graph_id} is not a score-local graph"))?;
        let Body::Graph(graph) = &definition.body else {
            return Err(format!("{graph_id} is a primitive"));
        };
        let mut graph = graph.clone();
        for binding in graph
            .nodes
            .values_mut()
            .flat_map(|node| node.inputs.values_mut())
            .chain(graph.outputs.values_mut())
        {
            if let Binding::Input { input } = binding {
                let value = inputs
                    .get(input)
                    .cloned()
                    .or_else(|| definition.inputs.get(input)?.default.clone())
                    .ok_or_else(|| format!("input {input} has no value"))?;
                *binding = Binding::Value { value };
            }
        }
        loop {
            let open = graph
                .nodes
                .iter()
                .find(|(_, node)| OPEN.contains(&node.definition.as_str()))
                .map(|(id, node)| (id.clone(), node.definition.clone()));
            let Some((id, helper)) = open else { break };
            let callee = score
                .definitions
                .get(&helper)
                .ok_or_else(|| format!("{helper} is missing from the score"))?;
            graph.inline(&id, callee).map_err(|e| e.to_string())?;
        }
        Ok(Self { graph, library })
    }

    pub(super) fn parse(&self) -> Found<Parsed> {
        let Some(Binding::Connection { node, .. }) = self.graph.outputs.get("lighting") else {
            return Err("the graph has no lighting output".into());
        };
        let output = self.node(node)?;
        if output.definition != "output" {
            return Err(format!("lighting comes from {}", output.definition));
        }
        let factors = |port: &str| -> Found<Option<Vec<Factor>>> {
            output
                .inputs
                .get(port)
                .map(|binding| {
                    let mut factors = Vec::new();
                    self.factors(binding, &mut factors)?;
                    Ok(factors)
                })
                .transpose()
        };
        Ok(Parsed {
            color: factors("color")?,
            strobe: factors("strobe")?,
            aims: ["pan", "tilt"]
                .iter()
                .any(|port| output.inputs.contains_key(*port)),
        })
    }

    // -----------------------------------------------------------------------
    // Reading values

    fn node(&self, id: &str) -> Found<&luma_patterns::Node> {
        self.graph
            .nodes
            .get(id)
            .ok_or_else(|| format!("unknown node {id}"))
    }

    /// The binding of `port`, or the node definition's default.
    fn port(&self, id: &str, port: &str) -> Found<Binding> {
        let node = self.node(id)?;
        if let Some(binding) = node.inputs.get(port) {
            return Ok(binding.clone());
        }
        self.library
            .definitions
            .get(&node.definition)
            .and_then(|definition| definition.inputs.get(port)?.default.clone())
            .map(Binding::from)
            .ok_or_else(|| format!("{}.{port} has no value", node.definition))
    }

    fn value(&self, id: &str, port: &str) -> Found<Value> {
        match self.port(id, port)? {
            Binding::Value { value } => Ok(value),
            _ => Err(format!("{}.{port} is wired", self.node(id)?.definition)),
        }
    }

    fn number(&self, id: &str, port: &str) -> Found<f64> {
        number(&self.value(id, port)?).ok_or_else(|| format!("{port} is not a number"))
    }

    fn envelope(&self, id: &str, port: &str) -> Found<Envelope> {
        match self.value(id, port)? {
            Value::Envelope(envelope) => Ok(envelope),
            other => Err(format!("{port} is not a curve: {other:?}")),
        }
    }

    fn boolean(&self, id: &str, port: &str) -> Found<bool> {
        match self.value(id, port)? {
            Value::Boolean(value) => Ok(value),
            _ => Err(format!("{port} is not a boolean")),
        }
    }

    /// The node and output feeding `port`.
    fn source(&self, id: &str, port: &str) -> Found<(String, String)> {
        match self.port(id, port)? {
            Binding::Connection { node, output } => Ok((node, output)),
            _ => Err(format!("{}.{port} is not wired", self.node(id)?.definition)),
        }
    }

    /// The definition of the node feeding `port`.
    fn source_is(&self, id: &str, port: &str, definition: &str) -> bool {
        self.source(id, port)
            .ok()
            .and_then(|(node, _)| self.node(&node).ok())
            .is_some_and(|node| node.definition == definition)
    }

    /// The first node upstream of `id` (itself included) with `definition`.
    fn upstream(&self, id: &str, definition: &str) -> Option<String> {
        let mut queue = vec![id.to_owned()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = queue.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let node = self.graph.nodes.get(&id)?;
            if node.definition == definition {
                return Some(id);
            }
            for binding in node.inputs.values() {
                if let Binding::Connection { node, .. } = binding {
                    queue.push(node.clone());
                }
            }
        }
        None
    }

    fn literal(&self, id: &str, port: &str, expected: f64) -> bool {
        self.number(id, port).is_ok_and(|v| v == expected)
    }

    // -----------------------------------------------------------------------
    // Factors

    fn factors(&self, binding: &Binding, out: &mut Vec<Factor>) -> Found<()> {
        match binding {
            Binding::Value {
                value: Value::Color(rgb),
            } => out.push(Factor::Color(*rgb)),
            Binding::Value { value } => out.push(Factor::Scalar(
                number(value).ok_or_else(|| format!("unexpected value {value:?}"))?,
            )),
            Binding::Input { input } => return Err(format!("unbound input {input}")),
            Binding::Connection { node, output } => self.node_factors(node, output, out)?,
        }
        Ok(())
    }

    fn node_factors(&self, id: &str, output: &str, out: &mut Vec<Factor>) -> Found<()> {
        let definition = self.node(id)?.definition.as_str();
        let port = |name: &str| self.port(id, name);
        match (definition, output) {
            ("core/multiply" | "multiply_mask", _) => {
                self.factors(&port("a")?, out)?;
                self.factors(&port("b")?, out)?;
            }
            ("scale_mask", _) => {
                self.factors(&port("mask")?, out)?;
                self.factors(&port("amount")?, out)?;
            }
            ("uniform_mask", _) => self.factors(&port("coverage")?, out)?,
            ("core/clamp_coverage", _) => self.factors(&port("value")?, out)?,
            // Clamps to 0–1 and adding zero change nothing a product holds.
            ("core/maximum", _) if self.literal(id, "b", 0.0) => self.factors(&port("a")?, out)?,
            ("core/maximum", _) if self.literal(id, "a", 0.0) => self.factors(&port("b")?, out)?,
            ("core/minimum", _) if self.literal(id, "b", 1.0) => self.factors(&port("a")?, out)?,
            ("core/add", _) if self.literal(id, "a", 0.0) => self.factors(&port("b")?, out)?,
            ("core/add", _) if self.literal(id, "b", 0.0) => self.factors(&port("a")?, out)?,
            ("envelope", "value") if self.clip_progress(id, "progress") => {
                out.push(Factor::Curve(self.envelope(id, "shape")?))
            }
            ("pulse", "mask") => out.push(Factor::Look(Look::Pulse {
                trigger: self.trigger(id, "trigger")?,
                duration: self.number(id, "duration")?,
                shape: self.envelope(id, "shape")?,
            })),
            ("dissolve", "mask") => out.push(Factor::Look(Look::Dissolve {
                trigger: self.trigger(id, "trigger")?,
                duration: self.number(id, "duration")?,
                coverage: self.envelope(id, "proportion")?,
                brightness: self.envelope(id, "shape")?,
                softness: self.number(id, "softness")?,
            })),
            ("random_heads_mask", "mask") => out.push(Factor::Look(Look::RandomHeads {
                trigger: Trigger::Periodic {
                    repeat: self.number(id, "repeat")?,
                    grid_aligned: self.boolean(id, "grid_aligned")?,
                    delay: self.number(id, "delay")?,
                },
                density: self.number(id, "density")?,
                shuffle: self.boolean(id, "shuffle")?,
            })),
            ("noise_mask", "mask") => out.push(Factor::Look(Look::Noise {
                period: self.number(id, "period")?,
                scale: self.number(id, "scale")?,
                shape: self.envelope(id, "shape")?,
            })),
            ("chase", "mask") => {
                out.push(Factor::Look(Look::Stroke(Box::new(self.recipe_chase(id)?))))
            }
            ("profile_mask/signals" | "profile_mask", "mask") => out.push(Factor::Look(
                Look::Stroke(Box::new(self.profile_stroke(id)?)),
            )),
            ("core/absolute", "value") => out.push(Factor::Look(Look::Stroke(Box::new(
                self.alternating_sides(id)?,
            )))),
            ("band_mask", "mask") => {
                let shape = self.envelope(id, "shape")?;
                out.push(Factor::Audio(Audio {
                    source: self.audio_source(id, "source")?,
                    low_hz: self.number(id, "low_hz")?,
                    high_hz: self.number(id, "high_hz")?,
                    floor: shape.sample(0.0).clamp(0.0, 1.0),
                }));
            }
            // An energy remapped between a quiet and a full level; clamps
            // downstream keep it within 0–1.
            ("remap_field/signals", "value") => {
                let (energy, _) = self.source(id, "value")?;
                if self.node(&energy)?.definition != "band_energy" {
                    return Err("remap of something other than band energy".into());
                }
                out.push(Factor::Audio(Audio {
                    source: self.audio_source(&energy, "source")?,
                    low_hz: self.number(&energy, "low_hz")?,
                    high_hz: self.number(&energy, "high_hz")?,
                    floor: 0.0,
                }));
            }
            ("wash", "color") => self.factors(&port("color")?, out)?,
            ("spatial_gradient", "color") => out.push(Factor::Look(Look::Space {
                colors: self.gradient(id, "gradient")?,
                mapping: self.mapping(id, "mapping")?,
            })),
            ("dissolve_mask/signals", "mask") => {
                out.push(Factor::Look(self.dissolve_over_clip(id)?));
            }
            // A level meter: a head is lit while the level is above its
            // height.
            ("core/greater", "mask")
                if self.source_is(id, "a", "band_mask")
                    && self.source_is(id, "b", "mapped_position") =>
            {
                let (band, output) = self.source(id, "a")?;
                self.node_factors(&band, &output, out)?;
                out.push(Factor::Note(
                    "a level meter (heads lit up to the audio level) → every head follows the level"
                        .into(),
                ));
            }
            ("ripple", "mask") => out.push(Factor::Look(Look::Stroke(Box::new(self.ripple(id)?)))),
            ("core/greater", "mask") if self.upstream(id, "band_energy").is_some() => {
                // The old level is energy remapped between the clip's quiet
                // and full levels, the same 0–1 scale as the audio source.
                let band = self.upstream(id, "remap_field/signals").and_then(|remap| {
                    let (energy, _) = self.source(&remap, "value").ok()?;
                    Some(Audio {
                        source: self.audio_source(&energy, "source").ok()?,
                        low_hz: self.number(&energy, "low_hz").ok()?,
                        high_hz: self.number(&energy, "high_hz").ok()?,
                        floor: 1.0,
                    })
                });
                let level = self.number(id, "b").ok();
                out.push(Factor::Gate(band.zip(level)))
            }
            ("sample_gradient" | "sample_field_gradient", "color") => {
                let gradient = match self.value(id, "gradient")? {
                    Value::Gradient(gradient) => gradient,
                    _ => return Err("gradient is not a gradient".into()),
                };
                let (position, _) = self.source(id, "position")?;
                if self.upstream(&position, "harmony").is_some() {
                    out.push(Factor::Look(Look::Harmony(gradient)));
                } else if self.clip_progress(id, "position") {
                    out.push(Factor::Look(Look::ColorOverTime(gradient)));
                } else if let Some(every) = self.cycles_over_clip(&position)? {
                    if self.upstream(&position, "mapped_position").is_some() {
                        out.push(Factor::Note(
                            "a color wave along the rig → every head shows the same color".into(),
                        ));
                    }
                    out.push(Factor::Look(Look::ColorTime {
                        colors: gradient,
                        curve: Envelope::linear(vec![[0.0, 0.0], [1.0, 1.0]]),
                        every,
                    }));
                } else if self.node(&position)?.definition == "core/greater"
                    && self.upstream(&position, "core/rank").is_some()
                {
                    out.push(Factor::Note(
                        "alternating colors along the rig → all heads swap colors together".into(),
                    ));
                    out.push(Factor::Look(self.alternating_colors(&position, &gradient)?));
                } else {
                    return Err("a gradient read at something other than the clip time".into());
                }
            }
            ("hsv", "color") => {
                let rhythm = self
                    .upstream(id, "rhythm")
                    .ok_or("hue that does not follow a rhythm")?;
                if !self.literal(id, "saturation", 1.0) || !self.literal(id, "value", 1.0) {
                    return Err("rainbow below full saturation or value".into());
                }
                if self.number(&rhythm, "delay")? != 0.0 || self.boolean(&rhythm, "grid_aligned")? {
                    return Err("rainbow with a phase offset".into());
                }
                out.push(Factor::Look(Look::Rainbow {
                    repeat: self.number(&rhythm, "repeat")?,
                }));
            }
            ("motion/signals", "active") | ("harmony", "present") => out.push(Factor::Implied),
            _ => return Err(format!("no rule for {definition}.{output}")),
        }
        Ok(())
    }

    /// `port` is fed by a clip clock's progress.
    fn clip_progress(&self, id: &str, port: &str) -> bool {
        matches!(self.source(id, port), Ok((node, output))
            if output == "progress"
                && self.node(&node).is_ok_and(|node| node.definition == "clip_time"))
    }

    fn audio_source(&self, id: &str, port: &str) -> Found<AudioInput> {
        match self.value(id, port)? {
            Value::AudioSource(source) => Ok(source),
            _ => Err(format!("{port} is not an audio source")),
        }
    }

    fn trigger(&self, id: &str, port: &str) -> Found<Trigger> {
        let (source, _) = self.source(id, port)?;
        match self.node(&source)?.definition.as_str() {
            "beat_trigger" => Ok(Trigger::Periodic {
                repeat: self.number(&source, "repeat")?,
                grid_aligned: self.boolean(&source, "grid_aligned")?,
                delay: self.number(&source, "delay")?,
            }),
            "drum_trigger" => match self.value(&source, "drum")? {
                Value::Drum(drum) => Ok(Trigger::Drum(drum)),
                _ => Err("drum is not a drum".into()),
            },
            other => Err(format!("events from {other}")),
        }
    }

    fn gradient(&self, id: &str, port: &str) -> Found<Gradient> {
        match self.value(id, port)? {
            Value::Gradient(gradient) => Ok(gradient),
            _ => Err(format!("{port} is not a gradient")),
        }
    }

    fn mapping(&self, id: &str, port: &str) -> Found<MappingSpec> {
        match self.value(id, port)? {
            Value::Mapping(mapping) => Ok(mapping),
            _ => Err(format!("{port} is not a mapping")),
        }
    }

    // -----------------------------------------------------------------------
    // Strokes

    /// The shipped chase: overlapping strokes that run fully onto and off an
    /// open axis.
    fn recipe_chase(&self, id: &str) -> Found<Stroke> {
        let (resolve, _) = self.source(id, "mapping")?;
        if self.node(&resolve)?.definition != "resolve_mapping" {
            return Err("chase mapping is computed".into());
        }
        let mapping = self.mapping(&resolve, "mapping")?;
        let boundary = match self.value(id, "boundary")? {
            Value::Boundary(boundary) => boundary,
            _ => return Err("boundary is not a boundary".into()),
        };
        let wrap = match boundary {
            Boundary::Wrap => true,
            Boundary::Clip => false,
            Boundary::Natural => closed(&mapping),
        };
        let (start, end) = (self.number(id, "start")?, self.number(id, "end")?);
        let width = self.number(id, "width")?;
        let run = if wrap {
            0.0
        } else {
            let span = end - start;
            width / 2.0 * span / span.abs().max(1e-6)
        };
        Ok(Stroke {
            trigger: self.trigger(id, "trigger")?,
            travel: self.number(id, "travel")?,
            path: self.envelope(id, "path")?,
            center: (start - run, end - start + 2.0 * run),
            width,
            shape: self.envelope(id, "shape")?,
            mapping,
            wrap,
            single: false,
            fade: None,
            keep_start: false,
            notes: Vec::new(),
        })
    }

    /// A shape along a field around a moving center: a rhythm-driven motion,
    /// a phase turning around a circle, or a curve over the clip.
    fn profile_stroke(&self, id: &str) -> Found<Stroke> {
        let width = self.number(id, "width")?;
        let shape = self.envelope(id, "shape")?;
        let (offset, _) = self.source(id, "offset")?;
        let offset_node = self.node(&offset)?;
        match offset_node.definition.as_str() {
            "core/subtract" if self.source_is(&offset, "b", "motion/signals") => {
                self.motion_stroke(&offset, width, shape)
            }
            "core/subtract" if self.source_is(&offset, "a", "core/fraction") => {
                self.circle_stroke(&offset, width, shape)
            }
            "core/subtract" if self.upstream(&offset, "core/sine").is_some() => {
                self.turning_line(&offset, width, shape)
            }
            "coordinate_offset" => {
                let (position, _) = self.source(&offset, "position")?;
                if !(self.node(&position)?.definition == "envelope"
                    && self.clip_progress(&position, "progress"))
                {
                    return Err("a stroke placed by something other than a clip curve".into());
                }
                let (resolve, _) = self.source(&offset, "mapping")?;
                let mapping = self.mapping(&resolve, "mapping")?;
                let wrap = match self.value(&offset, "boundary")? {
                    Value::Boundary(Boundary::Wrap) => true,
                    Value::Boundary(Boundary::Natural) => closed(&mapping),
                    _ => false,
                };
                Ok(Stroke {
                    trigger: Trigger::Clip,
                    travel: 0.0,
                    path: self.envelope(&position, "shape")?,
                    center: (0.0, 1.0),
                    width,
                    shape,
                    mapping,
                    wrap,
                    single: false,
                    fade: None,
                    keep_start: false,
                    notes: Vec::new(),
                })
            }
            other => Err(format!("a stroke offset from {other}")),
        }
    }

    /// `field − motion.position`: one stroke per rhythm cycle.
    fn motion_stroke(&self, offset: &str, width: f64, shape: Envelope) -> Found<Stroke> {
        let (motion, _) = self.source(offset, "b")?;
        let (field, _) = self.source(offset, "a")?;
        let mut notes = Vec::new();
        let mapping = match self.node(&field)?.definition.as_str() {
            "mapped_position" => self.mapping(&field, "mapping")?,
            "normalize_field/signals"
                if self.upstream(&field, "radial_distance/signals").is_some() =>
            {
                notes.push("radial center: selection centroid → extent middle".into());
                MappingSpec {
                    source: MappingSource::Radial,
                    per_group: false,
                    reverse: false,
                    mirror: None,
                }
            }
            other => return Err(format!("a stroke along {other}")),
        };
        let (rhythm, output) = self.source(&motion, "elapsed")?;
        if self.node(&rhythm)?.definition != "rhythm" || output != "elapsed" {
            return Err("motion driven by something other than a rhythm".into());
        }
        let (start, end) = (self.number(&motion, "start")?, self.number(&motion, "end")?);
        Ok(Stroke {
            trigger: Trigger::Periodic {
                repeat: self.number(&rhythm, "repeat")?,
                grid_aligned: self.boolean(&rhythm, "grid_aligned")?,
                delay: self.number(&rhythm, "delay")?,
            },
            travel: self.number(&motion, "travel")?,
            path: self.envelope(&motion, "path")?,
            center: (start, end - start),
            width,
            shape,
            mapping,
            wrap: false,
            single: true,
            fade: None,
            keep_start: false,
            notes,
        })
    }

    /// `fraction(copies − phase + 1/2) − 1/2`: `count` strokes turning around
    /// a closed mapping once per rhythm cycle, gliding or in `steps`.
    fn circle_stroke(&self, nearest: &str, width: f64, shape: Envelope) -> Found<Stroke> {
        let rhythm = self
            .upstream(nearest, "rhythm")
            .ok_or("a circle stroke without a rhythm")?;
        let field = self
            .upstream(nearest, "mapped_position")
            .ok_or("a circle stroke without a mapping")?;
        let repeat = self.number(&rhythm, "repeat")?;
        let trigger = Trigger::Periodic {
            repeat,
            grid_aligned: self.boolean(&rhythm, "grid_aligned")?,
            delay: self.number(&rhythm, "delay")?,
        };
        let copies = self
            .graph
            .nodes
            .iter()
            .find(|(id, node)| {
                node.definition == "core/multiply"
                    && self
                        .source(id, "a")
                        .is_ok_and(|(source, _)| source == field)
            })
            .map(|(id, _)| self.number(id, "b"))
            .transpose()?
            .unwrap_or(1.0);
        let steps = self.upstream(nearest, "core/floor").and_then(|floor| {
            let (scaled, _) = self.source(&floor, "value").ok()?;
            self.number(&scaled, "b").ok()
        });
        let path = match steps {
            None => Envelope::linear(vec![[0.0, 0.0], [1.0, 1.0]]),
            Some(count) if count >= 1.0 && count.fract() == 0.0 && count <= 64.0 => {
                let count = count as usize;
                let mut points: Vec<[f64; 2]> = (0..count)
                    .map(|i| {
                        let at = i as f64 / count as f64;
                        [at, at]
                    })
                    .collect();
                points.push([1.0, (count - 1) as f64 / count as f64]);
                Envelope {
                    curves: vec![EnvelopeCurve::Hold; points.len() - 1],
                    points,
                }
            }
            Some(count) => return Err(format!("{count} circle steps")),
        };
        let mut notes = Vec::new();
        if copies != 1.0 {
            notes.push(format!(
                "{copies} strokes around the circle: the first {} cycles miss strokes that began before the clip",
                copies - 1.0
            ));
        }
        if copies <= 0.0 {
            return Err(format!("{copies} circle copies"));
        }
        Ok(Stroke {
            trigger,
            travel: repeat * copies,
            path,
            center: (0.0, 1.0),
            width: width / copies,
            shape,
            mapping: self.mapping(&field, "mapping")?,
            wrap: true,
            single: false,
            fade: None,
            keep_start: false,
            notes,
        })
    }

    /// A ripple: each event draws a random head and a ring of `ring` grows
    /// from it to `reach` over `duration`, fading by `fade`.
    fn ripple(&self, id: &str) -> Found<Stroke> {
        let reach = self.number(id, "reach")?;
        if reach <= 0.0 {
            return Err(format!("ripple reach {reach}"));
        }
        Ok(Stroke {
            trigger: self.trigger(id, "trigger")?,
            travel: self.number(id, "duration")?,
            path: Envelope::linear(vec![[0.0, 0.0], [1.0, 1.0]]),
            center: (0.0, 1.0),
            width: self.number(id, "width")? / reach,
            shape: self.envelope(id, "ring")?,
            mapping: MappingSpec {
                source: MappingSource::Radial,
                per_group: false,
                reverse: false,
                mirror: None,
            },
            wrap: false,
            single: false,
            fade: Some(self.envelope(id, "fade")?),
            keep_start: false,
            notes: vec![
                "ripple: rings start at the rig center, not at a random head".into(),
                format!("ripple: rings reach the rig edge, not {reach} m"),
            ],
        })
    }

    /// `(z − z̄)·sin(θ + ¼) − (u − ū)·sin(θ)`: the distance from a line
    /// through the center that turns once per rhythm cycle. It lights two
    /// opposite sides, so it becomes two strokes half a turn apart.
    fn turning_line(&self, offset: &str, width: f64, shape: Envelope) -> Found<Stroke> {
        let rhythm = self
            .upstream(offset, "rhythm")
            .ok_or("a turning line without a rhythm")?;
        let repeat = self.number(&rhythm, "repeat")?;
        Ok(Stroke {
            trigger: Trigger::Periodic {
                repeat: repeat / 2.0,
                grid_aligned: self.boolean(&rhythm, "grid_aligned")?,
                delay: self.number(&rhythm, "delay")?,
            },
            travel: repeat,
            path: Envelope::linear(vec![[0.0, 0.0], [1.0, 1.0]]),
            center: (0.0, 1.0),
            width: 0.5,
            shape,
            mapping: MappingSpec {
                source: MappingSource::Angle,
                per_group: false,
                reverse: false,
                mirror: None,
            },
            wrap: true,
            single: false,
            fade: None,
            keep_start: true,
            notes: vec![
                format!(
                    "a line {width} wide turning in the u–z plane → two strokes half a turn apart on the angle axis (u–v plane)"
                ),
                "the first half turn misses the stroke that began before the clip".into(),
            ],
        })
    }

    /// The `dissolve_mask/signals` helper with a fixed order: heads light in
    /// random order as the coverage curve over the clip rises.
    fn dissolve_over_clip(&self, id: &str) -> Found<Look> {
        if self.boolean(id, "refresh")? {
            return Err("a dissolve that flickers".into());
        }
        let coverage = match self.port(id, "coverage")? {
            Binding::Value { value } => {
                let level = number(&value).ok_or("coverage is not a number")?;
                Envelope::linear(vec![[0.0, level], [1.0, level]])
            }
            Binding::Connection { node, output }
                if output == "value"
                    && self.node(&node)?.definition == "envelope"
                    && self.clip_progress(&node, "progress") =>
            {
                self.envelope(&node, "shape")?
            }
            _ => return Err("a dissolve coverage that is not a curve over the clip".into()),
        };
        Ok(Look::Dissolve {
            trigger: Trigger::Clip,
            duration: 0.0,
            coverage,
            brightness: Envelope::linear(vec![[0.0, 1.0], [1.0, 1.0]]),
            softness: self.number(id, "softness")?,
        })
    }

    /// `fraction(… + clip elapsed / period)`: a gradient read once every
    /// `period` beats from the clip start. The period.
    fn cycles_over_clip(&self, position: &str) -> Found<Option<f64>> {
        if self.node(position)?.definition != "core/fraction" {
            return Ok(None);
        }
        let Some(divide) = self.upstream(position, "core/divide") else {
            return Ok(None);
        };
        let Ok((clock, output)) = self.source(&divide, "a") else {
            return Ok(None);
        };
        if output != "elapsed" || self.node(&clock)?.definition != "clip_time" {
            return Ok(None);
        }
        let (sum, _) = self.source(position, "value")?;
        if self.node(&sum)?.definition != "core/add" {
            return Err("a color wave that does not add its phase".into());
        }
        Ok(Some(self.number(&divide, "b")?))
    }

    /// `fraction((rank + cycle) / 2) > ¼`: the two ends of a gradient on
    /// alternate heads, swapping every rhythm cycle.
    fn alternating_colors(&self, position: &str, gradient: &Gradient) -> Found<Look> {
        let rhythm = self
            .upstream(position, "rhythm")
            .ok_or("alternating colors without a rhythm")?;
        if self.number(&rhythm, "delay")? != 0.0 || self.boolean(&rhythm, "grid_aligned")? {
            return Err("alternating colors with a phase offset".into());
        }
        let stop = |t: f64| luma_patterns::ColorStop {
            t,
            color: gradient.sample(t),
            alpha: 1.0,
        };
        Ok(Look::ColorTime {
            colors: Gradient {
                stops: vec![stop(0.0), stop(1.0)],
            },
            curve: Envelope {
                points: vec![[0.0, 0.0], [0.5, 1.0], [1.0, 1.0]],
                curves: vec![EnvelopeCurve::Hold; 2],
            },
            every: 2.0 * self.number(&rhythm, "repeat")?,
        })
    }

    /// `|side(x) − side(phase)|`: one half of the rig, then the other, once
    /// per rhythm cycle. The right half (x above one half) lights first.
    fn alternating_sides(&self, id: &str) -> Found<Stroke> {
        let rhythm = self
            .upstream(id, "rhythm")
            .ok_or("absolute value without a rhythm")?;
        let field = self
            .upstream(id, "mapped_position")
            .ok_or("absolute value without a mapping")?;
        let repeat = self.number(&rhythm, "repeat")?;
        Ok(Stroke {
            trigger: Trigger::Periodic {
                repeat,
                grid_aligned: self.boolean(&rhythm, "grid_aligned")?,
                delay: self.number(&rhythm, "delay")?,
            },
            travel: repeat,
            path: Envelope {
                points: vec![[0.0, 0.75], [0.5, 0.25], [1.0, 0.25]],
                curves: vec![EnvelopeCurve::Hold; 2],
            },
            center: (0.0, 1.0),
            width: 0.5,
            shape: Envelope::linear(vec![[0.0, 1.0], [1.0, 1.0]]),
            mapping: self.mapping(&field, "mapping")?,
            wrap: false,
            single: false,
            fade: None,
            keep_start: false,
            notes: Vec::new(),
        })
    }
}

/// Coordinates that close on themselves.
pub(super) fn closed(mapping: &MappingSpec) -> bool {
    matches!(
        mapping.source,
        MappingSource::Circle { .. } | MappingSource::Angle
    )
}

pub(super) fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(v)
        | Value::Beats(v)
        | Value::Proportion(v)
        | Value::Position(v)
        | Value::Degrees(v)
        | Value::Seconds(v) => Some(*v),
        _ => None,
    }
}

/// The full mix, which is the only source an `audio` input reads.
pub(super) fn is_mix(source: &AudioInput) -> bool {
    source.source() == AudioSource::Mix && source.filters().is_empty()
}
