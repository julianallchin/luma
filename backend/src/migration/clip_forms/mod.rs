//! Converting a stored clip from its score-local graph to a clip form
//! (docs/specs/clip-forms.md, Migration). The conversion only proposes new
//! clip values; it reads the score and the track's analysis and writes
//! nothing.
//!
//! A clip graph is read as a color times a product of masks (see
//! [`looks`]). The product becomes one form: the moving part picks the form,
//! constants and curves over the clip become `alpha`, and removed controls
//! (`delay`, `grid_aligned`, drum triggers, mapping `reverse`, stems, harmony)
//! are rewritten by the rules of the spec. Every conversion lists the reasons
//! it may not render exactly like the old graph, so a render check can tell
//! an expected difference from a wrong rule.
mod curves;
mod looks;
#[cfg(test)]
mod tests;

use looks::{Audio, Factor, Look, Parser, Stroke, Trigger};
use luma_patterns::{
    is_form, presets, standard_library, AudioLevel, Boundary, Clip, ColorStop, Drum, Envelope,
    EnvelopeCurve, EventTimes, Events, FormPreset, Gradient, Score, Value,
};
use std::collections::BTreeMap;

/// Track data the removed sources read.
#[derive(Clone, Debug, Default)]
pub struct Host {
    /// Analyzed onsets per drum, in ordered absolute beats.
    pub onsets: BTreeMap<Drum, Vec<f64>>,
    /// Chord sections `(start, end, root)` in beats; `None` is no chord.
    pub chords: Vec<(f64, f64, Option<u8>)>,
    /// Heads the clip's selection resolves to.
    pub heads: usize,
}

/// A proposed form clip.
#[derive(Clone, Debug)]
pub struct Converted {
    pub clip: Clip,
    /// The old look the conversion recognized, for the report.
    pub look: String,
    /// Why the form may not render exactly like the old graph.
    pub notes: Vec<String>,
    /// Absolute beats where the output changes, for a render check.
    pub boundaries: Vec<f64>,
}

/// Convert clip `id` of `score`. `selection` is the stored selection JSON,
/// which may still carry a `subset` that the selection reader drops. An
/// error is a clip that no rule matches, with the reason.
pub fn convert(
    score: &Score,
    id: &str,
    selection: &serde_json::Value,
    host: &Host,
) -> Result<Converted, String> {
    let clip = score
        .clips
        .get(id)
        .ok_or_else(|| format!("unknown clip {id}"))?;
    if is_form(&clip.graph) {
        return Ok(Converted {
            clip: clip.clone(),
            look: "already a form".into(),
            notes: Vec::new(),
            boundaries: Vec::new(),
        });
    }
    let library = score
        .library(&standard_library())
        .map_err(|e| e.to_string())?;
    let parsed = Parser::new(score, &clip.graph, &clip.inputs, &library)?.parse()?;
    if parsed.aims {
        return Err("pan and tilt: aim forms are a later spec".into());
    }
    let mut build = Build::new(clip, host);
    let (form, look) = match (parsed.color, parsed.strobe) {
        (color, Some(strobe)) => {
            build.strobe(color.map(Parts::of).transpose()?, Parts::of(strobe)?)?
        }
        (Some(color), None) => build.color(Parts::of(color)?)?,
        (None, None) => return Err("the graph writes no light".into()),
    };
    let (form, look) = build.subset(selection, form, look)?;
    let mut clip = build.clip;
    clip.graph = form.form.clone();
    clip.inputs = form.inputs.clone();
    form.validate(&standard_library())
        .map_err(|e| format!("the converted clip is invalid: {e}"))?;
    build.boundaries.retain(|b| b.is_finite());
    build.boundaries.sort_by(f64::total_cmp);
    build.boundaries.dedup();
    Ok(Converted {
        clip,
        look,
        notes: build.notes,
        boundaries: build.boundaries,
    })
}

/// Form clips saved by a draft build store `audio(band, range)`. Rewrite
/// every such value in stored inputs JSON to `audio(from_hz, to_hz, floor)`,
/// and say what did not carry over.
pub fn repair_draft_inputs(inputs: &mut serde_json::Value) -> Vec<String> {
    let mut notes = Vec::new();
    let Some(inputs) = inputs.as_object_mut() else {
        return notes;
    };
    for (name, value) in inputs.iter_mut() {
        if value.get("type").and_then(|t| t.as_str()) != Some("audio") {
            continue;
        }
        let level = &value["value"];
        let (Some(band), Some(range)) = (
            level.get("band").and_then(|b| b.as_str()),
            level.get("range").and_then(|r| r.as_array()),
        ) else {
            continue;
        };
        let (from_hz, to_hz) = match band {
            "low" => (20.0, 250.0),
            "mid" => (250.0, 4000.0),
            "high" => (4000.0, 16000.0),
            _ => (20.0, 16000.0),
        };
        let bound = |i: usize| range.get(i).and_then(|v| v.as_f64());
        let (floor, top) = (bound(0).unwrap_or(0.0), bound(1).unwrap_or(1.0));
        if top != 1.0 {
            notes.push(format!("draft audio on {name}: top of range {top} dropped"));
        }
        value["value"] = serde_json::json!({
            "from_hz": from_hz,
            "to_hz": to_hz,
            "floor": floor.clamp(0.0, 1.0),
        });
    }
    notes
}

/// The factors of one output, sorted by kind.
#[derive(Clone)]
struct Parts {
    color: [f64; 3],
    scalar: f64,
    curves: Vec<Envelope>,
    audio: Option<Audio>,
    gated: bool,
    look: Option<Look>,
    /// The look's own gate was multiplied in.
    implied: bool,
}

impl Parts {
    fn of(factors: Vec<Factor>) -> Result<Self, String> {
        let mut parts = Self {
            color: [1.0; 3],
            scalar: 1.0,
            curves: Vec::new(),
            audio: None,
            gated: false,
            look: None,
            implied: false,
        };
        for factor in factors {
            match factor {
                Factor::Color(rgb) => {
                    for (channel, value) in parts.color.iter_mut().zip(rgb) {
                        *channel *= value;
                    }
                }
                Factor::Scalar(value) => parts.scalar *= value,
                Factor::Curve(curve) => match curves::constant(&curve) {
                    Some(value) => parts.scalar *= value,
                    None => parts.curves.push(curve),
                },
                Factor::Audio(audio) => {
                    if parts.audio.replace(audio).is_some() {
                        return Err("two audio levels multiplied".into());
                    }
                }
                Factor::Gate => parts.gated = true,
                Factor::Implied => parts.implied = true,
                Factor::Look(look) => {
                    if let Some(first) = &parts.look {
                        return Err(format!(
                            "two looks multiplied: {} × {}",
                            first.name(),
                            look.name()
                        ));
                    }
                    parts.look = Some(look);
                }
            }
        }
        Ok(parts)
    }
    fn white(&self) -> bool {
        self.color.iter().all(|v| (v - 1.0).abs() < 1e-12)
    }
}

struct Build<'a> {
    clip: Clip,
    host: &'a Host,
    /// The old clip's start and duration; curves over the clip keep their
    /// timing when the start moves.
    original: (f64, f64),
    notes: Vec<String>,
    boundaries: Vec<f64>,
}

/// A clip that starts less than this many beats after an event that is
/// still lit is taken to start on that event.
const SNAP: f64 = 1.0 / 16.0;

fn preset(name: &str) -> FormPreset {
    presets()
        .preset(name)
        .unwrap_or_else(|| panic!("shipped preset {name}"))
        .clone()
}
fn set(form: &mut FormPreset, name: &str, value: Value) {
    let slot = form
        .inputs
        .get_mut(name)
        .unwrap_or_else(|| panic!("{} has no input {name}", form.form));
    *slot = value;
}

impl<'a> Build<'a> {
    fn new(clip: &Clip, host: &'a Host) -> Self {
        Self {
            clip: clip.clone(),
            host,
            original: (clip.start, clip.duration),
            notes: Vec::new(),
            boundaries: Vec::new(),
        }
    }
    fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }
    fn end(&self) -> f64 {
        self.clip.start + self.clip.duration
    }

    // -----------------------------------------------------------------------
    // Light

    fn color(&mut self, parts: Parts) -> Result<(FormPreset, String), String> {
        let Some(look) = parts.look.clone() else {
            let mut form = preset("Wash");
            set(&mut form, "color", Value::Color(parts.color));
            let alpha = self.alpha(&parts);
            set(&mut form, "alpha", alpha);
            return Ok((form, "constant".into()));
        };
        let name = look.name().to_owned();
        let form = match look {
            Look::ColorOverTime(gradient) => {
                let mut form = self.color_time(&parts, "Color fade");
                set(&mut form, "colors", Value::Gradient(gradient));
                form
            }
            Look::Rainbow { repeat } => {
                self.note("rainbow: hue sampled into 64 stops that blend in OKLab");
                let mut form = self.color_time(&parts, "Rainbow");
                set(&mut form, "colors", Value::Gradient(rainbow()));
                set(&mut form, "every", Value::Beats(repeat));
                form
            }
            Look::Harmony(gradient) => {
                let (colors, curve) = self.harmony(&gradient)?;
                let mut form = self.color_time(&parts, "Color fade");
                set(&mut form, "colors", Value::Gradient(colors));
                set(&mut form, "curve", Value::Envelope(curve));
                form
            }
            Look::Pulse {
                trigger,
                duration,
                shape,
            } => {
                let every = self.every(&trigger, duration)?;
                let mut form = self.sparkle(&parts, every, duration);
                set(&mut form, "coverage", Value::Proportion(1.0));
                let brightness = self.hit(&shape);
                set(&mut form, "brightness", brightness);
                form
            }
            Look::Dissolve {
                trigger,
                duration,
                coverage,
                brightness,
                softness,
            } => {
                self.note("sparkle draws its random heads differently from dissolve");
                if softness > 0.0 {
                    self.note(format!("dissolve softness {softness} dropped"));
                }
                let every = self.every(&trigger, duration)?;
                let mut form = self.sparkle(&parts, every, duration);
                let coverage = self.hit(&coverage);
                set(&mut form, "coverage", coverage);
                let brightness = self.hit(&brightness);
                set(&mut form, "brightness", brightness);
                form
            }
            Look::RandomHeads {
                trigger,
                density,
                shuffle,
            } => {
                self.note(if shuffle {
                    "sparkle draws its random heads differently from random heads"
                } else {
                    "random heads without shuffle lit every head once per round; sparkle draws anew each event"
                });
                let Trigger::Periodic { repeat, .. } = trigger else {
                    return Err("random heads without a rhythm".into());
                };
                let every = self.every(&trigger, repeat)?;
                let mut form = self.sparkle(&parts, every, repeat);
                set(&mut form, "coverage", Value::Proportion(density));
                set(&mut form, "brightness", Value::Proportion(1.0));
                form
            }
            Look::Noise {
                period,
                scale,
                shape,
            } => self.noise(parts.clone(), period, scale, &shape)?,
            Look::Stroke(stroke) => self.chase(&parts, *stroke)?,
        };
        Ok((form, name))
    }

    /// A `color.time` preset with the clip's alpha. Its colors come from a
    /// gradient, so a color factor has nowhere to go.
    fn color_time(&mut self, parts: &Parts, name: &str) -> FormPreset {
        if !parts.white() {
            self.note("a color times a gradient: the color is dropped");
        }
        let mut form = preset(name);
        let alpha = self.alpha(parts);
        set(&mut form, "alpha", alpha);
        form
    }

    fn sparkle(&mut self, parts: &Parts, every: Value, duration: f64) -> FormPreset {
        let mut form = preset("Pulse");
        set(&mut form, "color", Value::Color(parts.color));
        set(&mut form, "every", every);
        set(&mut form, "duration", Value::Beats(duration));
        set(&mut form, "grain", Value::Number(1.0));
        let alpha = self.alpha(parts);
        set(&mut form, "alpha", alpha);
        form
    }

    /// An event curve: plain when it is flat.
    fn hit(&mut self, envelope: &Envelope) -> Value {
        if let Some(value) = curves::constant(envelope) {
            return Value::Proportion(value.clamp(0.0, 1.0));
        }
        let (curve, exact) = curves::keyframes(envelope, 1.0);
        if !exact {
            self.note("a Bézier curve sampled into straight pieces");
        }
        Value::Hit(curve)
    }

    fn noise(
        &mut self,
        mut parts: Parts,
        period: f64,
        scale: f64,
        shape: &Envelope,
    ) -> Result<FormPreset, String> {
        let mut form = preset("Drift");
        set(&mut form, "color", Value::Color(parts.color));
        set(&mut form, "speed", Value::Beats(period));
        let size = 1.0 / scale;
        if !(0.0..=1.0).contains(&size) {
            self.note(format!("noise scale {scale} is finer than the form allows"));
        }
        set(&mut form, "scale", Value::Proportion(size.clamp(0.01, 1.0)));
        // A straight shape from zero is a level; the form has no other shape.
        match shape.points.as_slice() {
            [[0.0, 0.0], [1.0, top]] if curves::glides(shape) && shape.curves.is_empty() => {
                parts.scalar *= top;
            }
            _ => self.note("noise shape has no contrast equivalent"),
        }
        set(&mut form, "contrast", Value::Proportion(0.0));
        let alpha = self.alpha(&parts);
        set(&mut form, "alpha", alpha);
        Ok(form)
    }

    fn chase(&mut self, parts: &Parts, stroke: Stroke) -> Result<FormPreset, String> {
        for note in &stroke.notes {
            self.note(note.clone());
        }
        let travel = match stroke.trigger {
            Trigger::Clip => self.clip.duration,
            _ => stroke.travel,
        };
        if let Trigger::Periodic { repeat, .. } = stroke.trigger {
            if stroke.single && travel > repeat + 1e-9 {
                self.note("the old stroke was cut at each event; strokes now overlap");
            }
            if stroke.single && !parts.implied && travel < repeat - 1e-9 {
                self.note("the old stroke stayed at its end until the next event");
            }
        }
        let every = self.every(&stroke.trigger, travel)?;
        // Put the stroke center in unreversed coordinates: reverse maps x to
        // 1 − x, and turns which end of the stroke leads.
        let mut axis = stroke.mapping.clone();
        let reversed = std::mem::replace(&mut axis.reverse, false);
        let (mut a, mut b) = stroke.center;
        if reversed {
            (a, b) = (1.0 - a, -b);
        }
        // The form runs a gliding stroke on an open axis from fully off one
        // end to fully off the other; undo that for the old centers.
        let width = stroke.width;
        if !stroke.wrap && curves::glides(&stroke.path) {
            (a, b) = ((a + width / 2.0) / (1.0 + width), b / (1.0 + width));
        }
        let mut path = curves::affine(&stroke.path, a, b);
        let outside = path
            .points
            .iter()
            .any(|p| !(-1e-12..=1.0 + 1e-12).contains(&p[1]))
            || path.curves.iter().any(|curve| match curve {
                EnvelopeCurve::Bezier { control1, control2 } => {
                    !(0.0..=1.0).contains(&control1[1]) || !(0.0..=1.0).contains(&control2[1])
                }
                _ => false,
            });
        if outside {
            self.note("the path leaves the axis; clamped");
        }
        let clamp = |p: &mut [f64; 2]| p[1] = p[1].clamp(0.0, 1.0);
        path.points.iter_mut().for_each(clamp);
        for curve in &mut path.curves {
            if let EnvelopeCurve::Bezier { control1, control2 } = curve {
                clamp(control1);
                clamp(control2);
            }
        }
        // The form turns an asymmetric shape with the direction of travel;
        // the old graphs kept it facing the mapping's own direction.
        let facing = if reversed { -1 } else { 1 };
        let shape = match curves::direction(&path) {
            0 if !curves::symmetric(&stroke.shape) => {
                self.note("an asymmetric shape now turns with a bouncing path");
                stroke.shape.clone()
            }
            0 => stroke.shape.clone(),
            direction if direction == facing => stroke.shape.clone(),
            _ => curves::mirror(&stroke.shape),
        };
        if width > 1.0 {
            self.note(format!("stroke width {width} is wider than the axis"));
        }
        let mut form = preset("Chase");
        set(&mut form, "color", Value::Color(parts.color));
        set(&mut form, "axis", Value::Mapping(axis));
        set(&mut form, "every", every);
        set(&mut form, "travel", Value::Beats(travel));
        set(&mut form, "width", Value::Proportion(width.clamp(0.0, 1.0)));
        set(&mut form, "width_relative", Value::Boolean(false));
        set(&mut form, "shape", Value::Envelope(shape));
        set(&mut form, "path", Value::Envelope(path));
        set(
            &mut form,
            "boundary",
            Value::Boundary(if stroke.wrap {
                Boundary::Wrap
            } else {
                Boundary::Clip
            }),
        );
        let alpha = self.alpha(parts);
        set(&mut form, "alpha", alpha);
        Ok(form)
    }

    /// Strobe with an optional color product. The strobe form lights white.
    fn strobe(
        &mut self,
        color: Option<Parts>,
        rate: Parts,
    ) -> Result<(FormPreset, String), String> {
        if rate.look.is_some() || !rate.curves.is_empty() || rate.audio.is_some() {
            return Err("a strobe rate that moves".into());
        }
        if rate.gated {
            self.note("the strobe gate on the audio level is dropped");
        }
        let mut form = preset("Strobe");
        set(
            &mut form,
            "rate",
            Value::Proportion(rate.scalar.clamp(0.0, 1.0)),
        );
        match color {
            None => {
                self.note("the strobe form also lights white; the old clip only strobed");
                set(&mut form, "alpha", Value::Proportion(1.0));
            }
            Some(parts) => {
                if let Some(look) = &parts.look {
                    return Err(format!("strobe over {}", look.name()));
                }
                if !parts.white() {
                    self.note("the strobe form lights white; the old color is dropped");
                }
                let alpha = self.alpha(&parts);
                set(&mut form, "alpha", alpha);
            }
        }
        Ok((form, "strobe".into()))
    }

    // -----------------------------------------------------------------------
    // Alpha and timing

    fn alpha(&mut self, parts: &Parts) -> Value {
        if let Some(audio) = &parts.audio {
            if !looks::is_mix(&audio.source) {
                self.note(format!("stem → mix ({})", audio.source.name()));
            }
            self.note("audio level is now scaled over the clip");
            if parts.gated {
                self.note("the threshold on the audio level is dropped");
            }
            if !parts.curves.is_empty() {
                self.note("a curve over the clip is dropped; alpha keeps the audio");
            }
            if (parts.scalar - 1.0).abs() > 1e-12 {
                self.note(format!("level {} on the audio is dropped", parts.scalar));
            }
            return Value::Audio(AudioLevel {
                from_hz: audio.low_hz,
                to_hz: audio.high_hz,
                floor: audio.floor,
            });
        }
        let curve = match parts.curves.as_slice() {
            [] => {
                if !(0.0..=1.0).contains(&parts.scalar) {
                    self.note(format!("level {} clamped to 0–1", parts.scalar));
                }
                return Value::Proportion(parts.scalar.clamp(0.0, 1.0));
            }
            [curve] => {
                let (curve, exact) = curves::keyframes(curve, parts.scalar);
                if !exact {
                    self.note("a Bézier curve sampled into straight pieces");
                }
                curve
            }
            several => {
                self.note(format!("a product of {} curves sampled", several.len()));
                let refs: Vec<&Envelope> = several.iter().collect();
                curves::resample(
                    |x| parts.scalar * several.iter().map(|c| c.sample(x)).product::<f64>(),
                    &curves::breaks(&refs),
                )
            }
        };
        let cut = (self.clip.start - self.original.0) / self.original.1;
        let (mut curve, exact) = curves::crop(&curve, cut);
        if !exact {
            self.note("a curve cut inside an eased segment");
        }
        if curve.values().any(|v| !(0.0..=1.0).contains(&v)) {
            self.note("a curve above 1 clamped");
            for (_, key) in &mut curve.points {
                if let luma_patterns::Key::Number(v) = key {
                    *v = v.clamp(0.0, 1.0);
                }
            }
        }
        Value::Time(curve)
    }

    /// The `every` value for events of `trigger` whose life is `life` beats.
    ///
    /// A form's events start at the clip start, so periodic events move the
    /// clip start to the first event. When an event just before the clip is
    /// still lit at its start — a clip placed a hair late — the start moves
    /// back to that event instead.
    fn every(&mut self, trigger: &Trigger, life: f64) -> Result<Value, String> {
        let (start, end) = (self.clip.start, self.end());
        let (events, value) = match *trigger {
            Trigger::Clip => {
                self.boundaries.push(start);
                return Ok(Value::Beats(self.clip.duration));
            }
            Trigger::Drum(drum) => {
                let onsets = self
                    .host
                    .onsets
                    .get(&drum)
                    .ok_or_else(|| format!("the track has no {} onsets", drum.name()))?;
                let lit: Vec<f64> = onsets
                    .iter()
                    .copied()
                    .filter(|t| *t < start && *t + life > start + 1e-9)
                    .collect();
                self.lit_before(&lit);
                let start = self.clip.start;
                let stamps: Vec<f64> = onsets
                    .iter()
                    .filter(|t| **t >= start - 1e-9 && **t < end)
                    .map(|t| (t - start).max(0.0))
                    .collect();
                let events: Vec<f64> = stamps.iter().map(|t| start + t).collect();
                let value = Value::Events(Events::Beats {
                    times: EventTimes::new(stamps).map_err(|e| e.to_string())?,
                });
                (events, value)
            }
            Trigger::Periodic {
                repeat,
                grid_aligned,
                delay,
            } => {
                if repeat <= 0.0 || !repeat.is_finite() {
                    return Err(format!("repeat {repeat}"));
                }
                let origin = if grid_aligned { 0.0 } else { start } + delay;
                let first = origin + ((start - origin) / repeat - 1e-9).ceil() * repeat;
                let mut lit: Vec<f64> = (1..=64)
                    .map(|k| first - f64::from(k) * repeat)
                    .take_while(|at| at + life > start + 1e-9)
                    .collect();
                lit.reverse();
                if self.lit_before(&lit) {
                    // The clip now starts on the event before it.
                } else if first >= end - 1e-3 {
                    self.note("no event starts inside the clip");
                    let none = EventTimes::new(Vec::new()).map_err(|e| e.to_string())?;
                    return Ok(Value::Events(Events::Beats { times: none }));
                } else if first - start > 1e-9 {
                    self.note(format!(
                        "start moved {:.4} beats to the first event",
                        first - start
                    ));
                    self.move_start(first);
                }
                let events = (0..)
                    .map(|k| self.clip.start + f64::from(k) * repeat)
                    .take_while(|at| *at < end)
                    .take(24)
                    .collect();
                (events, Value::Beats(repeat))
            }
        };
        for at in events.into_iter().take(32) {
            self.boundaries.extend([at, at + life]);
        }
        Ok(value)
    }

    /// `lit` are the events before the clip, in order, that are still lit at
    /// its start. A start less than [`SNAP`] after the first of them moves
    /// back onto it; otherwise the light they leave at the start is lost.
    /// True when the start moved.
    fn lit_before(&mut self, lit: &[f64]) -> bool {
        let Some(event) = lit.first().copied() else {
            return false;
        };
        let gap = self.clip.start - event;
        if gap < SNAP {
            self.note(format!(
                "start moved back {gap:.4} beats to the event just before it"
            ));
            self.move_start(event);
            true
        } else {
            self.note("an event before the clip is still lit at its start");
            false
        }
    }

    /// Start the clip at `beat`, keeping its end. Curves over the clip keep
    /// their timing.
    fn move_start(&mut self, beat: f64) {
        let end = self.end();
        self.clip.start = beat;
        self.clip.duration = end - beat;
    }

    /// The track's chord colors over the clip, as a palette gradient read by
    /// a stepped curve.
    fn harmony(&mut self, gradient: &Gradient) -> Result<(Gradient, Envelope), String> {
        let (start, end) = (self.clip.start, self.end());
        let mut cuts = vec![start];
        for (a, b, _) in &self.host.chords {
            cuts.extend([*a, *b].into_iter().filter(|t| *t > start && *t < end));
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let color_at = |beat: f64| {
            self.host
                .chords
                .iter()
                .rev()
                .find(|(a, b, _)| beat >= *a && beat < *b)
                .and_then(|(_, _, root)| *root)
                .map_or([0.0; 3], |root| {
                    gradient.sample((f64::from(root) / 11.0).clamp(0.0, 1.0))
                })
        };
        let mut segments: Vec<(f64, [f64; 3])> = Vec::new();
        for (i, cut) in cuts.iter().enumerate() {
            let next = cuts.get(i + 1).copied().unwrap_or(end);
            let color = color_at((cut + next) / 2.0);
            if segments.last().is_none_or(|(_, last)| *last != color) {
                segments.push((*cut, color));
            }
        }
        let mut palette: Vec<[f64; 3]> = Vec::new();
        for (_, color) in &segments {
            if !palette.contains(color) {
                palette.push(*color);
            }
        }
        if palette.len() > 64 || segments.len() > 255 {
            return Err("more chord colors than a gradient holds".into());
        }
        let last = (palette.len().max(2) - 1) as f64;
        let position =
            |color: &[f64; 3]| palette.iter().position(|c| c == color).unwrap() as f64 / last;
        let mut stops: Vec<ColorStop> = palette
            .iter()
            .enumerate()
            .map(|(i, color)| ColorStop {
                t: i as f64 / last,
                color: *color,
                alpha: 1.0,
            })
            .collect();
        if stops.len() == 1 {
            stops.push(ColorStop {
                t: 1.0,
                ..stops[0].clone()
            });
        }
        let mut points: Vec<[f64; 2]> = segments
            .iter()
            .map(|(at, color)| [(at - start) / self.clip.duration, position(color)])
            .collect();
        points.push([1.0, points.last().unwrap()[1]]);
        self.boundaries.extend(segments.iter().map(|(at, _)| *at));
        Ok((
            Gradient { stops },
            Envelope {
                curves: vec![EnvelopeCurve::Hold; points.len() - 1],
                points,
            },
        ))
    }

    // -----------------------------------------------------------------------
    // Subsets

    /// A stored `subset` lit a share of the selected heads. The selection
    /// reader drops it; a constant color becomes a sparkle with that share.
    fn subset(
        &mut self,
        selection: &serde_json::Value,
        form: FormPreset,
        look: String,
    ) -> Result<(FormPreset, String), String> {
        let Some(subset) = selection.get("subset") else {
            return Ok((form, look));
        };
        let share = if let Some(fraction) = subset.get("fraction").and_then(|v| v.as_f64()) {
            fraction
        } else if let Some(count) = subset.get("count").and_then(|v| v.as_f64()) {
            if self.host.heads == 0 {
                return Err("a subset count on a selection with no heads".into());
            }
            count / self.host.heads as f64
        } else {
            return Err(format!("subset {subset}"));
        };
        self.note("subset: the old render lit the whole group (the reader drops subsets)");
        if form.form != "color.constant@1" {
            self.note(format!("subset {subset} dropped from a {look} clip"));
            return Ok((form, look));
        }
        let mut sparkle = preset("Random heads");
        for input in ["color", "alpha"] {
            set(&mut sparkle, input, form.inputs[input].clone());
        }
        set(&mut sparkle, "every", Value::Beats(self.clip.duration));
        set(&mut sparkle, "duration", Value::Beats(self.clip.duration));
        set(
            &mut sparkle,
            "coverage",
            Value::Proportion(share.clamp(0.0, 1.0)),
        );
        Ok((sparkle, "constant with a subset".into()))
    }
}

/// Full-saturation hue from 0 to 1 turn in 64 stops.
fn rainbow() -> Gradient {
    Gradient {
        stops: (0..64)
            .map(|i| {
                let t = f64::from(i) / 63.0;
                ColorStop {
                    t,
                    color: hsv(t),
                    alpha: 1.0,
                }
            })
            .collect(),
    }
}

/// The `hsv` node at full saturation and value.
fn hsv(hue: f64) -> [f64; 3] {
    let h = hue.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h.rem_euclid(2.0) - 1.0).abs();
    match h.floor() as u8 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    }
}
