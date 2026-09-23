//! Form clip inputs. A form clip names a shipped form and holds a value for
//! each of its inputs. The sheet shows them in the form's order, under the
//! engine's names. Where the form allows it, an input can hold a source in
//! place of a plain value: a curve over the clip or over each hit, noise, an
//! audio band, or a list of stamped beats.

use super::*;
use luma_lib::node_graph::lighting::decode;
use luma_patterns as p;
use luma_ui::arg::envelope::{EnvelopeChanged, EnvelopeEditor};
use luma_ui::arg::number::format_value;
use luma_ui::text_input::{self, TextInput};

/// Inputs in beats that must stay above zero.
const SPEEDS: [&str; 4] = ["every", "travel", "duration", "speed"];
/// The least beats a speed input takes.
const MIN_BEATS: f64 = 1. / 16.;
/// The top of a speed curve's value axis, in beats.
const CURVE_BEATS: f64 = 8.;
/// Beats a new noise source takes to wander once.
const NOISE_SPEED: f64 = 4.;
/// How many beats a new stamped list holds.
const STAMPS: usize = 4;
/// The grain choices. A clump holds N heads; N is its own field.
const GRAINS: [&str; 3] = ["Head", "Fixture", "Clump"];
/// The two readings of `color.time`'s `every`.
const EVERY: [&str; 2] = ["Once", "Beats"];
/// The period a switch from "Once" to "Beats" starts at.
const EVERY_BEATS: f64 = 4.;

/// A form input's row: what the form says about the input, and which source
/// the stored value holds.
#[derive(Clone, Copy)]
pub(super) struct Slot {
    form: &'static str,
    spec: &'static p::Input,
    speed: bool,
    mode: Option<p::SourceKind>,
}

impl Slot {
    /// The slot for `def` when `form` is a shipped form that has it.
    pub(super) fn new(form: &str, def: &PatternArgDef, stored: &serde_json::Value) -> Option<Self> {
        let form = *p::FORMS.iter().find(|id| **id == form)?;
        let spec = document::form_definition(form)?.inputs.get(&def.id)?;
        Some(Self {
            form,
            spec,
            speed: SPEEDS.contains(&def.id.as_str()),
            mode: decode(spec.value_type, stored)
                .ok()
                .and_then(|value| value.source_kind()),
        })
    }

    /// The span a number curve's value axis covers, in the input's unit.
    fn range(&self) -> [f64; 2] {
        if self.speed {
            [0., CURVE_BEATS]
        } else {
            [0., 1.]
        }
    }

    /// A number the input accepts: speeds stay above zero, the rest in 0–1.
    fn fit(&self, value: f64) -> f64 {
        if self.speed {
            value.max(MIN_BEATS)
        } else {
            value.clamp(0., 1.)
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
        p::SourceKind::Events => "↗ Stamped beats",
    }
}

const PLAIN: &str = "Plain";

fn level(value: &p::Value) -> Option<f64> {
    match value {
        p::Value::Beats(v) | p::Value::Proportion(v) | p::Value::Number(v) => Some(*v),
        _ => None,
    }
}

/// `value` converted to `to`. A plain value becomes a source that starts at
/// it; a source becomes the plain value it starts at.
fn promote(slot: &Slot, value: &p::Value, to: Option<p::SourceKind>) -> p::Value {
    let color = match value {
        p::Value::Color(rgb) => Some(*rgb),
        p::Value::Time(curve) | p::Value::Hit(curve) if curve.is_color() => Some(curve.sample(0.)),
        _ => None,
    };
    let number = match value {
        p::Value::Time(curve) | p::Value::Hit(curve) => Some(curve.sample(0.)[0]),
        p::Value::Noise(p::NoiseSource { range, .. })
        | p::Value::Audio(p::AudioLevel { range, .. }) => Some(range[1]),
        p::Value::Events(p::Events::Beats { times }) => match times.as_slice() {
            [first, second, ..] if second > first => Some(second - first),
            _ => None,
        },
        other => level(other),
    }
    .or_else(|| slot.spec.default.as_ref().and_then(level))
    .unwrap_or(1.);
    let number = slot.fit(number);
    let key = color.map_or(p::Key::Number(number), p::Key::Color);
    let flat = || p::Keyframes {
        points: vec![(0., key), (1., key)],
        segments: vec![p::Segment::Linear],
    };
    match to {
        None => color.map_or_else(|| slot.plain(number), p::Value::Color),
        Some(p::SourceKind::Time) => p::Value::Time(flat()),
        Some(p::SourceKind::Hit) => p::Value::Hit(flat()),
        Some(p::SourceKind::Noise) => p::Value::Noise(p::NoiseSource {
            speed: NOISE_SPEED,
            range: [0., number],
        }),
        Some(p::SourceKind::Audio) => p::Value::Audio(p::AudioLevel {
            band: p::Band::Low,
            range: [0., number],
        }),
        Some(p::SourceKind::Events) => p::Value::Events(p::Events::Beats {
            times: p::EventTimes::new((0..STAMPS).map(|i| i as f64 * number).collect::<Vec<_>>())
                .expect("ordered beats"),
        }),
    }
}

/// A named curve preset (values 0–1) scaled to the input's range.
fn scaled(slot: &Slot, curve: &p::Keyframes) -> p::Keyframes {
    let [low, high] = slot.range();
    p::Keyframes {
        points: curve
            .points
            .iter()
            .map(|(x, key)| match key {
                p::Key::Number(v) => (*x, p::Key::Number(slot.fit(low + v * (high - low)))),
                p::Key::Color(rgb) => (*x, p::Key::Color(*rgb)),
            })
            .collect(),
        segments: curve.segments.clone(),
    }
}

fn same_curve(a: &p::Keyframes, b: &p::Keyframes) -> bool {
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-9;
    let segments = |curve: &p::Keyframes| {
        let mut segments = curve.segments.clone();
        segments.resize(curve.points.len().saturating_sub(1), p::Segment::Linear);
        segments
    };
    a.points.len() == b.points.len()
        && segments(a) == segments(b)
        && a.points.iter().zip(&b.points).all(|((ax, ak), (bx, bk))| {
            close(*ax, *bx)
                && match (ak, bk) {
                    (p::Key::Number(a), p::Key::Number(b)) => close(*a, *b),
                    (p::Key::Color(a), p::Key::Color(b)) => {
                        a.iter().zip(b).all(|(a, b)| close(*a, *b))
                    }
                    _ => false,
                }
        })
}

/// A number curve in the envelope editor's 0–1 box. The editor spans the whole
/// clip, and a curve holds its end values beyond its first and last point, so
/// flat ends are added where the curve stops short.
fn envelope_of(slot: &Slot, curve: &p::Keyframes) -> p::Envelope {
    let [low, high] = slot.range();
    let mut points: Vec<[f64; 2]> = curve
        .points
        .iter()
        .map(|(x, key)| {
            let value = match key {
                p::Key::Number(v) => *v,
                p::Key::Color(_) => low,
            };
            [*x, ((value - low) / (high - low)).clamp(0., 1.)]
        })
        .collect();
    let mut curves: Vec<p::EnvelopeCurve> = points
        .windows(2)
        .enumerate()
        .map(
            |(i, pair)| match curve.segments.get(i).copied().unwrap_or_default() {
                p::Segment::Linear => p::EnvelopeCurve::Linear,
                p::Segment::Hold => p::EnvelopeCurve::Hold,
                p::Segment::Step => p::EnvelopeCurve::Step,
                // A cubic with its handles at the thirds is the engine's ease.
                p::Segment::Ease => {
                    let (a, b) = (pair[0], pair[1]);
                    p::EnvelopeCurve::Bezier {
                        control1: [a[0] + (b[0] - a[0]) / 3., a[1]],
                        control2: [a[0] + 2. * (b[0] - a[0]) / 3., b[1]],
                    }
                }
            },
        )
        .collect();
    if points.first().is_some_and(|first| first[0] > 0.) {
        points.insert(0, [0., points[0][1]]);
        curves.insert(0, p::EnvelopeCurve::Linear);
    }
    if let Some(last) = points.last().copied().filter(|last| last[0] < 1.) {
        points.push([1., last[1]]);
        curves.push(p::EnvelopeCurve::Linear);
    }
    let envelope = p::Envelope { points, curves };
    if envelope.validate().is_ok() {
        envelope
    } else {
        p::Envelope::linear(vec![[0., 0.], [1., 0.]])
    }
}

/// The envelope editor's box back as a curve in the input's unit. A handle
/// curve is stored as the engine's ease.
fn keyframes_of(slot: &Slot, envelope: &p::Envelope) -> p::Keyframes {
    let [low, high] = slot.range();
    p::Keyframes {
        points: envelope
            .points
            .iter()
            .map(|[x, y]| (*x, p::Key::Number(slot.fit(low + y * (high - low)))))
            .collect(),
        segments: envelope
            .curves
            .iter()
            .map(|curve| match curve {
                p::EnvelopeCurve::Linear => p::Segment::Linear,
                p::EnvelopeCurve::Bezier { .. } => p::Segment::Ease,
                p::EnvelopeCurve::Hold => p::Segment::Hold,
                p::EnvelopeCurve::Step => p::Segment::Step,
            })
            .collect(),
    }
}

/// A color curve as gradient stops: each point is a stop at its progress.
fn gradient_of(curve: &p::Keyframes) -> Gradient {
    Gradient::new(curve.points.iter().map(|(x, key)| {
        let [r, g, b] = match key {
            p::Key::Color(rgb) => *rgb,
            p::Key::Number(v) => [*v; 3],
        };
        GradientStop {
            t: *x as f32,
            color: Rgba {
                r: r as f32,
                g: g as f32,
                b: b as f32,
                a: 1.,
            },
        }
    }))
}

fn color_keys(gradient: &Gradient) -> p::Keyframes {
    p::Keyframes {
        points: gradient
            .stops()
            .iter()
            .map(|stop| {
                (
                    f64::from(stop.t),
                    p::Key::Color([
                        f64::from(stop.color.r),
                        f64::from(stop.color.g),
                        f64::from(stop.color.b),
                    ]),
                )
            })
            .collect(),
        segments: Vec::new(),
    }
}

/// Beats typed as a list: numbers at or after zero, split by commas or
/// spaces. `None` while the text is not such a list.
fn parse_beats(text: &str) -> Option<Vec<f64>> {
    let mut times = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(|token| {
            token
                .parse::<f64>()
                .ok()
                .filter(|t| t.is_finite() && *t >= 0.)
        })
        .collect::<Option<Vec<_>>>()?;
    if times.is_empty() {
        return None;
    }
    times.sort_by(f64::total_cmp);
    Some(times)
}

fn format_beats(times: &[f64]) -> String {
    times
        .iter()
        .map(|t| format_value(*t))
        .collect::<Vec<_>>()
        .join(", ")
}

fn stamped(value: &p::Value) -> Option<&[f64]> {
    match value {
        p::Value::Events(p::Events::Beats { times }) => Some(times.as_slice()),
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
                  min: f64,
                  max: f64,
                  width: f32,
                  window: &mut Window,
                  cx: &mut Context<Luma>| {
        cx.new(|cx| DraftedNumber::new(label, value, min, max, width, window, cx))
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
        Some(p::Value::Time(curve) | p::Value::Hit(curve)) if curve.is_color() => {
            let hit = slot.mode == Some(p::SourceKind::Hit);
            let entity = cx.new(|cx| GradientEditor::new(gradient_of(&curve), window, cx));
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
            let entity = cx.new(|_| EnvelopeEditor::new(envelope_of(slot, &curve)));
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
                MIN_BEATS,
                1e9,
                FIELD_W,
                window,
                cx,
            );
            let low = number(
                format!("{name}: Low"),
                noise.range[0],
                0.,
                1.,
                half,
                window,
                cx,
            );
            let high = number(
                format!("{name}: High"),
                noise.range[1],
                0.,
                1.,
                half,
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
            let low = number(
                format!("{name}: Low"),
                audio.range[0],
                0.,
                1.,
                half,
                window,
                cx,
            );
            let high = number(
                format!("{name}: High"),
                audio.range[1],
                0.,
                1.,
                half,
                window,
                cx,
            );
            subs.push(on_number(&low, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.range[0] = v;
                }
            }));
            subs.push(on_number(&high, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.range[1] = v;
                }
            }));
            Widget::Audio([low, high])
        }
        Some(p::Value::Events(p::Events::Beats { times })) => {
            let text = format_beats(times.as_slice());
            let field = cx.new(|cx| {
                let mut field = TextInput::search("0, 1, 2", cx);
                field.set_text(text, cx);
                field
            });
            let def = def.clone();
            subs.push(cx.subscribe(
                &field,
                move |this: &mut Luma, field, event: &text_input::Event, cx| {
                    if *event != text_input::Event::Edited {
                        return;
                    }
                    let Some(times) = parse_beats(field.read(cx).text()) else {
                        return;
                    };
                    // A resync writes the stored list back into the field;
                    // that echo is not an edit.
                    this.form_edit(&def, spec, cx, |value| {
                        (stamped(value) != Some(&times[..])).then(|| {
                            p::Value::Events(p::Events::Beats {
                                times: p::EventTimes::new(times).expect("sorted beats"),
                            })
                        })
                    });
                },
            ));
            Widget::Stamps(field)
        }
        value => {
            let current = value.as_ref().and_then(level);
            if def.id == "grain" {
                let entity = number(
                    format!("{name}: Clump size"),
                    current.filter(|n| *n >= 2.).unwrap_or(2.),
                    2.,
                    64.,
                    FIELD_W,
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Number(v.round())
                }));
                return Widget::Grain(entity);
            }
            if slot.form == "color.time@1" && def.id == "every" {
                let entity = number(
                    format!("{name}: Beats"),
                    current.unwrap_or(0.),
                    0.,
                    1e9,
                    FIELD_W,
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Beats(v)
                }));
                return Widget::Every(entity);
            }
            if let Some(p::Author::Choice { options, .. }) = &spec.author {
                return Widget::Preset(options);
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
    match (&*widget, value) {
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
        (Widget::Audio([low, high]), Some(p::Value::Audio(audio))) => {
            low.update(cx, |field, cx| field.set_value(audio.range[0], cx));
            high.update(cx, |field, cx| field.set_value(audio.range[1], cx));
        }
        (Widget::Stamps(field), Some(value)) => {
            let times = stamped(&value).unwrap_or_default().to_vec();
            if parse_beats(field.read(cx).text()).as_deref() != Some(&times[..]) {
                field.update(cx, |field, cx| field.set_text(format_beats(&times), cx));
            }
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
        (Widget::Preset(_), _) => {}
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
/// order. The rare ones sit under "Advanced".
pub(super) fn rows(state: &Editor, built: &Built, app: &Entity<Luma>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (index, cell) in built.cells.iter().enumerate() {
        let Some(slot) = &cell.form else {
            rows.extend(arg_rows(state, app, index, cell));
            continue;
        };
        match cell.def.id.as_str() {
            // Width's own row carries it.
            "width_relative" => continue,
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
    let promote =
        (!slot.spec.promotable.is_empty()).then(|| promote_select(state, app, index, cell, slot));
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(6.))
        .w_full()
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .child(luma_ui::caption(name.to_string()))
                .child(div().flex_1())
                .children(share)
                .children(promote),
        )
        .child(control)
        .agent_node(Role::Row, name.to_string())
        .into_any_element()
}

/// The menu that switches an input between its plain value and a source.
fn promote_select(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    slot: &Slot,
) -> Div {
    let kinds = slot.spec.promotable.clone();
    let labels: Vec<&str> = std::iter::once(PLAIN)
        .chain(kinds.iter().map(|kind| source_label(*kind)))
        .collect();
    let current = slot.mode.map_or(PLAIN, source_label);
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
        state.sheet.open == Some(Menu::Source(index)),
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
    let toggle = app.clone();
    let pick = app.clone();
    let on_pick = Rc::new(on_pick);
    luma_arg_select(
        id,
        current,
        labels,
        state.sheet.open == Some(Menu::Choice(index)),
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = if editor.sheet.open == Some(Menu::Choice(index)) {
                        None
                    } else {
                        Some(Menu::Choice(index))
                    };
                });
            });
        },
        move |picked, _, cx| {
            let on_pick = on_pick.clone();
            pick.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.open = None);
                on_pick(picked, this, cx);
            });
        },
    )
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
        Widget::Preset(options) => {
            let options: &'static [p::Preset] = options;
            let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
            let current = options
                .iter()
                .find(|option| document::close(&document::wire_value(&option.value), &cell.synced))
                .map_or("Custom", |option| option.label.as_str());
            choice_select(
                state,
                app,
                index,
                name.to_string(),
                current,
                &labels,
                move |picked, this, cx| {
                    this.arg_live(&def.id, document::wire_value(&options[picked].value), cx);
                },
            )
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
                luma_ui::float::segment(label, chosen, format!("every-{label}"))
                    .id(SharedString::from(format!("every-{label}")))
                    .on_click(move |_, _, cx| {
                        let beats = if label == EVERY[0] { 0. } else { EVERY_BEATS };
                        if chosen {
                            return;
                        }
                        app.update(cx, |this, cx| {
                            this.arg_live(&id, serde_json::json!(beats), cx)
                        });
                    })
                    .agent_node(Role::Button, format!("Every {label}"))
            }));
            column()
                .child(segments)
                .when(!once, |el| el.child(entity.clone()))
        }
        Widget::Envelope(entity) if slot.mode.is_some() => {
            let curve = match &value {
                Some(p::Value::Time(curve) | p::Value::Hit(curve)) => Some(curve.clone()),
                _ => None,
            };
            let curves = &p::presets().curves;
            let labels: Vec<&str> = curves.iter().map(|curve| curve.name.as_str()).collect();
            let current = curves
                .iter()
                .find(|preset| {
                    curve
                        .as_ref()
                        .is_some_and(|curve| same_curve(&scaled(slot, &preset.curve), curve))
                })
                .map_or("Custom", |preset| preset.name.as_str());
            let shape = *slot;
            let [low, high] = slot.range();
            let unit = if slot.speed { " beats" } else { "" };
            column()
                .child(choice_select(
                    state,
                    app,
                    index,
                    format!("{name}:curve"),
                    current,
                    &labels,
                    move |picked, this, cx| {
                        let curve = scaled(&shape, &curves[picked].curve);
                        this.form_edit(&def, spec, cx, |value| match value {
                            p::Value::Time(_) => Some(p::Value::Time(curve)),
                            p::Value::Hit(_) => Some(p::Value::Hit(curve)),
                            _ => None,
                        });
                    },
                ))
                .child(entity.clone())
                .child(luma_ui::caption(format!(
                    "Values {}–{}{unit}",
                    format_value(low),
                    format_value(high)
                )))
        }
        Widget::Gradient(entity) if slot.mode.is_some() => column().child(entity.clone()).child(
            luma_ui::caption("Colors from the start to the end".to_string()),
        ),
        Widget::Noise([speed, low, high]) => column()
            .child(arg_row("Speed (beats)", speed.clone()))
            .child(arg_row("Range", range_row(low, high))),
        Widget::Audio([low, high]) => {
            let bands = [
                ("Low", p::Band::Low),
                ("Mid", p::Band::Mid),
                ("High", p::Band::High),
                ("Full", p::Band::Full),
            ];
            let band = match &value {
                Some(p::Value::Audio(audio)) => audio.band,
                _ => p::Band::Low,
            };
            let labels = bands.map(|(label, _)| label);
            let current = bands
                .iter()
                .find(|(_, at)| *at == band)
                .map_or("Low", |(label, _)| label);
            column()
                .child(arg_row(
                    "Band",
                    choice_select(
                        state,
                        app,
                        index,
                        format!("{name}:band"),
                        current,
                        &labels,
                        move |picked, this, cx| {
                            this.form_edit(&def, spec, cx, |value| match value {
                                p::Value::Audio(audio) => Some(p::Value::Audio(p::AudioLevel {
                                    band: bands[picked].1,
                                    range: audio.range,
                                })),
                                _ => None,
                            });
                        },
                    ),
                ))
                .child(arg_row("Range", range_row(low, high)))
        }
        Widget::Stamps(field) => {
            let times = value.as_ref().and_then(stamped).unwrap_or_default();
            column()
                .child(
                    luma_ui::float::field()
                        .w(px(FIELD_W))
                        .font_family(luma_ui::fonts::MONO)
                        .child(div().w_full().child(field.clone()))
                        .agent_node(
                            Role::Input,
                            format!("{name}: Beats = {}", format_beats(times)),
                        ),
                )
                .child(luma_ui::caption(
                    "Beats from the clip start, split by commas".to_string(),
                ))
        }
        Widget::Color(entity) => div().child(entity.clone()),
        Widget::Scalar(entity) => div().child(entity.clone()),
        Widget::Gradient(entity) => div().child(entity.clone()),
        Widget::Mapping(entity) => div().child(entity.clone()),
        _ => return None,
    })
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
        envelope_of, format_beats, keyframes_of, parse_beats, promote, same_curve, scaled, stamped,
        Slot, CURVE_BEATS, MIN_BEATS,
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
    fn promoting_starts_flat_at_the_plain_value_and_returns_to_it() {
        let every = slot("color.chase@1", "every");
        let time = promote(&every, &p::Value::Beats(2.), Some(p::SourceKind::Time));
        assert_eq!(
            time,
            p::Value::Time(p::Keyframes::numbers(
                &[[0., 2.], [1., 2.]],
                &[p::Segment::Linear]
            ))
        );
        let p::Value::Time(mut curve) = time else {
            unreachable!()
        };
        curve.points[0].1 = p::Key::Number(3.);
        assert_eq!(
            promote(&every, &p::Value::Time(curve), None),
            p::Value::Beats(3.)
        );

        let events = promote(&every, &p::Value::Beats(2.), Some(p::SourceKind::Events));
        assert_eq!(stamped(&events), Some(&[0., 2., 4., 6.][..]));
        assert_eq!(promote(&every, &events, None), p::Value::Beats(2.));

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
                band: p::Band::Low,
                range: [0., 0.8]
            })
        );
    }

    #[test]
    fn a_curve_round_trips_through_the_envelope_box() {
        let every = slot("color.chase@1", "every");
        let curve = p::Keyframes::numbers(
            &[[0., 2.], [0.5, 4.], [1., 0.5]],
            &[p::Segment::Hold, p::Segment::Ease],
        );
        let envelope = envelope_of(&every, &curve);
        assert!(envelope.validate().is_ok());
        assert_eq!(envelope.points, vec![[0., 0.25], [0.5, 0.5], [1., 0.0625]]);
        assert!(same_curve(&keyframes_of(&every, &envelope), &curve));

        // A curve that stops short holds its ends across the box.
        let short = p::Keyframes::numbers(&[[0.25, 0.5]], &[]);
        let alpha = slot("color.chase@1", "alpha");
        assert_eq!(
            envelope_of(&alpha, &short).points,
            vec![[0., 0.5], [0.25, 0.5], [1., 0.5]]
        );
    }

    #[test]
    fn curve_presets_scale_to_the_input() {
        let travel = slot("color.chase@1", "travel");
        let up = p::presets().curve("Ramp up").unwrap();
        let curve = scaled(&travel, up);
        assert_eq!(curve.points.first().unwrap().1, p::Key::Number(MIN_BEATS));
        assert_eq!(curve.points.last().unwrap().1, p::Key::Number(CURVE_BEATS));
    }

    #[test]
    fn beat_lists_parse_or_wait() {
        assert_eq!(parse_beats("0, 1.5 3"), Some(vec![0., 1.5, 3.]));
        assert_eq!(parse_beats("3, 1"), Some(vec![1., 3.]));
        assert_eq!(parse_beats(""), None);
        assert_eq!(parse_beats("1, x"), None);
        assert_eq!(parse_beats("-1"), None);
        assert_eq!(format_beats(&[0., 1.5]), "0, 1.5");
    }
}
