//! Form clip inputs. A form clip names a shipped form and holds a value for
//! each of its inputs. The sheet shows them in the form's order, under the
//! engine's names. Where the form allows it, an input can hold a source in
//! place of a plain value: a curve over the clip or over each hit, noise, the
//! level of a frequency range of the mix.

use super::*;
use luma_lib::node_graph::lighting::decode;
use luma_patterns as p;
use luma_ui::arg::envelope::{EnvelopeChanged, EnvelopeEditor};
use luma_ui::arg::number::format_value;
use luma_ui::arg::preset_picker::{luma_preset_picker, Thumb};

/// Inputs in beats that must stay above zero.
const SPEEDS: [&str; 4] = ["every", "travel", "duration", "speed"];
/// The least beats a speed input takes.
const MIN_BEATS: f64 = 1. / 32.;
/// The top of a speed curve's value axis, in beats.
const CURVE_BEATS: f64 = 8.;
/// Beats a new noise source takes to wander once.
const NOISE_SPEED: f64 = 4.;
/// The grain choices. A clump holds N heads; N is its own field.
const GRAINS: [&str; 3] = ["Head", "Fixture", "Clump"];
/// The two readings of a period that can be 0: once over the clip, or beats.
const EVERY: [&str; 2] = ["Once", "Beats"];
/// The period a switch from "Once" to "Beats" starts at.
const EVERY_BEATS: f64 = 4.;
/// How far from the venue's origin a point field reaches, in metres.
const POINT_REACH: f64 = 100.;

/// A form input's row: what the form says about the input, and which source
/// the stored value holds.
#[derive(Clone, Copy)]
pub(super) struct Slot {
    form: &'static str,
    key: &'static str,
    spec: &'static p::Input,
    speed: bool,
    mode: Option<p::SourceKind>,
    /// Custom was chosen in the row's preset picker: its editor shows even
    /// while the value matches a preset.
    editing: bool,
}

impl Slot {
    /// The slot for `def` when `form` is a shipped form that has it.
    pub(super) fn new(form: &str, def: &PatternArgDef, stored: &serde_json::Value) -> Option<Self> {
        let form = *p::FORMS.iter().find(|id| **id == form)?;
        let (key, spec) = document::form_definition(form)?
            .inputs
            .get_key_value(&def.id)?;
        Some(Self {
            form,
            key,
            spec,
            speed: SPEEDS.contains(&def.id.as_str()),
            mode: decode(spec.value_type, stored)
                .ok()
                .and_then(|value| value.source_kind()),
            editing: false,
        })
    }

    /// A period where the sheet offers 0 beats, once over the clip: `every`
    /// of color over time, the Wash and the chase, and a chase's `travel`.
    fn once(&self) -> bool {
        match self.form {
            "color.time@1" | "color.constant@1" => self.key == "every",
            "color.chase@1" => matches!(self.key, "every" | "travel"),
            _ => false,
        }
    }

    /// The bounds the form gives a number input, such as a chase width.
    fn bounds(&self) -> Option<[f64; 2]> {
        match self.spec.author {
            Some(p::Author::Number {
                min: Some(min),
                max: Some(max),
            }) => Some([min, max]),
            _ => None,
        }
    }

    /// The span a number curve's value axis covers, in the input's unit. A
    /// vector's noise wanders on each of U, V and Z within −1 to 1.
    fn range(&self) -> [f64; 2] {
        if self.speed {
            [0., CURVE_BEATS]
        } else if self.vector() {
            [-1., 1.]
        } else {
            self.bounds().unwrap_or([0., 1.])
        }
    }

    /// A U, V, Z input: an aim direction or a point.
    fn vector(&self) -> bool {
        matches!(self.spec.default, Some(p::Value::Vector(_)))
    }

    /// The unit a number field of this input shows: beats for a speed,
    /// degrees for an aim's fan, size and spread.
    fn unit(&self) -> Option<&'static str> {
        if self.speed {
            Some("beats")
        } else if self.form == "aim@1" && matches!(self.key, "fan" | "size" | "spread") {
            Some("°")
        } else {
            None
        }
    }

    /// `low–high` in the input's unit, for a caption.
    fn span_text(&self, [low, high]: [f64; 2]) -> String {
        let unit = match self.unit() {
            Some("°") => "°".to_string(),
            Some(unit) => format!(" {unit}"),
            None => String::new(),
        };
        format!("{}–{}{unit}", format_value(low), format_value(high))
    }

    /// The shipped curve presets this input offers.
    fn curves(&self) -> impl Iterator<Item = &'static p::CurvePreset> {
        p::presets().curves_for(self.key)
    }

    /// A number the input accepts: speeds stay above zero, the rest within
    /// the input's bounds, 0–1 by default.
    fn fit(&self, value: f64) -> f64 {
        if self.speed {
            value.max(MIN_BEATS)
        } else {
            let [low, high] = self.range();
            value.clamp(low, high)
        }
    }

    /// A plain number in the input's own unit.
    fn plain(&self, value: f64) -> p::Value {
        match self.spec.default {
            Some(p::Value::Beats(_)) => p::Value::Beats(value),
            Some(p::Value::Proportion(_)) => p::Value::Proportion(value),
            _ => p::Value::Number(value),
        }
    }
}

/// What the promote menu calls each source.
fn source_label(kind: p::SourceKind) -> &'static str {
    match kind {
        p::SourceKind::Time => "↗ Over time",
        p::SourceKind::Hit => "↗ Per hit",
        p::SourceKind::Noise => "↗ Noise",
        p::SourceKind::Audio => "↗ Audio",
    }
}

/// What the promote menu calls a value that is not a source.
const FIXED: &str = "Fixed";

fn level(value: &p::Value) -> Option<f64> {
    match value {
        p::Value::Beats(v) | p::Value::Proportion(v) | p::Value::Number(v) => Some(*v),
        _ => None,
    }
}

/// `value` converted to `to`. A plain value becomes a source that starts at
/// it; a source becomes the plain value it starts at.
fn promote(slot: &Slot, value: &p::Value, to: Option<p::SourceKind>) -> p::Value {
    // A color or a vector: three channels.
    let triple = match value {
        p::Value::Color(rgb) | p::Value::Vector(rgb) => Some(*rgb),
        p::Value::Time(curve) | p::Value::Hit(curve) if curve.is_color() => Some(curve.sample(0.)),
        _ => None,
    }
    .or_else(|| match (slot.vector(), &slot.spec.default) {
        (true, Some(p::Value::Vector(v))) => Some(*v),
        _ => None,
    });
    let number = match value {
        p::Value::Time(curve) | p::Value::Hit(curve) => Some(curve.sample(0.)[0]),
        p::Value::Noise(p::NoiseSource { range, .. }) => Some(range[1]),
        // Audio at its loudest gives the top of the input: 1, or the most
        // degrees of a fan or a size.
        p::Value::Audio(_) => Some(slot.range()[1]),
        other => level(other),
    }
    .or_else(|| slot.spec.default.as_ref().and_then(level))
    .unwrap_or(1.);
    let number = slot.fit(number);
    let key = triple.map_or(p::Key::Number(number), p::Key::Color);
    let flat = || p::Keyframes::with_eases([(0., key), (1., key)], &[]);
    match to {
        None => match triple {
            Some(v) if slot.vector() => p::Value::Vector(v),
            Some(rgb) => p::Value::Color(rgb),
            None => slot.plain(number),
        },
        Some(p::SourceKind::Time) => p::Value::Time(flat()),
        Some(p::SourceKind::Hit) => p::Value::Hit(flat()),
        Some(p::SourceKind::Noise) => p::Value::Noise(p::NoiseSource {
            speed: NOISE_SPEED,
            range: if slot.vector() {
                slot.range()
            } else {
                [number.min(0.), number.max(0.)]
            },
        }),
        Some(p::SourceKind::Audio) => {
            let first = &p::presets().frequencies[0];
            p::Value::Audio(p::AudioLevel {
                from_hz: first.from_hz,
                to_hz: first.to_hz,
                floor: 0.,
                threshold: 0.,
            })
        }
    }
}

/// A named curve preset (values 0–1) scaled to the input's range. Eases are
/// local to their segments, so they keep their shape.
fn scaled(slot: &Slot, curve: &p::Keyframes) -> p::Keyframes {
    let [low, high] = slot.range();
    curve.map(|key| match key {
        p::Key::Number(v) => p::Key::Number(slot.fit(low + v * (high - low))),
        color => *color,
    })
}

fn same_curve(a: &p::Keyframes, b: &p::Keyframes) -> bool {
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-9;
    let same_ease = |a: p::Ease, b: p::Ease| match (a, b) {
        (p::Ease::Bezier(a), p::Ease::Bezier(b)) => a.iter().zip(&b).all(|(a, b)| close(*a, *b)),
        (a, b) => a == b,
    };
    a.points.len() == b.points.len()
        && a.points.iter().zip(&b.points).all(|(a, b)| {
            close(a.x, b.x)
                && same_ease(a.ease, b.ease)
                && match (a.value, b.value) {
                    (p::Key::Number(a), p::Key::Number(b)) => close(a, b),
                    (p::Key::Color(a), p::Key::Color(b)) => {
                        a.iter().zip(&b).all(|(a, b)| close(*a, *b))
                    }
                    _ => false,
                }
        })
}

/// A number curve in the envelope editor's 0–1 box.
fn envelope_of(slot: &Slot, curve: &p::Keyframes) -> p::Envelope {
    let [low, high] = slot.range();
    let envelope = curve.map(|key| {
        let value = match key {
            p::Key::Number(v) => *v,
            p::Key::Color(_) => low,
        };
        ((value - low) / (high - low)).clamp(0., 1.)
    });
    if envelope.validate().is_ok() {
        envelope
    } else {
        p::Envelope::linear(vec![[0., 0.], [1., 0.]])
    }
}

/// The envelope editor's box back as a curve in the input's unit. The eases
/// are the same, so what is drawn is what plays.
fn keyframes_of(slot: &Slot, envelope: &p::Envelope) -> p::Keyframes {
    let [low, high] = slot.range();
    envelope.map(|y| p::Key::Number(slot.fit(low + y * (high - low))))
}

/// A color curve as gradient stops: each point is a stop at its progress.
fn gradient_of(curve: &p::Keyframes) -> Gradient {
    Gradient::new(curve.points.iter().map(|point| {
        let [r, g, b] = match point.value {
            p::Key::Color(rgb) => rgb,
            p::Key::Number(v) => [v; 3],
        };
        GradientStop {
            t: point.x as f32,
            color: Rgba {
                r: r as f32,
                g: g as f32,
                b: b as f32,
                a: 1.,
            },
        }
    }))
}

/// Gradient stops as a color curve from 0 to 1. The end colors hold out to
/// the ends, and of two stops at one place the first is kept.
fn color_keys(gradient: &Gradient) -> p::Keyframes {
    let mut points: Vec<(f64, p::Key)> = Vec::new();
    for stop in gradient.stops() {
        let t = f64::from(stop.t).clamp(0., 1.);
        let color = p::Key::Color([
            f64::from(stop.color.r),
            f64::from(stop.color.g),
            f64::from(stop.color.b),
        ]);
        if points.is_empty() && t > 0. {
            points.push((0., color));
        }
        if points.last().is_none_or(|(x, _)| t > *x) {
            points.push((t, color));
        }
    }
    if let Some(&(_, color)) = points.last().filter(|(x, _)| *x < 1.) {
        points.push((1., color));
    }
    p::Keyframes::with_eases(points, &[])
}

/// A direction as turn and tilt, in degrees. Turn 0 is downstage and grows
/// toward stage right; tilt 0 is level and −90 is straight down. A direction
/// straight up or down has no turn: `None`.
fn turn_tilt([u, v, z]: [f64; 3]) -> (Option<f64>, f64) {
    let flat = u.hypot(v);
    let tilt = z.atan2(flat).to_degrees();
    let turn = (flat > 1e-6).then(|| u.atan2(v).to_degrees());
    (turn, tilt)
}

/// The unit direction at `turn` and `tilt` degrees, to four places.
fn direction_at(turn: f64, tilt: f64) -> [f64; 3] {
    let (turn, tilt) = (turn.to_radians(), tilt.to_radians());
    [tilt.cos() * turn.sin(), tilt.cos() * turn.cos(), tilt.sin()]
        .map(|v| (v * 1e4).round() / 1e4 + 0.)
}

/// A stored vector, spelled for a caption: "U 0.00 · V 0.77 · Z −0.64".
fn vector_text(v: [f64; 3]) -> String {
    let part = |name: &str, v: f64| {
        let text = format!("{:.2}", v.abs());
        let sign = if v < 0. && text != "0.00" { "−" } else { "" };
        format!("{name} {sign}{text}")
    };
    format!(
        "{} · {} · {}",
        part("U", v[0]),
        part("V", v[1]),
        part("Z", v[2])
    )
}

/// The vectors a vector input shows: its value when fixed, the start and
/// the end of a curve over the clip.
fn vector_ends(value: &p::Value) -> Vec<[f64; 3]> {
    let key = |key: &p::Key| match key {
        p::Key::Color(v) => *v,
        p::Key::Number(n) => [*n; 3],
    };
    match value {
        p::Value::Vector(v) => vec![*v],
        p::Value::Time(curve) => match (curve.points.first(), curve.points.last()) {
            (Some(first), Some(last)) => vec![key(&first.value), key(&last.value)],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// `value` with end `end` (see [`vector_ends`]) passed through `edit`: the
/// vector itself, or the first or last point of a curve over the clip.
fn edit_vector_end(
    value: &p::Value,
    end: usize,
    edit: impl FnOnce([f64; 3]) -> [f64; 3],
) -> Option<p::Value> {
    match value {
        p::Value::Vector(v) => Some(p::Value::Vector(edit(*v))),
        p::Value::Time(curve) if curve.is_color() => {
            let mut curve = curve.clone();
            let at = if end == 0 { 0 } else { curve.points.len() - 1 };
            let p::Key::Color(v) = curve.points[at].value else {
                return None;
            };
            curve.points[at].value = p::Key::Color(edit(v));
            Some(p::Value::Time(curve))
        }
        _ => None,
    }
}

// -- widgets ------------------------------------------------------------------

/// The control for a form input's current value.
pub(super) fn widget(
    slot: &Slot,
    def: &PatternArgDef,
    stored: &serde_json::Value,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Widget {
    let value = decode(slot.spec.value_type, stored).ok();
    let spec = slot.spec;
    let name = spec.name.clone();
    let number = |label: String,
                  value: f64,
                  [min, max]: [f64; 2],
                  width: f32,
                  unit: Option<&'static str>,
                  window: &mut Window,
                  cx: &mut Context<Luma>| {
        cx.new(|cx| {
            let field = DraftedNumber::new(label, value, min, max, width, window, cx);
            match unit {
                Some("beats") => field.with_unit("beats").with_per_unit("per beat", cx),
                Some(unit) => field.with_unit(unit),
                None => field,
            }
        })
    };
    // Each field edits one part of the stored value.
    let on_number =
        |field: &Entity<DraftedNumber>, cx: &mut Context<Luma>, edit: fn(&mut p::Value, f64)| {
            let def = def.clone();
            cx.subscribe(field, move |this: &mut Luma, _, event: &NumberEvent, cx| {
                let NumberEvent::Committed(number) = *event;
                this.form_edit(&def, spec, cx, |value| {
                    let mut value = value.clone();
                    edit(&mut value, number);
                    Some(value)
                });
            })
        };
    match value {
        // A direction is edited as turn and tilt, which the row draws; a
        // point as U, V and Z in metres. A curve edits its two ends.
        Some(value) if slot.vector() && !vector_ends(&value).is_empty() => {
            let ends = vector_ends(&value);
            if slot.key == "direction" {
                return Widget::Direction(
                    ends.iter().map(|v| turn_tilt(*v).0.unwrap_or(0.)).collect(),
                );
            }
            let names = if ends.len() == 1 {
                vec![""]
            } else {
                vec!["Start ", "End "]
            };
            let third = (FIELD_W - 16.) / 3.;
            Widget::Point(
                ends.iter()
                    .zip(names)
                    .enumerate()
                    .map(|(end, (v, prefix))| {
                        std::array::from_fn(|axis| {
                            let field = number(
                                format!("{name}: {prefix}{}", ["U", "V", "Z"][axis]),
                                v[axis],
                                [-POINT_REACH, POINT_REACH],
                                third,
                                Some("m"),
                                window,
                                cx,
                            );
                            let def = def.clone();
                            subs.push(cx.subscribe(
                                &field,
                                move |this: &mut Luma, _, event: &NumberEvent, cx| {
                                    let NumberEvent::Committed(n) = *event;
                                    this.form_edit(&def, spec, cx, |value| {
                                        edit_vector_end(value, end, |mut v| {
                                            v[axis] = n;
                                            v
                                        })
                                    });
                                },
                            ));
                            field
                        })
                    })
                    .collect(),
            )
        }
        Some(p::Value::Time(curve) | p::Value::Hit(curve)) if curve.is_color() => {
            let hit = slot.mode == Some(p::SourceKind::Hit);
            let entity =
                cx.new(|cx| GradientEditor::new(gradient_of(&curve), window, cx).without_alpha(cx));
            let def = def.clone();
            subs.push(cx.subscribe(
                &entity,
                move |this: &mut Luma, _, event: &GradientChanged, cx| {
                    let curve = color_keys(&event.0);
                    let value = if hit {
                        p::Value::Hit(curve)
                    } else {
                        p::Value::Time(curve)
                    };
                    this.arg_live(&def.id, document::wire_value(&value), cx);
                },
            ));
            Widget::Gradient(entity)
        }
        Some(p::Value::Time(curve) | p::Value::Hit(curve)) => {
            let hit = slot.mode == Some(p::SourceKind::Hit);
            let entity =
                cx.new(|_| EnvelopeEditor::new(envelope_of(slot, &curve)).without_presets());
            let def = def.clone();
            let shape = *slot;
            subs.push(cx.subscribe(
                &entity,
                move |this: &mut Luma, _, event: &EnvelopeChanged, cx| {
                    let curve = keyframes_of(&shape, &event.0);
                    let value = if hit {
                        p::Value::Hit(curve)
                    } else {
                        p::Value::Time(curve)
                    };
                    this.arg_live(&def.id, document::wire_value(&value), cx);
                },
            ));
            Widget::Envelope(entity)
        }
        Some(p::Value::Noise(noise)) => {
            let half = (FIELD_W - 8.) / 2.;
            let speed = number(
                format!("{name}: Speed"),
                noise.speed,
                [MIN_BEATS, 1e9],
                FIELD_W,
                Some("beats"),
                window,
                cx,
            );
            let low = number(
                format!("{name}: Low"),
                noise.range[0],
                slot.range(),
                half,
                slot.unit(),
                window,
                cx,
            );
            let high = number(
                format!("{name}: High"),
                noise.range[1],
                slot.range(),
                half,
                slot.unit(),
                window,
                cx,
            );
            subs.push(on_number(&speed, cx, |value, v| {
                if let p::Value::Noise(noise) = value {
                    noise.speed = v;
                }
            }));
            subs.push(on_number(&low, cx, |value, v| {
                if let p::Value::Noise(noise) = value {
                    noise.range[0] = v;
                }
            }));
            subs.push(on_number(&high, cx, |value, v| {
                if let p::Value::Noise(noise) = value {
                    noise.range[1] = v;
                }
            }));
            Widget::Noise([speed, low, high])
        }
        Some(p::Value::Audio(audio)) => {
            let half = (FIELD_W - 8.) / 2.;
            let (min, max) = (p::AudioLevel::MIN_HZ, p::AudioLevel::MAX_HZ);
            let hz = |label: String, value: f64, window: &mut Window, cx: &mut Context<Luma>| {
                cx.new(|cx| {
                    DraftedNumber::new(label, value, min, max, half, window, cx).with_unit("Hz")
                })
            };
            let from = hz(format!("{name}: From"), audio.from_hz, window, cx);
            let to = hz(format!("{name}: To"), audio.to_hz, window, cx);
            let percent = |label: &str, value: f64, window: &mut Window, cx: &mut Context<Luma>| {
                cx.new(|cx| {
                    DraftedNumber::new(
                        format!("{name}: {label}"),
                        value * 100.,
                        0.,
                        100.,
                        FIELD_W,
                        window,
                        cx,
                    )
                    .with_unit("%")
                })
            };
            let floor = percent("Floor", audio.floor, window, cx);
            let threshold = percent("Threshold", audio.threshold, window, cx);
            // A range that would cross is not stored.
            subs.push(on_number(&from, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.from_hz = v.min(audio.to_hz - 1.);
                }
            }));
            subs.push(on_number(&to, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.to_hz = v.max(audio.from_hz + 1.);
                }
            }));
            subs.push(on_number(&floor, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.floor = (v / 100.).clamp(0., 1.);
                }
            }));
            subs.push(on_number(&threshold, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.threshold = (v / 100.).clamp(0., 1.);
                }
            }));
            Widget::Audio([from, to, floor, threshold])
        }
        value => {
            let current = value.as_ref().and_then(level);
            if def.id == "grain" {
                let entity = number(
                    format!("{name}: Clump size"),
                    current.filter(|n| *n >= 2.).unwrap_or(2.),
                    [2., 64.],
                    FIELD_W,
                    None,
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Number(v.round())
                }));
                return Widget::Grain(entity);
            }
            if slot.once() {
                let entity = number(
                    format!("{name}: Beats"),
                    current.unwrap_or(0.),
                    [0., 1e9],
                    FIELD_W,
                    Some("beats"),
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Beats(v)
                }));
                return Widget::Every(entity);
            }
            if slot.speed {
                // A speed stays above zero.
                let entity = number(
                    name,
                    slot.fit(current.unwrap_or(MIN_BEATS)),
                    [MIN_BEATS, 1e9],
                    FIELD_W,
                    Some("beats"),
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Beats(v)
                }));
                return Widget::Scalar(entity);
            }
            if let Some(p::Value::Mapping(mapping)) = &value {
                let normal = plane_normal(mapping);
                let third = (FIELD_W - 16.) / 3.;
                let [u, v, z] = std::array::from_fn(|axis| {
                    number(
                        format!("{name}: Plane {}", ["U", "V", "Z"][axis]),
                        normal[axis],
                        [-1e9, 1e9],
                        third,
                        None,
                        window,
                        cx,
                    )
                });
                subs.push(on_number(&u, cx, |value, n| set_normal(value, 0, n)));
                subs.push(on_number(&v, cx, |value, n| set_normal(value, 1, n)));
                subs.push(on_number(&z, cx, |value, n| set_normal(value, 2, n)));
                let mirror = mirror_plane(mapping);
                let normal: [_; 3] = std::array::from_fn(|axis| {
                    number(
                        format!("{name}: Mirror {}", ["U", "V", "Z"][axis]),
                        mirror.normal[axis],
                        [-1e9, 1e9],
                        third,
                        None,
                        window,
                        cx,
                    )
                });
                subs.push(on_number(&normal[0], cx, |value, n| {
                    set_mirror(value, |plane| plane.normal[0] = n)
                }));
                subs.push(on_number(&normal[1], cx, |value, n| {
                    set_mirror(value, |plane| plane.normal[1] = n)
                }));
                subs.push(on_number(&normal[2], cx, |value, n| {
                    set_mirror(value, |plane| plane.normal[2] = n)
                }));
                let offset = number(
                    format!("{name}: Mirror offset"),
                    mirror.offset,
                    [-1e9, 1e9],
                    FIELD_W,
                    Some("m"),
                    window,
                    cx,
                );
                subs.push(on_number(&offset, cx, |value, n| {
                    set_mirror(value, |plane| plane.offset = n)
                }));
                return Widget::Axis(AxisFields {
                    plane: [u, v, z],
                    normal,
                    offset,
                    custom_mirror: Rc::new(std::cell::Cell::new(
                        mirror_index(mapping.mirror.as_ref()) == CUSTOM_MIRROR,
                    )),
                });
            }
            if let Some(p::Author::Choice { options, .. }) = &spec.author {
                // A choice of curves edits a custom curve in the envelope editor.
                let editor = envelope_options(options).map(|curves| {
                    let envelope = match &value {
                        Some(p::Value::Envelope(envelope)) if envelope.validate().is_ok() => {
                            envelope.clone()
                        }
                        _ => match &curves[0].1 {
                            Thumb::Curve(envelope) => envelope.clone(),
                            Thumb::Gradient(_) => unreachable!("curve options"),
                        },
                    };
                    let entity = cx.new(|_| EnvelopeEditor::new(envelope).without_presets());
                    let def = def.clone();
                    subs.push(cx.subscribe(
                        &entity,
                        move |this: &mut Luma, _, event: &EnvelopeChanged, cx| {
                            let value = p::Value::Envelope(event.0.clone());
                            this.arg_live(&def.id, document::wire_value(&value), cx);
                        },
                    ));
                    entity
                });
                return Widget::Preset(options, editor);
            }
            if let Some([min, max]) = slot.bounds() {
                // A number with bounds of its own, such as a chase width.
                let entity = number(
                    name,
                    slot.fit(current.unwrap_or(min)),
                    [min, max],
                    FIELD_W,
                    slot.unit(),
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Number(v)
                }));
                return Widget::Scalar(entity);
            }
            plain_widget(def, stored, true, &[], window, cx, subs)
        }
    }
}

/// Push a stored value into a form row's controls. Rebuilds the control when
/// the value changed between plain and a source, or between two sources.
/// Returns false for a plain control the sheet itself refreshes.
pub(super) fn resync(
    slot: &mut Slot,
    def: &PatternArgDef,
    widget: &mut Widget,
    stored: &serde_json::Value,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> bool {
    let value = decode(slot.spec.value_type, stored).ok();
    let mode = value.as_ref().and_then(p::Value::source_kind);
    if mode != slot.mode {
        slot.mode = mode;
        *widget = self::widget(slot, def, stored, window, cx, subs);
        return true;
    }
    match (&mut *widget, value) {
        (Widget::Direction(turns), Some(value)) => {
            // A direction straight up or down keeps the turn it had.
            for (held, v) in turns.iter_mut().zip(vector_ends(&value)) {
                if let (Some(turn), _) = turn_tilt(v) {
                    *held = turn;
                }
            }
        }
        (Widget::Point(sets), Some(value)) => {
            for (fields, v) in sets.iter().zip(vector_ends(&value)) {
                for (field, n) in fields.iter().zip(v) {
                    field.update(cx, |field, cx| field.set_value(n, cx));
                }
            }
        }
        (Widget::Envelope(entity), Some(p::Value::Time(curve) | p::Value::Hit(curve))) => {
            let envelope = envelope_of(slot, &curve);
            entity.update(cx, |editor, cx| editor.set_value(envelope, cx));
        }
        (Widget::Gradient(entity), Some(p::Value::Time(curve) | p::Value::Hit(curve))) => {
            let gradient = gradient_of(&curve);
            entity.update(cx, |editor, cx| editor.set_value(gradient, cx));
        }
        (Widget::Noise([speed, low, high]), Some(p::Value::Noise(noise))) => {
            speed.update(cx, |field, cx| field.set_value(noise.speed, cx));
            low.update(cx, |field, cx| field.set_value(noise.range[0], cx));
            high.update(cx, |field, cx| field.set_value(noise.range[1], cx));
        }
        (Widget::Audio([from, to, floor, threshold]), Some(p::Value::Audio(audio))) => {
            from.update(cx, |field, cx| field.set_value(audio.from_hz, cx));
            to.update(cx, |field, cx| field.set_value(audio.to_hz, cx));
            floor.update(cx, |field, cx| field.set_value(audio.floor * 100., cx));
            threshold.update(cx, |field, cx| field.set_value(audio.threshold * 100., cx));
        }
        (Widget::Grain(entity), Some(value)) => {
            if let Some(n) = level(&value).filter(|n| *n >= 2.) {
                entity.update(cx, |field, cx| field.set_value(n, cx));
            }
        }
        (Widget::Every(entity), Some(value)) => {
            if let Some(n) = level(&value) {
                entity.update(cx, |field, cx| field.set_value(n, cx));
            }
        }
        (Widget::Preset(_, Some(entity)), Some(p::Value::Envelope(envelope))) => {
            if envelope.validate().is_ok() {
                entity.update(cx, |editor, cx| editor.set_value(envelope, cx));
            }
        }
        (Widget::Preset(..), _) => {}
        (Widget::Axis(fields), Some(p::Value::Mapping(mapping))) => {
            for (field, n) in fields.plane.iter().zip(plane_normal(&mapping)) {
                field.update(cx, |field, cx| field.set_value(n, cx));
            }
            let mirror = mirror_plane(&mapping);
            for (field, n) in fields.normal.iter().zip(mirror.normal) {
                field.update(cx, |field, cx| field.set_value(n, cx));
            }
            fields
                .offset
                .update(cx, |field, cx| field.set_value(mirror.offset, cx));
        }
        _ => return false,
    }
    true
}

impl Luma {
    /// Rewrite one form input from its stored value. `edit` returns `None`
    /// to leave it as it is.
    fn form_edit(
        &mut self,
        def: &PatternArgDef,
        spec: &'static p::Input,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&p::Value) -> Option<p::Value>,
    ) {
        let mut wire = None;
        self.with_track_editor(cx, |editor| {
            if let Ok(value) = decode(spec.value_type, &stored_arg(editor, def)) {
                wire = edit(&value).map(|value| document::wire_value(&value));
            }
        });
        if let Some(wire) = wire {
            self.arg_live(&def.id, wire, cx);
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// The rows of a form clip: the selection, then each input in the form's
/// order. The rare ones sit under "Advanced". An aim shows its base, then
/// its motion, each without the rows that do not apply.
pub(super) fn rows(state: &Editor, built: &Built, app: &Entity<Luma>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (index, cell) in built.cells.iter().enumerate() {
        let Some(slot) = &cell.form else {
            rows.extend(arg_rows(state, app, index, cell));
            continue;
        };
        if slot.form == "aim@1" && aim_hides(built, slot.key) {
            continue;
        }
        match cell.def.id.as_str() {
            // Width's own row carries it.
            "width_relative" => continue,
            // An aim's motion group.
            "motion" if slot.form == "aim@1" => {
                rows.push(luma_ui::float::divider().into_any_element())
            }
            "boundary" => rows.push(
                div()
                    .pt(px(8.))
                    .child(luma_ui::caption("Advanced".to_string()))
                    .agent_node(Role::Text, "Advanced")
                    .into_any_element(),
            ),
            _ => {}
        }
        match control(state, app, index, cell, slot) {
            Some(control) => rows.push(row(state, built, app, index, cell, slot, control)),
            None => rows.extend(arg_rows(state, app, index, cell)),
        }
    }
    rows
}

/// Whether an `aim@1` row does not apply to the clip's blend, base and
/// motion: an Offset clip has no base, so base, direction and point go; the
/// direction or the point by the base; shape, size, spread and speed by the
/// motion. `every` paces a shape and a fan per hit.
fn aim_hides(built: &Built, key: &str) -> bool {
    let offset = built.blend == BlendMode::Offset;
    let stored = |key: &str| {
        built
            .cells
            .iter()
            .find(|cell| cell.def.id == key)
            .map(|cell| &cell.synced)
    };
    let choice = |key: &str| {
        stored(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let (base, motion) = (choice("base"), choice("motion"));
    let fan_per_hit = stored("fan")
        .and_then(|fan| fan.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("hit");
    match key {
        "base" => offset,
        "direction" => offset || base != "direction",
        "point" => offset || base != "point",
        "shape" | "spread" => motion != "shape",
        "size" => motion == "none",
        "speed" => motion != "noise",
        "every" => motion != "shape" && !fan_per_hit,
        _ => false,
    }
}

/// A labelled form row. The header line carries the promote menu, and for a
/// chase's width, what the width is a share of.
fn row(
    state: &Editor,
    built: &Built,
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    slot: &Slot,
    control: Div,
) -> AnyElement {
    let name = slot.spec.name.as_str();
    let share = (slot.form == "color.chase@1" && cell.def.id == "width")
        .then(|| {
            built
                .cells
                .iter()
                .find(|cell| cell.def.id == "width_relative")
        })
        .flatten()
        .map(|relative| width_share(app, relative));
    let promote = (!slot.spec.promotable.is_empty()).then(|| {
        div()
            .w(px(MODE_W))
            .flex()
            .flex_col()
            .child(promote_select(state, app, index, cell, slot))
    });
    let accessories = share
        .into_iter()
        .chain(promote)
        .map(IntoElement::into_any_element)
        .collect();
    sheet_row(name, accessories, control)
}

/// The menu that switches an input between a fixed value and a source.
fn promote_select(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    slot: &Slot,
) -> Div {
    let kinds = slot.spec.promotable.clone();
    let labels: Vec<&str> = std::iter::once(FIXED)
        .chain(kinds.iter().map(|kind| source_label(*kind)))
        .collect();
    let current = slot.mode.map_or(FIXED, source_label);
    let toggle = app.clone();
    let pick = app.clone();
    let def = cell.def.clone();
    let spec = slot.spec;
    let mode = slot.mode;
    let shape = *slot;
    luma_arg_select(
        format!("{}:source", slot.spec.name),
        current,
        &labels,
        menu_visibility(state, Menu::Source(index)),
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = if editor.sheet.open == Some(Menu::Source(index)) {
                        None
                    } else {
                        Some(Menu::Source(index))
                    };
                });
            });
        },
        move |picked, _, cx| {
            let to = picked.checked_sub(1).map(|at| kinds[at]);
            pick.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.open = None);
                if to == mode {
                    return;
                }
                this.form_edit(&def, spec, cx, |value| Some(promote(&shape, value, to)));
            });
        },
    )
}

/// A chase width's reading: a share of the gap between strokes, or of the
/// whole axis.
fn width_share(app: &Entity<Luma>, relative: &Cell) -> Div {
    let on = relative.synced.as_bool().unwrap_or(true);
    let id = relative.def.id.clone();
    luma_ui::float::segmented().children([("Of gap", true), ("Of axis", false)].map(
        |(label, value)| {
            let app = app.clone();
            let id = id.clone();
            luma_ui::float::segment(label, on == value, format!("width-share-{label}"))
                .id(SharedString::from(format!("width-share-{label}")))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.arg_live(&id, serde_json::json!(value), cx)
                    });
                })
                .agent_node(Role::Button, format!("Width {label}"))
        },
    ))
}

/// Opens or closes this row's `Menu::Choice`.
fn choice_toggle(app: &Entity<Luma>, index: usize) -> impl Fn(&mut Window, &mut App) + Clone {
    menu_toggle(app, Menu::Choice(index))
}

/// Opens `menu`, or closes it when it is open.
fn menu_toggle(app: &Entity<Luma>, menu: Menu) -> impl Fn(&mut Window, &mut App) + Clone {
    let app = app.clone();
    move |_, cx| {
        app.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| {
                editor.sheet.open = if editor.sheet.open == Some(menu) {
                    None
                } else {
                    Some(menu)
                };
            });
        });
    }
}

/// A select that opens under `menu`.
fn menu_select(
    state: &Editor,
    app: &Entity<Luma>,
    menu: Menu,
    id: String,
    current: &str,
    labels: &[&str],
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    luma_arg_select(
        id,
        current,
        labels,
        menu_visibility(state, menu),
        menu_toggle(app, menu),
        choice_pick(app, on_pick),
    )
}

/// The axis the custom plane turns around, or the default one to start from.
fn plane_normal(mapping: &p::MappingSpec) -> [f64; 3] {
    match &mapping.plane {
        Some(p::AxisPlane::Custom { normal }) => *normal,
        _ => [0., -1., 0.],
    }
}

fn set_normal(value: &mut p::Value, axis: usize, n: f64) {
    if let p::Value::Mapping(mapping) = value {
        if let Some(p::AxisPlane::Custom { normal }) = &mut mapping.plane {
            normal[axis] = n;
        }
    }
}

/// The mirror choices: a plane through the middle of the span, left–right
/// (normal U), front–back (V) or up–down (Z), or a custom plane.
const MIRRORS: [&str; 5] = ["Off", "Left–right", "Front–back", "Up–down", "Custom plane"];
/// The index of Custom plane in [`MIRRORS`].
const CUSTOM_MIRROR: usize = 4;

/// Which of [`MIRRORS`] `mirror` is: a normal that is none of the fixed
/// planes is a custom plane.
fn mirror_index(mirror: Option<&p::MirrorPlane>) -> usize {
    match mirror.map(|plane| plane.normal) {
        None => 0,
        Some([1., 0., 0.]) => 1,
        Some([0., 1., 0.]) => 2,
        Some([0., 0., 1.]) => 3,
        Some(_) => CUSTOM_MIRROR,
    }
}

/// The mirror plane, or the left–right plane to start from.
fn mirror_plane(mapping: &p::MappingSpec) -> p::MirrorPlane {
    mapping.mirror.clone().unwrap_or(p::MirrorPlane {
        normal: [1., 0., 0.],
        offset: 0.,
    })
}

fn set_mirror(value: &mut p::Value, edit: impl FnOnce(&mut p::MirrorPlane)) {
    if let p::Value::Mapping(mapping) = value {
        if let Some(plane) = &mut mapping.mirror {
            edit(plane);
        }
    }
}

/// The axis row: which way, what one axis spans, the mirror where the axis
/// takes one and, for radial and angle, the plane.
#[allow(clippy::too_many_arguments)]
fn axis_control(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    name: &str,
    def: PatternArgDef,
    spec: &'static p::Input,
    mapping: p::MappingSpec,
    fields: &AxisFields,
) -> Div {
    let Some(p::Author::Choice { options, .. }) = &spec.author else {
        return div();
    };
    let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
    let current = options.iter().position(
        |option| matches!(&option.value, p::Value::Mapping(preset) if preset.source == mapping.source),
    );
    let edit =
        move |this: &mut Luma, cx: &mut Context<Luma>, change: &dyn Fn(&mut p::MappingSpec)| {
            this.form_edit(&def, spec, cx, |value| match value {
                p::Value::Mapping(mapping) => {
                    let mut mapping = mapping.clone();
                    change(&mut mapping);
                    Some(p::Value::Mapping(mapping))
                }
                _ => None,
            });
        };
    let edit = Rc::new(edit);
    let pick_axis = edit.clone();
    let pick_span = edit.clone();
    let pick_mirror = edit.clone();
    let pick_plane = edit;
    let mirrors = mapping.source.takes_mirror();
    let mirror = match mapping.mirror.as_ref() {
        Some(_) if fields.custom_mirror.get() => CUSTOM_MIRROR,
        plane => mirror_index(plane),
    };
    let custom_mirror = fields.custom_mirror.clone();
    let round = mapping.plane.is_some();
    let plane = mapping.plane.as_ref().map_or(0, p::AxisPlane::index);
    let spans: Vec<&str> = p::Span::OPTIONS.iter().map(|(_, label)| *label).collect();
    let span = p::Span::OPTIONS
        .iter()
        .position(|(span, _)| *span == mapping.span)
        .unwrap_or(0);
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(menu_select(
            state,
            app,
            Menu::Choice(index),
            name.to_string(),
            current.map_or("Custom", |at| labels[at]),
            &labels,
            move |picked, this, cx| {
                let p::Value::Mapping(preset) = &options[picked].value else {
                    return;
                };
                // A new direction keeps the spans and, where it takes one,
                // the mirror; radial and angle keep their plane, Auto when
                // they had none.
                pick_axis(this, cx, &|mapping| {
                    mapping.source = preset.source.clone();
                    if !mapping.source.takes_mirror() {
                        mapping.mirror = None;
                    }
                    mapping.plane = match (&preset.plane, &mapping.plane) {
                        (None, _) => None,
                        (Some(_), Some(kept)) => Some(kept.clone()),
                        (Some(plane), None) => Some(plane.clone()),
                    };
                });
            },
        ))
        .child(arg_row(
            "Spans",
            menu_select(
                state,
                app,
                Menu::Span(index),
                format!("{name}: Spans"),
                spans[span],
                &spans,
                move |picked, this, cx| {
                    let span = p::Span::OPTIONS[picked].0;
                    pick_span(this, cx, &|mapping| mapping.span = span);
                },
            ),
        ))
        .when(mirrors, |el| {
            el.child(arg_row(
                "Mirror",
                menu_select(
                    state,
                    app,
                    Menu::Mirror(index),
                    format!("{name}: Mirror"),
                    MIRRORS[mirror],
                    &MIRRORS,
                    move |picked, this, cx| {
                        custom_mirror.set(picked == CUSTOM_MIRROR);
                        // A new plane keeps the offset; Custom plane starts
                        // from the plane there is.
                        pick_mirror(this, cx, &|mapping| {
                            let kept = mirror_plane(mapping);
                            mapping.mirror = match picked {
                                0 => None,
                                1 => Some([1., 0., 0.]),
                                2 => Some([0., 1., 0.]),
                                3 => Some([0., 0., 1.]),
                                _ => Some(kept.normal),
                            }
                            .map(|normal| p::MirrorPlane {
                                normal,
                                offset: kept.offset,
                            });
                        });
                    },
                ),
            ))
            .when(mirror == CUSTOM_MIRROR, |el| {
                el.child(arg_row(
                    "Normal · U, V, Z",
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .children(fields.normal.iter().cloned()),
                ))
            })
            .when(mirror != 0, |el| {
                el.child(arg_row("Offset", fields.offset.clone()))
            })
        })
        .when(round, |el| {
            el.child(arg_row(
                "Plane",
                menu_select(
                    state,
                    app,
                    Menu::Plane(index),
                    format!("{name}: Plane"),
                    p::AxisPlane::OPTIONS[plane],
                    &p::AxisPlane::OPTIONS,
                    move |picked, this, cx| {
                        pick_plane(this, cx, &|mapping| {
                            let normal = plane_normal(mapping);
                            mapping.plane = Some(match picked {
                                1 => p::AxisPlane::UpDown,
                                2 => p::AxisPlane::FrontBack,
                                3 => p::AxisPlane::LeftRight,
                                4 => p::AxisPlane::Custom { normal },
                                _ => p::AxisPlane::Auto,
                            });
                        });
                    },
                ),
            ))
        })
        .when(plane == 4, |el| {
            el.child(arg_row(
                "Axis · U, V, Z",
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .children(fields.plane.iter().cloned()),
            ))
        })
}

/// Closes the open menu, then runs `on_pick` with the picked index.
fn choice_pick(
    app: &Entity<Luma>,
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> impl Fn(usize, &mut Window, &mut App) + Clone {
    let app = app.clone();
    let on_pick = Rc::new(on_pick);
    move |picked, _, cx| {
        let on_pick = on_pick.clone();
        app.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| editor.sheet.open = None);
            on_pick(picked, this, cx);
        });
    }
}

/// A select that opens under this row's `Menu::Choice`.
fn choice_select(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    id: String,
    current: &str,
    labels: &[&str],
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    luma_arg_select(
        id,
        current,
        labels,
        menu_visibility(state, Menu::Choice(index)),
        choice_toggle(app, index),
        choice_pick(app, on_pick),
    )
}

/// A preset picker that opens under this row's `Menu::Choice`. Its Custom
/// tile keeps the value and shows the row's editor.
#[allow(clippy::too_many_arguments)]
fn choice_presets(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    id: String,
    value: &p::Envelope,
    current: Option<usize>,
    options: &[(SharedString, Thumb)],
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    let app_pick = app.clone();
    let on_pick = Rc::new(on_pick);
    luma_preset_picker(
        id,
        &Thumb::Curve(value.clone()),
        current,
        options,
        true,
        menu_visibility(state, Menu::Choice(index)),
        choice_toggle(app, index),
        move |picked, _, cx| {
            let on_pick = on_pick.clone();
            app_pick.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = None;
                    let slot = editor
                        .sheet
                        .built
                        .as_mut()
                        .and_then(|built| built.cells.get_mut(index))
                        .and_then(|cell| cell.form.as_mut());
                    if let Some(slot) = slot {
                        slot.editing = picked.is_none();
                    }
                });
                if let Some(at) = picked {
                    on_pick(at, this, cx);
                }
            });
        },
    )
}

/// The shipped curve presets as the input's curves, each in the envelope
/// editor's 0–1 box.
fn curve_options(slot: &Slot) -> Vec<(SharedString, Thumb)> {
    slot.curves()
        .map(|preset| {
            let curve = scaled(slot, &preset.curve);
            (
                preset.name.clone().into(),
                Thumb::Curve(envelope_of(slot, &curve)),
            )
        })
        .collect()
}

/// The preset `curve` equals, if any.
fn curve_preset(slot: &Slot, curve: &p::Keyframes) -> Option<usize> {
    slot.curves()
        .position(|preset| same_curve(&scaled(slot, &preset.curve), curve))
}

/// A choice's options as named curves, when every option is an envelope.
fn envelope_options(options: &[p::Preset]) -> Option<Vec<(SharedString, Thumb)>> {
    options
        .iter()
        .map(|option| match &option.value {
            p::Value::Envelope(envelope) => {
                Some((option.label.clone().into(), Thumb::Curve(envelope.clone())))
            }
            _ => None,
        })
        .collect()
}

/// The control under a form row's header, or `None` for a control the plain
/// sheet draws.
fn control(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    slot: &Slot,
) -> Option<Div> {
    let name = slot.spec.name.as_str();
    let def = cell.def.clone();
    let spec = slot.spec;
    let value = decode(spec.value_type, &cell.synced).ok();
    let column = || div().w_full().flex().flex_col().gap(px(6.));
    Some(match &cell.widget {
        Widget::Direction(turns) => {
            let ends = vector_ends(value.as_ref()?);
            let timed = ends.len() > 1;
            column().children(ends.into_iter().enumerate().map(|(end, v)| {
                let (turn, tilt) = turn_tilt(v);
                let turn = turn.unwrap_or_else(|| turns.get(end).copied().unwrap_or(0.));
                let [turn_label, tilt_label] = match (timed, end) {
                    (false, _) => ["Turn", "Tilt"],
                    (true, 0) => ["Start turn", "Start tilt"],
                    (true, _) => ["End turn", "End tilt"],
                };
                let turned = app.clone();
                let turn_def = def.clone();
                let turn_scrub = direction_scrub(
                    &format!("{name}: {turn_label}"),
                    turn,
                    [-180., 180.],
                    move |turn, cx| {
                        turned.update(cx, |this, cx| {
                            edit_widget(this, index, cx, |widget| {
                                if let Widget::Direction(turns) = widget {
                                    if let Some(held) = turns.get_mut(end) {
                                        *held = turn;
                                    }
                                }
                            });
                            this.form_edit(&turn_def, spec, cx, |value| {
                                edit_vector_end(value, end, |v| direction_at(turn, turn_tilt(v).1))
                            });
                        });
                    },
                );
                let tilted = app.clone();
                let tilt_def = def.clone();
                let tilt_scrub = direction_scrub(
                    &format!("{name}: {tilt_label}"),
                    tilt,
                    [-90., 90.],
                    move |tilt, cx| {
                        tilted.update(cx, |this, cx| {
                            this.form_edit(&tilt_def, spec, cx, |value| {
                                edit_vector_end(value, end, |_| direction_at(turn, tilt))
                            });
                        });
                    },
                );
                column()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(arg_row(turn_label, turn_scrub))
                            .child(arg_row(tilt_label, tilt_scrub)),
                    )
                    .child(luma_ui::caption(vector_text(v)))
            }))
        }
        Widget::Point(sets) => {
            let timed = sets.len() > 1;
            column().children(sets.iter().enumerate().map(|(end, fields)| {
                let label = match (timed, end) {
                    (false, _) => "U, V, Z",
                    (true, 0) => "Start · U, V, Z",
                    (true, _) => "End · U, V, Z",
                };
                arg_row(
                    label,
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .children(fields.iter().cloned()),
                )
            }))
        }
        Widget::Axis(fields) => {
            let Some(p::Value::Mapping(mapping)) = value.or_else(|| spec.default.clone()) else {
                return None;
            };
            axis_control(state, app, index, name, def, spec, mapping, fields)
        }
        Widget::Preset(options, editor) => {
            let options: &'static [p::Preset] = options;
            let current = options.iter().position(|option| {
                document::close(&document::wire_value(&option.value), &cell.synced)
            });
            let pick = move |picked: usize, this: &mut Luma, cx: &mut Context<Luma>| {
                this.arg_live(&def.id, document::wire_value(&options[picked].value), cx);
            };
            match (envelope_options(options), editor) {
                (Some(curves), Some(editor)) => {
                    let shown = match &value {
                        Some(p::Value::Envelope(envelope)) => envelope.clone(),
                        _ => match &curves[current.unwrap_or(0)].1 {
                            Thumb::Curve(envelope) => envelope.clone(),
                            Thumb::Gradient(_) => unreachable!("curve options"),
                        },
                    };
                    column()
                        .child(choice_presets(
                            state,
                            app,
                            index,
                            name.to_string(),
                            &shown,
                            current,
                            &curves,
                            pick,
                        ))
                        .when(slot.editing || current.is_none(), |el| {
                            el.child(editor.clone())
                        })
                }
                _ => {
                    let labels: Vec<&str> =
                        options.iter().map(|option| option.label.as_str()).collect();
                    choice_select(
                        state,
                        app,
                        index,
                        name.to_string(),
                        current.map_or("Custom", |at| labels[at]),
                        &labels,
                        pick,
                    )
                }
            }
        }
        Widget::Grain(entity) => {
            let n = value.as_ref().and_then(level).unwrap_or(1.);
            let kind = match n {
                n if n == 1. => 0,
                n if n == 0. => 1,
                _ => 2,
            };
            let clump = entity.clone();
            column()
                .child(choice_select(
                    state,
                    app,
                    index,
                    name.to_string(),
                    GRAINS[kind],
                    &GRAINS,
                    move |picked, this, cx| {
                        let n = match picked {
                            0 => 1.,
                            1 => 0.,
                            _ => clump.read(cx).value().max(2.),
                        };
                        this.arg_live(&def.id, serde_json::json!(n), cx);
                    },
                ))
                .when(kind == 2, |el| el.child(entity.clone()))
        }
        Widget::Every(entity) => {
            let beats = value.as_ref().and_then(level).unwrap_or(0.);
            let once = beats <= 0.;
            let segments = luma_ui::float::segmented().children(EVERY.map(|label| {
                let app = app.clone();
                let id = def.id.clone();
                let chosen = (label == EVERY[0]) == once;
                let key = format!("{}-{label}", def.id);
                luma_ui::float::segment(label, chosen, key.clone())
                    .id(SharedString::from(key))
                    .on_click(move |_, _, cx| {
                        let beats = if label == EVERY[0] { 0. } else { EVERY_BEATS };
                        if chosen {
                            return;
                        }
                        app.update(cx, |this, cx| {
                            this.arg_live(&id, serde_json::json!(beats), cx)
                        });
                    })
                    .agent_node(Role::Button, format!("{name} {label}"))
            }));
            column()
                .child(segments)
                .when(!once, |el| el.child(entity.clone()))
        }
        Widget::Envelope(entity) if slot.mode.is_some() => {
            let curve = match &value {
                Some(p::Value::Time(curve) | p::Value::Hit(curve)) => Some(curve),
                _ => None,
            };
            let shown = curve.map_or_else(
                || p::Envelope::linear(vec![[0., 0.], [1., 0.]]),
                |curve| envelope_of(slot, curve),
            );
            let current = curve.and_then(|curve| curve_preset(slot, curve));
            let shape = *slot;
            let span = slot.span_text(slot.range());
            let custom = slot.editing || current.is_none();
            column()
                .child(choice_presets(
                    state,
                    app,
                    index,
                    format!("{name}:curve"),
                    &shown,
                    current,
                    &curve_options(slot),
                    move |picked, this, cx| {
                        let Some(preset) = shape.curves().nth(picked) else {
                            return;
                        };
                        let curve = scaled(&shape, &preset.curve);
                        this.form_edit(&def, spec, cx, |value| match value {
                            p::Value::Time(_) => Some(p::Value::Time(curve)),
                            p::Value::Hit(_) => Some(p::Value::Hit(curve)),
                            _ => None,
                        });
                    },
                ))
                .when(custom, |el| {
                    el.child(entity.clone())
                        .child(luma_ui::caption(format!("Values {span}")))
                })
        }
        Widget::Gradient(entity) if slot.mode.is_some() => column().child(entity.clone()).child(
            luma_ui::caption("Colors from the start to the end".to_string()),
        ),
        Widget::Noise([speed, low, high]) => column()
            .child(arg_row("Speed (beats)", speed.clone()))
            .child(arg_row("Range", range_row(low, high)))
            .when(slot.vector(), |el| {
                el.child(luma_ui::caption(
                    "U, V and Z each wander in this range".to_string(),
                ))
            }),
        Widget::Audio([from, to, floor, threshold]) => {
            // Named ranges only fill the two frequency fields.
            let named = &p::presets().frequencies;
            let mut labels: Vec<&str> = named.iter().map(|f| f.name.as_str()).collect();
            labels.push("Custom");
            let current = match &value {
                Some(p::Value::Audio(audio)) => named
                    .iter()
                    .find(|f| f.from_hz == audio.from_hz && f.to_hz == audio.to_hz)
                    .map_or("Custom", |f| f.name.as_str()),
                _ => "Custom",
            };
            column()
                .child(arg_row(
                    "Frequency",
                    choice_select(
                        state,
                        app,
                        index,
                        format!("{name}:frequency"),
                        current,
                        &labels,
                        move |picked, this, cx| {
                            let Some(range) = p::presets().frequencies.get(picked) else {
                                return;
                            };
                            this.form_edit(&def, spec, cx, |value| match value {
                                p::Value::Audio(audio) => Some(p::Value::Audio(p::AudioLevel {
                                    from_hz: range.from_hz,
                                    to_hz: range.to_hz,
                                    ..audio.clone()
                                })),
                                _ => None,
                            });
                        },
                    ),
                ))
                .child(arg_row("Range", range_row(from, to)))
                .child(arg_row("Floor", floor.clone()))
                // Energy below the threshold gives 0; 0% is no gate.
                .child(arg_row("Threshold", threshold.clone()))
                // The engine reads the level as a share of the input's top.
                .when(slot.unit() == Some("°"), |el| {
                    el.child(luma_ui::caption(format!(
                        "Level 0–100% gives {}",
                        slot.span_text([0., slot.range()[1]])
                    )))
                })
        }
        Widget::Color(entity) => div().child(entity.clone()),
        Widget::Scalar(entity) => div().child(entity.clone()),
        Widget::Gradient(entity) => div().child(entity.clone()),
        _ => return None,
    })
}

/// A turn or a tilt: a scrub in whole degrees, half the column wide, so the
/// two sit side by side.
fn direction_scrub(
    id: &str,
    value: f64,
    [min, max]: [f64; 2],
    on_change: impl Fn(f64, &mut App) + 'static,
) -> Stateful<Div> {
    luma_ui::float::scrub_with_unit(
        id.to_string(),
        value,
        min,
        max,
        1.,
        (FIELD_W - 8.) / 2.,
        "°",
        move |value, _, cx| on_change(value, cx),
    )
}

fn range_row(low: &Entity<DraftedNumber>, high: &Entity<DraftedNumber>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .child(low.clone())
        .child(high.clone())
}

#[cfg(test)]
mod tests {
    use super::{
        curve_options, curve_preset, direction_at, edit_vector_end, envelope_of, envelope_options,
        keyframes_of, promote, same_curve, scaled, turn_tilt, vector_ends, vector_text, Slot,
        Thumb, CURVE_BEATS, MIN_BEATS,
    };
    use luma_lib::models::node_graph::{PatternArgDef, PatternArgType};
    use luma_patterns as p;

    fn slot(form: &str, key: &str) -> Slot {
        let def = PatternArgDef {
            id: key.into(),
            name: key.into(),
            arg_type: PatternArgType::Scalar,
            default_value: serde_json::Value::Null,
        };
        Slot::new(form, &def, &serde_json::json!(1.0)).expect("a form input")
    }

    #[test]
    fn promoting_starts_flat_at_the_fixed_value_and_returns_to_it() {
        let every = slot("color.chase@1", "every");
        let time = promote(&every, &p::Value::Beats(2.), Some(p::SourceKind::Time));
        assert_eq!(
            time,
            p::Value::Time(p::Keyframes::numbers(&[[0., 2.], [1., 2.]], &[]))
        );
        let p::Value::Time(mut curve) = time else {
            unreachable!()
        };
        curve.points[0].value = p::Key::Number(3.);
        assert_eq!(
            promote(&every, &p::Value::Time(curve), None),
            p::Value::Beats(3.)
        );

        let color = slot("color.chase@1", "color");
        let red = p::Value::Color([1., 0., 0.]);
        let curve = promote(&color, &red, Some(p::SourceKind::Time));
        assert!(matches!(&curve, p::Value::Time(curve) if curve.is_color()));
        assert_eq!(promote(&color, &curve, None), red);

        let alpha = slot("color.chase@1", "alpha");
        assert_eq!(
            promote(
                &alpha,
                &p::Value::Proportion(0.8),
                Some(p::SourceKind::Audio)
            ),
            p::Value::Audio(p::AudioLevel {
                from_hz: 40.,
                to_hz: 100.,
                floor: 0.,
                threshold: 0.,
            })
        );
    }

    #[test]
    fn a_curve_round_trips_through_the_envelope_box() {
        let every = slot("color.chase@1", "every");
        let curve = p::Keyframes::numbers(
            &[[0., 2.], [0.5, 4.], [1., 0.5]],
            &[p::Ease::Hold, p::Ease::EaseInOut],
        );
        let envelope = envelope_of(&every, &curve);
        assert!(envelope.validate().is_ok());
        assert_eq!(
            envelope,
            p::Envelope::eased(
                vec![[0., 0.25], [0.5, 0.5], [1., 0.0625]],
                &[p::Ease::Hold, p::Ease::EaseInOut]
            )
        );
        assert!(same_curve(&keyframes_of(&every, &envelope), &curve));
    }

    #[test]
    fn drawn_handles_are_stored_and_read_back_as_drawn() {
        // Handles far from any standard ease, on alpha (0–1) and on a speed.
        let drawn = p::Envelope::eased(
            vec![[0., 0.1], [0.4, 0.9], [1., 0.3]],
            &[
                p::Ease::Bezier([0.125, 0.875, 0.75, 0.125]),
                p::Ease::Bezier([0.5, 0., 0.75, 1.]),
            ],
        );
        let alpha = slot("color.chase@1", "alpha");
        let stored = keyframes_of(&alpha, &drawn);
        assert_eq!(stored.ease(0), drawn.ease(0));
        // Stored, read back, and shown again: the same handles.
        let wire = super::document::wire_value(&p::Value::Time(stored.clone()));
        let Ok(p::Value::Time(read)) = super::decode(alpha.spec.value_type, &wire) else {
            panic!("reads back")
        };
        assert_eq!(read, stored);
        assert_eq!(envelope_of(&alpha, &read), drawn);
        // What plays is what the editor draws.
        for i in 0..=40 {
            let x = f64::from(i) / 40.;
            assert!((read.sample(x)[0] - drawn.sample(x)).abs() < 1e-12, "{x}");
        }
        // On a speed the values scale and the eases stay as drawn.
        let travel = slot("color.chase@1", "travel");
        let back = envelope_of(&travel, &keyframes_of(&travel, &drawn));
        for (a, b) in back.points.iter().zip(&drawn.points) {
            assert_eq!(a.ease, b.ease);
        }
    }

    #[test]
    fn curve_presets_scale_to_the_input() {
        let travel = slot("color.chase@1", "travel");
        let up = p::presets().curve("Ramp up").unwrap();
        let curve = scaled(&travel, up);
        assert_eq!(
            curve.points.first().unwrap().value,
            p::Key::Number(MIN_BEATS)
        );
        assert_eq!(
            curve.points.last().unwrap().value,
            p::Key::Number(CURVE_BEATS)
        );
    }

    #[test]
    fn the_curve_picker_finds_the_preset_a_curve_came_from() {
        let travel = slot("color.chase@1", "travel");
        let options = curve_options(&travel);
        assert_eq!(options.len(), travel.curves().count());
        assert!(options
            .iter()
            .all(|(_, thumb)| matches!(thumb, Thumb::Curve(e) if e.validate().is_ok())));
        for (at, preset) in travel.curves().enumerate() {
            assert_eq!(
                curve_preset(&travel, &scaled(&travel, &preset.curve)),
                Some(at)
            );
        }
        let custom = p::Keyframes::numbers(&[[0., 3.], [1., 5.]], &[]);
        assert_eq!(curve_preset(&travel, &custom), None);
    }

    #[test]
    fn alpha_curves_match_after_a_round_trip() {
        let alpha = slot("color.chase@1", "alpha");
        let names: Vec<_> = alpha.curves().map(|c| c.name.as_str()).collect();
        assert_eq!(names[0], "Full");
        assert!(!names.contains(&"Ramp up"), "{names:?}");
        // A fixed alpha promoted to a curve is the flat "Full" curve.
        let p::Value::Time(flat) =
            promote(&alpha, &p::Value::Proportion(1.), Some(p::SourceKind::Time))
        else {
            unreachable!()
        };
        assert_eq!(curve_preset(&alpha, &flat), Some(0));
        for (at, preset) in alpha.curves().enumerate() {
            // Stored, read back, and passed through the envelope editor.
            let value = p::Value::Time(scaled(&alpha, &preset.curve));
            let wire = super::document::wire_value(&value);
            let Ok(p::Value::Time(read)) = super::decode(alpha.spec.value_type, &wire) else {
                panic!("{} reads back", preset.name)
            };
            assert_eq!(curve_preset(&alpha, &read), Some(at), "{}", preset.name);
            let edited = keyframes_of(&alpha, &envelope_of(&alpha, &read));
            assert_eq!(curve_preset(&alpha, &edited), Some(at), "{}", preset.name);
        }
    }

    #[test]
    fn chase_shape_and_path_pick_from_curves() {
        for key in ["shape", "path"] {
            let spec = slot("color.chase@1", key).spec;
            let Some(p::Author::Choice { options, .. }) = &spec.author else {
                panic!("{key} is a choice");
            };
            let curves = envelope_options(options).expect("every option is a curve");
            assert_eq!(curves.len(), options.len());
        }
        let path = slot("color.chase@1", "path").spec;
        let Some(p::Author::Choice { options, .. }) = &path.author else {
            unreachable!()
        };
        let names: Vec<_> = envelope_options(options)
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(names.iter().any(|name| name == "Steps (4)"), "{names:?}");
        let axis = slot("color.chase@1", "axis").spec;
        if let Some(p::Author::Choice { options, .. }) = &axis.author {
            assert!(envelope_options(options).is_none(), "axis stays a select");
        }
    }

    #[test]
    fn a_direction_reads_as_turn_and_tilt() {
        // The resting aim: 40° down toward downstage.
        let rest = [0., 0.766, -0.643];
        let (turn, tilt) = turn_tilt(rest);
        assert!(
            turn.unwrap().abs() < 1e-9 && (tilt + 40.).abs() < 0.02,
            "{tilt}"
        );
        assert_eq!(direction_at(0., -40.), [0., 0.766, -0.6428]);
        // Turn grows toward stage right.
        assert_eq!(direction_at(90., 0.), [1., 0., 0.]);
        // Straight down says no turn.
        assert_eq!(turn_tilt([0., 0., -1.]), (None, -90.));
        assert_eq!(vector_text(rest), "U 0.00 · V 0.77 · Z −0.64");
    }

    #[test]
    fn a_vector_promotes_to_a_curve_whose_ends_edit_apart() {
        let direction = slot("aim@1", "direction");
        let rest = p::Value::Vector([0., 0.766, -0.643]);
        let curve = promote(&direction, &rest, Some(p::SourceKind::Time));
        assert_eq!(vector_ends(&curve), vec![[0., 0.766, -0.643]; 2]);
        let moved = edit_vector_end(&curve, 1, |_| [0., 0., -1.]).unwrap();
        assert_eq!(
            vector_ends(&moved),
            vec![[0., 0.766, -0.643], [0., 0., -1.]]
        );
        assert_eq!(promote(&direction, &moved, None), rest);
        let p::Value::Noise(noise) = promote(&direction, &rest, Some(p::SourceKind::Noise)) else {
            panic!("noise")
        };
        assert_eq!(noise.range, [-1., 1.]);
        // Audio reads 0–1 as 0 to the most degrees; fixed again, it is the top.
        let fan = slot("aim@1", "fan");
        let audio = promote(&fan, &p::Value::Number(30.), Some(p::SourceKind::Audio));
        assert_eq!(promote(&fan, &audio, None), p::Value::Number(90.));
    }
}
