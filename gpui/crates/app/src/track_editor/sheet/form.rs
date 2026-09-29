//! One recursive editor for form inputs and the numeric inputs of sources.
use super::*;
use luma_lib::node_graph::lighting::decode;
use luma_patterns as p;
use luma_ui::arg::noise::NoisePreview;
use luma_ui::arg::strip::{self, CurveStrip, StripValue};
use serde_json::{json, Value as Json};

#[derive(Clone, Copy)]
pub(super) struct Slot {
    form: &'static str,
    key: &'static str,
    spec: &'static p::Input,
}
impl Slot {
    pub(super) fn new(form: &str, def: &PatternArgDef, _: &Json) -> Option<Self> {
        let form = *p::FORMS.iter().find(|id| **id == form)?;
        let (key, spec) = document::form_definition(form)?
            .inputs
            .get_key_value(&def.id)?;
        Some(Self { form, key, spec })
    }
}
#[derive(Clone)]
struct Target {
    form: &'static str,
    def: PatternArgDef,
    spec: &'static p::Input,
    path: String,
}
impl Target {
    fn child(&self, path: &str) -> Self {
        Self {
            path: format!("{}{path}", self.path),
            ..self.clone()
        }
    }
    fn motion_size(&self) -> bool {
        self.form == "aim@1"
            && self.path.is_empty()
            && matches!(self.def.id.as_str(), "horizontal" | "vertical" | "lean")
    }
    fn change(&self, this: &mut Luma, cx: &mut Context<Luma>, change: impl FnOnce(&mut Json)) {
        let mut edited = None;
        this.with_track_editor(cx, |editor| {
            let Ok(value) = decode(self.spec.value_type, &stored_arg(editor, &self.def)) else {
                return;
            };
            let Ok(mut raw) = serde_json::to_value(value) else {
                return;
            };
            if let Some(at) = insert_at(&mut raw, &self.path) {
                change(at);
            }
            match serde_json::from_value::<p::Value>(raw) {
                Ok(value) => edited = Some(document::wire_value(&value)),
                Err(error) => eprintln!("source edit is invalid: {error}"),
            }
        });
        if let Some(value) = edited {
            this.arg_live(&self.def.id, value, cx);
        }
    }
    fn set(&self, this: &mut Luma, cx: &mut Context<Luma>, value: Json) {
        self.change(this, cx, |at| *at = value);
    }
}
fn insert_at<'a>(value: &'a mut Json, path: &str) -> Option<&'a mut Json> {
    let mut at = value;
    for part in path.split('/').filter(|s| !s.is_empty()) {
        if at.is_array() {
            at = at.get_mut(part.parse::<usize>().ok()?)?;
        } else {
            if !at.is_object() {
                *at = json!({});
            }
            at = at.as_object_mut()?.entry(part).or_insert(Json::Null);
        }
    }
    Some(at)
}
#[derive(Clone, Copy)]
enum Units {
    Number,
    Beats,
    Share,
    Degrees,
    Metres,
    Hertz,
}
impl Units {
    fn label(self) -> Option<&'static str> {
        match self {
            Self::Beats => Some("beats"),
            Self::Degrees => Some("°"),
            Self::Metres => Some("m"),
            Self::Hertz => Some("Hz"),
            _ => None,
        }
    }
    fn bounds(self) -> [f64; 2] {
        match self {
            Self::Share => [0., 1.],
            Self::Beats => [0., 1e9],
            Self::Degrees => [-180., 180.],
            Self::Metres => [-100., 100.],
            Self::Number => [-1e9, 1e9],
            Self::Hertz => [20., 20000.],
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Self::Beats => "beats",
            Self::Share => "proportion",
            _ => "number",
        }
    }
}
pub(super) struct Field {
    label: String,
    target: Target,
    value: Json,
    units: Units,
    sources: Vec<p::SourceKind>,
    control: Control,
    children: Vec<Field>,
    shape: Json,
}
enum Control {
    None,
    Number(Entity<DraftedNumber>, bool),
    Vector(Vec<Entity<DraftedNumber>>),
    Direction([Entity<DraftedNumber>; 2]),
    Curve(Entity<CurveStrip>, [f64; 2], Option<usize>),
    Gradient(Entity<CurveStrip>),
    Choice(Vec<(&'static str, Json)>),
    /// A section title between rows; it holds no value.
    Heading,
    /// What a Noise source gives, over time and along the rig.
    NoisePreview(Entity<NoisePreview>),
}
const SOURCE_KINDS: [p::SourceKind; 5] = [
    p::SourceKind::Time,
    p::SourceKind::Space,
    p::SourceKind::Random,
    p::SourceKind::Noise,
    p::SourceKind::Audio,
];
fn source_name(kind: p::SourceKind) -> &'static str {
    match kind {
        p::SourceKind::Time => "Time",
        p::SourceKind::Space => "Space",
        p::SourceKind::Random => "Random",
        p::SourceKind::Noise => "Noise",
        p::SourceKind::Audio => "Audio",
    }
}
fn structure(value: &Json) -> Json {
    match value {
        Json::Object(map) => Json::Object(
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if key == "gain" || key == "phase" {
                            json!({
                                "shape": structure(value),
                                "default": numeric(value) == if key == "gain" { 1. } else { 0. }
                                    && value.get("value").is_some_and(Json::is_number)
                            })
                        } else if matches!(key.as_str(), "points" | "stops") && !value.is_object() {
                            Json::Null
                        } else {
                            structure(value)
                        },
                    )
                })
                .collect(),
        ),
        Json::Array(_) => Json::Null,
        Json::Number(_) => Json::Null,
        other => other.clone(),
    }
}
fn units(slot: &Slot) -> Units {
    if slot.key == "point" {
        Units::Metres
    } else if matches!(slot.key, "horizontal" | "vertical" | "lean") {
        Units::Degrees
    } else if slot
        .spec
        .default
        .as_ref()
        .is_some_and(|v| matches!(v, p::Value::Proportion(_)))
    {
        Units::Share
    } else {
        Units::Number
    }
}
fn value_at(raw: &Json, path: &str, fallback: &Json) -> Json {
    raw.pointer(path)
        .cloned()
        .unwrap_or_else(|| fallback.clone())
}
fn numeric(value: &Json) -> f64 {
    value
        .as_f64()
        .or_else(|| value.get("value").and_then(Json::as_f64))
        .unwrap_or(0.)
}
fn curve_bounds(curve: &p::Keyframes, units: Units) -> [f64; 2] {
    let [mut low, mut high] = match units {
        Units::Beats => [1. / 64., 4.],
        Units::Number => [0., 1.],
        Units::Degrees => [-1., 1.],
        other => other.bounds(),
    };
    for v in curve.values() {
        low = low.min(v);
        high = high.max(v);
    }
    if high - low < 1e-9 {
        high = low + 1.;
    }
    [low, high]
}
fn normal_curve(
    curve: &p::Keyframes,
    [low, high]: [f64; 2],
    component: Option<usize>,
) -> p::Envelope {
    curve.map(|key| {
        let v = match key {
            p::Key::Number(v) => *v,
            p::Key::Color(v) => v[component.unwrap_or(0)],
        };
        ((v - low) / (high - low)).clamp(0., 1.)
    })
}
fn default_source(kind: p::SourceKind, old: &Json, units: Units) -> Json {
    let positive = matches!(units, Units::Beats);
    let minimum = if positive { 1. / 64. } else { 0. };
    let fixed = if positive {
        numeric(old).max(minimum)
    } else {
        numeric(old)
    };
    let value = old
        .get("value")
        .filter(|v| v.is_array())
        .cloned()
        .unwrap_or(json!(fixed));
    let axis = json!({"source":{"kind":"u"},"reverse":false,"per_group":false});
    match kind {
        p::SourceKind::Time => json!({"type":"time","value":{"points":[[0,value],[1,value]]}}),
        p::SourceKind::Space => {
            if old["type"] == "color" {
                json!({"type":"space","value":{"axis":axis,"gradient":{"stops":[{"t":0,"color":value,"alpha":1},{"t":1,"color":[0,0,0],"alpha":1}]}}})
            } else {
                json!({"type":"space","value":{"axis":axis,"curve":{"points":[[0,if positive{0.125}else{0.}],[1,1]]},"gain":{"type":"number","value":if fixed==0.{1.}else{fixed}}}})
            }
        }
        p::SourceKind::Random => {
            json!({"type":"random","value":{"events":{"every":{"type":"beats","value":1}},"coverage":{"type":"proportion","value":if positive{1.}else{0.5}},"level":{"type":units.tag(),"value":if fixed==0.{1.}else{fixed}}}})
        }
        p::SourceKind::Noise => {
            json!({"type":"noise","value":{"speed":{"type":"beats","value":4},"range":[{"type":"number","value":minimum},{"type":"number","value":if fixed==0.{1.}else{fixed}}]}})
        }
        p::SourceKind::Audio => {
            json!({"type":"audio","value":{"from_hz":20,"to_hz":150,"floor":if positive{0.125}else{0.},"threshold":0}})
        }
    }
}
fn number_field(
    target: Target,
    label: String,
    value: f64,
    units: Units,
    tagged: bool,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Entity<DraftedNumber> {
    let [low, high] = units.bounds();
    let entity = cx.new(|cx| {
        let field = DraftedNumber::new(
            label,
            value,
            low.min(value),
            high.max(value),
            FIELD_W,
            window,
            cx,
        );
        let field = match units.label() {
            Some(unit) => field.with_unit(unit),
            None => field,
        };
        if matches!(units, Units::Beats) {
            field.with_per_unit("per beat", cx)
        } else {
            field
        }
    });
    subs.push(cx.subscribe(
        &entity,
        move |this: &mut Luma, _, event: &NumberEvent, cx| {
            let NumberEvent::Committed(value) = *event;
            target.set(
                this,
                cx,
                if tagged {
                    json!({"type":units.tag(),"value":value})
                } else {
                    json!(value)
                },
            );
        },
    ));
    entity
}
fn curve_field(
    target: Target,
    label: String,
    curve: p::Keyframes,
    units: Units,
    component: Option<usize>,
    color: bool,
    spatial: bool,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Control {
    let range = curve_bounds(&curve, units);
    let display = if color {
        StripValue::Colors(curve)
    } else {
        StripValue::Number(normal_curve(&curve, range, component))
    };
    let clock_target = target.clone();
    let app = cx.entity().downgrade();
    let preset_key = target
        .path
        .trim_end_matches("/value")
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&target.def.id);
    let mut strip = CurveStrip::new(label, display)
        .with_presets(if color {
            strip::color_curve_presets()
        } else if spatial || preset_key == "offset" || preset_key == "curve" {
            let presets = if spatial {
                p::shape_presets()
            } else if preset_key == "offset" {
                p::path_presets()
            } else {
                p::progress_presets()
            };
            presets
                .into_iter()
                .filter_map(|(name, value)| match value {
                    p::Value::Envelope(curve) => Some((name.into(), StripValue::Number(curve))),
                    _ => None,
                })
                .collect()
        } else {
            p::presets()
                .curves_for(preset_key)
                .map(|preset| {
                    (
                        preset.name.clone().into(),
                        StripValue::Number(preset.curve.map(|key| match key {
                            p::Key::Number(v) => *v,
                            _ => 0.,
                        })),
                    )
                })
                .collect()
        })
        .with_scale(range, units.label())
        .over_time(Rc::new(move |cx| source_clock(&app, &clock_target, cx)));
    if spatial {
        strip = strip.across_space(source_heads(cx.entity().downgrade(), target.clone()));
    }
    let entity = cx.new(|_| strip);
    subs.push(cx.subscribe(
        &entity,
        move |this: &mut Luma, _, event: &StripChanged, cx| {
            let edited = event.0.clone();
            target.change(this, cx, |at| match edited {
                StripValue::Colors(curve) => {
                    at["points"] = serde_json::to_value(curve.points).unwrap_or(Json::Null);
                }
                StripValue::Number(envelope) => {
                    let old: Option<p::Keyframes> =
                        serde_json::from_value(json!({"points":at["points"]})).ok();
                    let keys =
                        envelope.map(|v| p::Key::Number(range[0] + v * (range[1] - range[0])));
                    let keys = if let (Some(ch), Some(old)) = (component, old) {
                        p::Keyframes::with_eases(
                            keys.points.iter().map(|p| {
                                let mut v = old.sample(p.x);
                                v[ch] = match p.value {
                                    p::Key::Number(v) => v,
                                    _ => 0.,
                                };
                                (p.x, p::Key::Color(v))
                            }),
                            &keys.points.iter().map(|p| p.ease).collect::<Vec<_>>(),
                        )
                    } else {
                        keys
                    };
                    at["points"] = serde_json::to_value(keys.points).unwrap_or(Json::Null);
                }
                _ => {}
            });
        },
    ));
    Control::Curve(entity, range, component)
}
fn gradient_field(
    target: Target,
    label: String,
    gradient: p::Gradient,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Control {
    let strip = CurveStrip::new(label, StripValue::Gradient(ui_gradient(&gradient)))
        .with_presets(strip::gradient_presets())
        .across_space(source_heads(cx.entity().downgrade(), target.clone()));
    let entity = cx.new(|_| strip);
    subs.push(cx.subscribe(
        &entity,
        move |this: &mut Luma, _, event: &StripChanged, cx| {
            if let StripValue::Gradient(g) = &event.0 {
                target.set(
                    this,
                    cx,
                    serde_json::to_value(pattern_gradient(g)).unwrap_or(Json::Null),
                );
            }
        },
    ));
    Control::Gradient(entity)
}
fn choices(label: &str, target: Target, value: Json, options: Vec<(&'static str, Json)>) -> Field {
    Field {
        label: label.into(),
        target,
        value,
        units: Units::Number,
        sources: vec![],
        control: Control::Choice(options),
        children: vec![],
        shape: Json::Null,
    }
}
fn build_field(
    label: String,
    target: Target,
    value: Json,
    units: Units,
    sources: Vec<p::SourceKind>,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Field {
    let mut field = Field {
        label: label.clone(),
        target: target.clone(),
        value: value.clone(),
        units,
        sources,
        control: Control::None,
        children: vec![],
        shape: structure(&value),
    };
    let kind = value["type"].as_str().unwrap_or("");
    let child = |name: &str,
                 path: &str,
                 v: Json,
                 u: Units,
                 cx: &mut Context<Luma>,
                 subs: &mut Vec<Subscription>,
                 window: &mut Window| {
        build_field(
            name.into(),
            target.child(path),
            v,
            u,
            SOURCE_KINDS.to_vec(),
            window,
            cx,
            subs,
        )
    };
    let v = &value["value"];
    match kind {
        "number" | "proportion" | "beats" | "position" | "degrees" => {
            field.control = Control::Number(
                number_field(
                    target,
                    label,
                    numeric(&value),
                    units,
                    true,
                    window,
                    cx,
                    subs,
                ),
                true,
            )
        }
        "vector" => {
            let parts = v.as_array().cloned().unwrap_or_else(|| vec![json!(0); 3]);
            if target.def.id == "direction" && target.path.is_empty() {
                let vector: [f64; 3] = std::array::from_fn(|i| parts[i].as_f64().unwrap_or(0.));
                let turn = vector[0].atan2(vector[1]).to_degrees();
                let tilt = vector[2].atan2(vector[0].hypot(vector[1])).to_degrees();
                let entities = std::array::from_fn(|axis| {
                    let val = ((if axis == 0 { turn } else { tilt }) * 100.).round() / 100.;
                    let name = format!("{label}: {}", if axis == 0 { "Turn" } else { "Tilt" });
                    let entity = cx.new(|cx| {
                        DraftedNumber::new(name, val, -180., 180., FIELD_W, window, cx)
                            .with_unit("°")
                    });
                    let edit = target.clone();
                    subs.push(cx.subscribe(
                        &entity,
                        move |this: &mut Luma, _, event: &NumberEvent, cx| {
                            let NumberEvent::Committed(n) = *event;
                            edit.change(this, cx, |v| {
                                let a = &v["value"];
                                let u = a[0].as_f64().unwrap_or(0.);
                                let y = a[1].as_f64().unwrap_or(0.);
                                let z = a[2].as_f64().unwrap_or(0.);
                                let turn = if axis == 0 {
                                    n
                                } else {
                                    u.atan2(y).to_degrees()
                                }
                                .to_radians();
                                let tilt = if axis == 1 {
                                    n
                                } else {
                                    z.atan2(u.hypot(y)).to_degrees()
                                }
                                .to_radians();
                                v["value"] = json!([
                                    tilt.cos() * turn.sin(),
                                    tilt.cos() * turn.cos(),
                                    tilt.sin()
                                ]);
                            });
                        },
                    ));
                    entity
                });
                field.control = Control::Direction(entities);
            } else {
                field.control = Control::Vector(
                    (0..3)
                        .map(|i| {
                            number_field(
                                target.child(&format!("/value/{i}")),
                                format!("{label}: {}", ["U", "V", "Z"][i]),
                                parts[i].as_f64().unwrap_or(0.),
                                units,
                                false,
                                window,
                                cx,
                                subs,
                            )
                        })
                        .collect(),
                );
            }
        }
        "color" => {
            // Keep the established color picker at the root of a Color input.
        }
        "time" => {
            if let Ok(time) = serde_json::from_value::<p::TimeSource>(v.clone()) {
                match time.curve {
                    p::SourceCurve::Keys(keys)
                        if keys.is_color()
                            && target
                                .spec
                                .default
                                .as_ref()
                                .is_some_and(|v| matches!(v, p::Value::Vector(_))) =>
                    {
                        for ch in 0..3 {
                            let mut part = choices(
                                ["U", "V", "Z"][ch],
                                target.child("/value"),
                                v.clone(),
                                vec![],
                            );
                            part.control = curve_field(
                                target.child("/value"),
                                format!("{label} {}", ["U", "V", "Z"][ch]),
                                keys.clone(),
                                units,
                                Some(ch),
                                false,
                                false,
                                cx,
                                subs,
                            );
                            field.children.push(part);
                        }
                    }
                    p::SourceCurve::Keys(keys) => {
                        let color = keys.is_color();
                        let curve_units = if target.motion_size() {
                            Units::Number
                        } else {
                            units
                        };
                        field.control = curve_field(
                            target.child("/value"),
                            label.clone(),
                            keys,
                            curve_units,
                            None,
                            color,
                            false,
                            cx,
                            subs,
                        );
                    }
                    p::SourceCurve::Gradient(g) => {
                        let mut gradient = choices(
                            "Colors",
                            target.child("/value/gradient"),
                            v["gradient"].clone(),
                            vec![],
                        );
                        gradient.control = gradient_field(
                            gradient.target.clone(),
                            label.clone(),
                            g.gradient,
                            cx,
                            subs,
                        );
                        field.children.push(gradient);
                        let keys = g.curve.map(|v| p::Key::Number(*v));
                        let mut curve = choices(
                            "Through the colors",
                            target.child("/value/curve"),
                            v["curve"].clone(),
                            vec![],
                        );
                        curve.control = curve_field(
                            curve.target.clone(),
                            format!("{label} through colors"),
                            keys,
                            Units::Share,
                            None,
                            false,
                            false,
                            cx,
                            subs,
                        );
                        field.children.push(curve);
                    }
                }
                if target.def.id != "fade" {
                    field
                        .children
                        .extend(event_fields(&target, v, window, cx, subs));
                    if target.motion_size() || time.phase.scalar_value() != Some(0.) {
                        field.children.push(child(
                            "Phase",
                            "/value/phase",
                            serde_json::to_value(&time.phase).unwrap_or(Json::Null),
                            Units::Number,
                            cx,
                            subs,
                            window,
                        ));
                    }
                    if target.motion_size() || time.gain.scalar_value() != Some(1.) {
                        field.children.push(child(
                            if target.motion_size() {
                                "Size"
                            } else {
                                "Level"
                            },
                            "/value/gain",
                            serde_json::to_value(&time.gain).unwrap_or(Json::Null),
                            if target.motion_size() {
                                Units::Degrees
                            } else {
                                Units::Share
                            },
                            cx,
                            subs,
                            window,
                        ));
                    }
                }
            }
        }
        "space" => {
            let axis_target = target.child("/value/axis");
            field.children.push(heading("Where", &target));
            field
                .children
                .extend(mapping_fields(axis_target, &v["axis"], window, cx, subs));
            if let Ok(g) = serde_json::from_value::<p::Gradient>(v["gradient"].clone()) {
                field.control = gradient_field(
                    target.child("/value/gradient"),
                    format!("{label} colors"),
                    g,
                    cx,
                    subs,
                );
            } else if let Ok(curve) = serde_json::from_value::<p::Envelope>(v["curve"].clone()) {
                field.control = curve_field(
                    target.child("/value/curve"),
                    format!("{label} shape"),
                    curve.map(|v| p::Key::Number(*v)),
                    Units::Share,
                    None,
                    false,
                    true,
                    cx,
                    subs,
                );
            }
            field.children.push(grain_field(&target, v));
            field.children.push(heading("Motion", &target));
            field.children.push(choices("Motion",target.child("/value/offset"),v["offset"].clone(),vec![("Static",Json::Null),("Moving",json!({"type":"time","value":{"events":{"every":{"type":"beats","value":2}},"points":[[0,0],[1,1]]}}))]));
            if !v["offset"].is_null() {
                field.children.push(child(
                    "Path",
                    "/value/offset",
                    v["offset"].clone(),
                    Units::Share,
                    cx,
                    subs,
                    window,
                ));
                field.children.push(heading("Stroke", &target));
                field.children.push(child(
                    "Width",
                    "/value/width",
                    v.get("width")
                        .cloned()
                        .unwrap_or(json!({"type":"number","value":0.2})),
                    Units::Number,
                    cx,
                    subs,
                    window,
                ));
                field.children.push(choices(
                    "Width relative to",
                    target.child("/value/width_relative"),
                    v.get("width_relative").cloned().unwrap_or(json!(false)),
                    vec![("Axis", json!(false)), ("Gap", json!(true))],
                ));
                field.children.push(choices(
                    "Boundary",
                    target.child("/value/boundary"),
                    v.get("boundary").cloned().unwrap_or(json!("clip")),
                    vec![("Clip", json!("clip")), ("Wrap", json!("wrap"))],
                ));
            }
            let gain = v
                .get("gain")
                .cloned()
                .unwrap_or(json!({"type":"number","value":1}));
            if target.motion_size()
                || serde_json::from_value::<p::Value>(gain.clone())
                    .is_ok_and(|gain| gain.scalar_value() != Some(1.))
            {
                field.children.push(child(
                    if target.motion_size() {
                        "Size"
                    } else {
                        "Level"
                    },
                    "/value/gain",
                    gain,
                    units,
                    cx,
                    subs,
                    window,
                ));
            }
        }
        "random" => {
            field
                .children
                .extend(event_fields(&target, v, window, cx, subs));
            field.children.push(grain_field(&target, v));
            for (key, name) in [("coverage", "Coverage"), ("level", "Level")] {
                field.children.push(child(
                    name,
                    &format!("/value/{key}"),
                    v.get(key)
                        .cloned()
                        .unwrap_or(json!({"type":"proportion","value":1})),
                    Units::Share,
                    cx,
                    subs,
                    window,
                ));
            }
        }
        "noise" => {
            let transport = clip_transport(cx.entity().downgrade());
            let preview = cx.new(|_| {
                NoisePreview::new(
                    label.clone(),
                    form_path(&target),
                    matches!(units, Units::Share),
                    transport,
                )
            });
            preview.update(cx, |preview, cx| {
                preview.set_value(serde_json::from_value(v.clone()).ok(), cx)
            });
            field.control = Control::NoisePreview(preview);
            field.children.push(heading("Range", &target));
            for i in 0..2 {
                field.children.push(child(
                    if i == 0 { "Low" } else { "High" },
                    &format!("/value/range/{i}"),
                    v["range"][i].clone(),
                    units,
                    cx,
                    subs,
                    window,
                ));
            }
            field.children.push(heading("Motion", &target));
            field.children.push(child(
                "Speed",
                "/value/speed",
                v.get("speed")
                    .cloned()
                    .unwrap_or(json!({"type":"beats","value":4.})),
                Units::Beats,
                cx,
                subs,
                window,
            ));
            field.children.push(heading("Look", &target));
            field.children.push(grain_field(&target, v));
            field.children.push(choices(
                "Space",
                target.child("/value/scale"),
                v["scale"].clone(),
                vec![
                    ("Uniform", Json::Null),
                    ("Spatial", json!({"type":"number","value":0.5})),
                ],
            ));
            for (key, name, default) in [("scale", "Scale", 0.5), ("contrast", "Contrast", 0.)] {
                if key == "scale" && v[key].is_null() {
                    continue;
                }
                field.children.push(child(
                    name,
                    &format!("/value/{key}"),
                    v.get(key)
                        .cloned()
                        .unwrap_or(json!({"type":"proportion","value":default})),
                    Units::Share,
                    cx,
                    subs,
                    window,
                ));
            }
            field.children.push(choices(
                "Wandering",
                target.child("/value/independent"),
                v.get("independent").cloned().unwrap_or(json!(false)),
                vec![("Together", json!(false)), ("Per unit", json!(true))],
            ));
        }
        "audio" => {
            field.children.push(choices(
                "Band",
                target.child("/value"),
                json!({"from_hz":v["from_hz"],"to_hz":v["to_hz"]}),
                p::presets()
                    .frequencies
                    .iter()
                    .map(|band| {
                        (
                            band.name.as_str(),
                            json!({"from_hz":band.from_hz,"to_hz":band.to_hz}),
                        )
                    })
                    .collect(),
            ));
            for (key, name, u, default) in [
                ("from_hz", "From Hz", Units::Hertz, 20.),
                ("to_hz", "To Hz", Units::Hertz, 150.),
                ("floor", "Floor", Units::Share, 0.),
                ("threshold", "Threshold", Units::Share, 0.),
            ] {
                let target = target.child(&format!("/value/{key}"));
                let val = v.get(key).cloned().unwrap_or(json!(default));
                let mut f = choices(name, target.clone(), val.clone(), vec![]);
                f.control = Control::Number(
                    number_field(
                        target,
                        format!("{label}: {name}"),
                        numeric(&val),
                        u,
                        false,
                        window,
                        cx,
                        subs,
                    ),
                    false,
                );
                field.children.push(f);
            }
            field.children.push(child(
                if target.motion_size() {
                    "Size"
                } else {
                    "Level"
                },
                "/value/gain",
                v.get("gain")
                    .cloned()
                    .unwrap_or(json!({"type":"number","value":1})),
                units,
                cx,
                subs,
                window,
            ));
        }
        "mapping" => {
            field
                .children
                .extend(mapping_fields(target.child("/value"), v, window, cx, subs))
        }
        "choice" => {
            if let Some(p::Author::Choice { options, .. }) = &target.spec.author {
                // The option labels are static catalog data.
                field.control = Control::Choice(
                    options
                        .iter()
                        .map(|o| {
                            (
                                o.label.as_str(),
                                serde_json::to_value(&o.value).unwrap_or(Json::Null),
                            )
                        })
                        .collect(),
                );
            }
        }
        _ => {}
    }
    field
}
fn heading(label: &str, target: &Target) -> Field {
    let mut field = choices(label, target.clone(), Json::Null, vec![]);
    field.control = Control::Heading;
    field
}
/// The path the form lowering names a source by, "brightness/low" for
/// "/value/range/0" under Brightness: a noise source's default key.
fn form_path(target: &Target) -> String {
    let mut parts = vec![target.def.id.as_str()];
    let mut path = target
        .path
        .split('/')
        .filter(|part| !matches!(*part, "" | "value" | "events"));
    while let Some(part) = path.next() {
        parts.push(match part {
            "range" if path.next() == Some("0") => "low",
            "range" => "high",
            _ => part,
        });
    }
    parts.join("/")
}
/// The clip's transport for a noise preview: beats since the clip start, the
/// origin playback evaluates the clip from, or 0 while the playhead is
/// outside the clip.
fn clip_transport(app: WeakEntity<Luma>) -> luma_ui::arg::noise::TransportSource {
    Rc::new(move |cx| {
        let mut transport = luma_ui::arg::noise::Transport::default();
        let Some(app) = app.upgrade() else {
            return transport;
        };
        let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
            return transport;
        };
        let Some(clip) = primary_clip(editor) else {
            return transport;
        };
        let timeline = editor.beats.as_deref().and_then(|b| b.timeline().ok());
        let (start, length) = match (&clip.core, &timeline) {
            (Some(core), _) => (Some(core.start), Some(core.duration)),
            (None, Some(timeline)) => {
                let start = timeline.beat_at(clip.start).ok();
                let end = timeline.beat_at(clip.end).ok();
                (start, start.zip(end).map(|(a, b)| b - a))
            }
            _ => (None, None),
        };
        transport.seed = clip.core.as_ref().map_or(0, |core| core.seed);
        transport.playing = editor.transport.playing;
        let now = timeline.and_then(|t| t.beat_at(f64::from(editor.transport.position)).ok());
        if let (Some(now), Some(start), Some(length)) = (now, start, length) {
            let elapsed = now - start;
            if (0. ..=length).contains(&elapsed) {
                transport.beat = elapsed;
            }
        }
        transport
    })
}
fn grain_field(target: &Target, value: &Json) -> Field {
    choices(
        "Grain",
        target.child("/value/grain"),
        value.get("grain").cloned().unwrap_or(json!("head")),
        vec![
            ("Head", json!("head")),
            ("Fixture", json!("fixture")),
            ("Clump of 2", json!("clump2")),
            ("Clump of 4", json!("clump4")),
            ("Clump of 8", json!("clump8")),
        ],
    )
}
fn event_fields(
    target: &Target,
    value: &Json,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Vec<Field> {
    let clock = value.get("events").cloned().unwrap_or(Json::Null);
    let mut opts = vec![
        ("Inherit", Json::Null),
        ("Over clip", json!({"every":{"type":"beats","value":0}})),
        ("Repeat", json!({"every":{"type":"beats","value":2}})),
    ];
    let mut result = Vec::new();
    // Cross-input clocks remain references when the leader's timing is edited.
    for (key, label) in [
        ("brightness", "Follow brightness"),
        ("color", "Follow color"),
        ("horizontal", "Follow horizontal"),
        ("vertical", "Follow vertical"),
    ] {
        if target.def.id != key
            && document::form_definition(target.form).is_some_and(|d| d.inputs.contains_key(key))
        {
            opts.push((label, json!({"same_as":key})));
        }
    }
    result.push(choices(
        "Events",
        target.child("/value/events"),
        clock.clone(),
        opts,
    ));
    // Every 0 beats is "Over clip": one event, so Every and Duration do not apply.
    let over_clip = numeric(&clock["every"]) == 0. && clock["every"]["type"] == "beats";
    if clock.get("every").is_some() && !over_clip {
        result.push(build_field(
            "Every".into(),
            target.child("/value/events/every"),
            clock["every"].clone(),
            Units::Beats,
            SOURCE_KINDS.to_vec(),
            window,
            cx,
            subs,
        ));
        {
            result.push(choices(
                "Duration",
                target.child("/value/events/life"),
                clock["life"].clone(),
                vec![
                    ("Same as every", Json::Null),
                    ("Custom", json!({"type":"beats","value":2})),
                ],
            ));
            if !clock["life"].is_null() {
                result.push(build_field(
                    "Custom duration".into(),
                    target.child("/value/events/life"),
                    clock["life"].clone(),
                    Units::Beats,
                    SOURCE_KINDS.to_vec(),
                    window,
                    cx,
                    subs,
                ));
            }
        }
    }
    result
}
fn mapping_fields(
    target: Target,
    value: &Json,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Vec<Field> {
    let mut fields = vec![
        choices(
            "Axis",
            target.child("/source/kind"),
            value["source"]["kind"].clone(),
            vec![
                ("Order", json!("order")),
                ("X", json!("u")),
                ("Y", json!("v")),
                ("Z", json!("z")),
                ("Radial", json!("radial")),
                ("Angle", json!("angle")),
                ("Random", json!("random")),
            ],
        ),
        choices(
            "Span",
            target.child("/span"),
            value.get("span").cloned().unwrap_or(json!("selection")),
            vec![
                ("Selection", json!("selection")),
                ("Fixture", json!("fixture")),
                ("Group", json!("group")),
            ],
        ),
    ];
    if matches!(value["source"]["kind"].as_str(), Some("radial" | "angle")) {
        fields.push(choices(
            "Plane",
            target.child("/plane"),
            value
                .get("plane")
                .cloned()
                .unwrap_or(json!({"kind":"auto"})),
            vec![
                ("Auto", json!({"kind":"auto"})),
                ("Around up–down", json!({"kind":"up_down"})),
                ("Around front–back", json!({"kind":"front_back"})),
                ("Around left–right", json!({"kind":"left_right"})),
                ("Custom", json!({"kind":"custom","normal":[0,-1,0]})),
            ],
        ));
        if value["plane"]["kind"] == "custom" {
            for i in 0..3 {
                let t = target.child(&format!("/plane/normal/{i}"));
                let v = value["plane"]["normal"][i].clone();
                let mut f = choices(
                    ["Plane U", "Plane V", "Plane Z"][i],
                    t.clone(),
                    v.clone(),
                    vec![],
                );
                f.control = Control::Number(
                    number_field(
                        t,
                        f.label.clone(),
                        numeric(&v),
                        Units::Number,
                        false,
                        window,
                        cx,
                        subs,
                    ),
                    false,
                );
                fields.push(f);
            }
        }
    } else if matches!(
        value["source"]["kind"].as_str(),
        Some("u" | "v" | "z" | "vector" | "major_axis")
    ) {
        fields.push(choices(
            "Mirror",
            target.child("/mirror"),
            value["mirror"].clone(),
            vec![
                ("Off", Json::Null),
                ("Left–right", json!({"normal":[1,0,0],"offset":0})),
                ("Front–back", json!({"normal":[0,1,0],"offset":0})),
                ("Up–down", json!({"normal":[0,0,1],"offset":0})),
            ],
        ));
        if value["mirror"].is_object() {
            for (key, name) in [
                ("normal/0", "Mirror U"),
                ("normal/1", "Mirror V"),
                ("normal/2", "Mirror Z"),
                ("offset", "Mirror offset"),
            ] {
                let t = target.child(&format!("/mirror/{key}"));
                let v = value["mirror"]
                    .pointer(&format!("/{key}"))
                    .cloned()
                    .unwrap_or(json!(0));
                let mut f = choices(name, t.clone(), v.clone(), vec![]);
                f.control = Control::Number(
                    number_field(
                        t,
                        name.into(),
                        numeric(&v),
                        Units::Number,
                        false,
                        window,
                        cx,
                        subs,
                    ),
                    false,
                );
                fields.push(f);
            }
        }
    }
    fields
}
pub(super) fn widget(
    slot: &Slot,
    def: &PatternArgDef,
    stored: &Json,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Widget {
    let Ok(value) = decode(slot.spec.value_type, stored) else {
        return Widget::Invalid("Unreadable input".into());
    };
    if matches!(value, p::Value::Color(_)) {
        return super::plain_widget(def, stored, true, &[], window, cx, subs);
    }
    let raw = serde_json::to_value(value).unwrap_or(Json::Null);
    let target = Target {
        form: slot.form,
        def: def.clone(),
        spec: slot.spec,
        path: String::new(),
    };
    Widget::Form(Box::new(build_field(
        slot.spec.name.clone(),
        target,
        raw,
        units(slot),
        slot.spec.promotable.clone(),
        window,
        cx,
        subs,
    )))
}
fn source_heads(app: WeakEntity<Luma>, target: Target) -> strip::HeadSource {
    type Resolved = (p::MappingSpec, Rc<[p::Cell]>, u64, Rc<[f64]>);
    let cache: Rc<std::cell::RefCell<Option<Resolved>>> = Rc::default();
    Rc::new(move |cx| {
        let app = app.upgrade()?;
        let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
            return None;
        };
        let clip = primary_clip(editor)?;
        let root = decode(target.spec.value_type, clip.args.get(&target.def.id)?).ok()?;
        let raw = serde_json::to_value(root).ok()?;
        let path = target
            .path
            .trim_end_matches("/value/curve")
            .trim_end_matches("/value/gradient");
        let p::Value::Space(space) = serde_json::from_value(raw.pointer(path)?.clone()).ok()?
        else {
            return None;
        };
        if space.offset.is_some() {
            return None;
        }
        let cells = editor.sheet.heads.cells.clone()?;
        let seed = clip.core.as_ref().map_or(0, |clip| clip.seed);
        let mut cache = cache.borrow_mut();
        if let Some((axis, held, at, positions)) = cache.as_ref() {
            if *axis == space.axis && Rc::ptr_eq(held, &cells) && *at == seed {
                return Some(positions.clone());
            }
        }
        let positions: Rc<[f64]> = space
            .axis
            .resolve(&cells, seed)
            .ok()?
            .coordinates
            .iter()
            .map(|c| c.position)
            .collect();
        *cache = Some((space.axis, cells, seed, positions.clone()));
        Some(positions)
    })
}
fn source_clock(app: &WeakEntity<Luma>, target: &Target, cx: &App) -> Option<strip::Clock> {
    let app = app.upgrade()?;
    let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
        return None;
    };
    let clip = primary_clip(editor)?;
    let timeline = editor.beats.as_deref()?.timeline().ok()?;
    let start = timeline.beat_at(clip.start).ok()?;
    let length = timeline.beat_at(clip.end).ok()? - start;
    let elapsed = timeline
        .beat_at(f64::from(editor.transport.position))
        .ok()?
        - start;
    let root = decode(target.spec.value_type, clip.args.get(&target.def.id)?).ok()?;
    let raw = serde_json::to_value(root).ok()?;
    // An inherited curve reads the nearest enclosing clock, including a
    // Space source's offset. Cross-input references follow that input's clock.
    fn clock_at(value: &Json) -> Option<Json> {
        if let Some(events) = value.get("events").filter(|e| !e.is_null()) {
            return Some(events.clone());
        }
        if value["type"] == "space" {
            return clock_at(&value["value"]["offset"]);
        }
        value.get("value").and_then(clock_at)
    }
    let mut path = target.path.as_str();
    let mut events = None;
    loop {
        if let Some(at) = raw.pointer(path) {
            events = clock_at(at);
        }
        if events.is_some() || path.is_empty() {
            break;
        }
        path = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    }
    for _ in 0..24 {
        let Some(key) = events.as_ref().and_then(|e| e["same_as"].as_str()) else {
            break;
        };
        let spec = document::form_definition(target.form)?.inputs.get(key)?;
        let leader = decode(spec.value_type, clip.args.get(key)?).ok()?;
        events = clock_at(&serde_json::to_value(leader).ok()?);
    }
    let events = events.as_ref();
    let (span, phase) = match events.and_then(|e| e.get("every")) {
        Some(e) if e["value"].as_f64().is_some_and(|e| e > 0.) => {
            let every = e["value"].as_f64()?;
            let life = match events.and_then(|e| e.get("life")).filter(|v| !v.is_null()) {
                Some(value) => value["value"].as_f64()?,
                None => every,
            };
            let life = if life == 0. { length } else { life };
            (life, elapsed.rem_euclid(every) / life)
        }
        Some(e) if e["value"].as_f64() != Some(0.) => return None,
        _ => (length, elapsed / length),
    };
    Some(strip::Clock {
        beats: span,
        phase: ((0. ..=length).contains(&elapsed) && (0. ..=1.).contains(&phase)).then_some(phase),
        playing: editor.transport.playing,
    })
}
fn menu_id(target: &Target, suffix: &str) -> usize {
    format!("{}{}{suffix}", target.def.id, target.path)
        .bytes()
        .fold(0xcbf29ce484222325usize, |h, b| {
            (h ^ usize::from(b)).wrapping_mul(0x100000001b3)
        })
}
fn select(
    state: &Editor,
    app: &Entity<Luma>,
    target: &Target,
    suffix: &str,
    current: &str,
    labels: &[&str],
    pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    let menu = Menu::Choice(menu_id(target, suffix));
    let toggle = app.clone();
    let choose = app.clone();
    let pick = Rc::new(pick);
    luma_arg_select(
        format!("{} {} {suffix}", target.def.name, target.path),
        current,
        labels,
        menu_visibility(state, menu),
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = if editor.sheet.open == Some(menu) {
                        None
                    } else {
                        Some(menu)
                    }
                })
            });
        },
        move |at, _, cx| {
            let pick = pick.clone();
            choose.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.open = None);
                pick(at, this, cx);
            });
        },
    )
}
fn source_select(
    state: &Editor,
    app: &Entity<Luma>,
    target: &Target,
    value: &Json,
    units: Units,
    kinds: &[p::SourceKind],
) -> Div {
    let current = value
        .get("type")
        .and_then(Json::as_str)
        .and_then(|t| {
            kinds
                .iter()
                .find(|k| source_name(**k).eq_ignore_ascii_case(t))
        })
        .map(|k| source_name(*k))
        .unwrap_or("Fixed");
    let mut labels = vec!["Fixed"];
    labels.extend(kinds.iter().map(|k| source_name(*k)));
    let kinds = kinds.to_vec();
    let target = target.clone();
    let edit = target.clone();
    select(
        state,
        app,
        &target,
        "source",
        current,
        &labels,
        move |at, this, cx| {
            edit.change(this, cx, |value| {
                *value = if at == 0 {
                    if let Some(default) = &edit.spec.default {
                        if edit.path.is_empty()
                            && matches!(default, p::Value::Vector(_) | p::Value::Color(_))
                        {
                            serde_json::to_value(default).unwrap_or(Json::Null)
                        } else {
                            json!({"type":units.tag(),"value":1})
                        }
                    } else {
                        json!({"type":units.tag(),"value":1})
                    }
                } else {
                    let mut source = default_source(kinds[at - 1], value, units);
                    if edit.motion_size()
                        && kinds[at - 1] == p::SourceKind::Time
                        && value.get("value").is_some_and(Json::is_number)
                    {
                        source["value"]["points"] = json!([[0, 1], [1, 1]]);
                        source["value"]["gain"] = json!({"type":"number","value":numeric(value)});
                    }
                    source
                };
            });
        },
    )
}
fn field_control(field: &Field, state: &Editor, app: &Entity<Luma>) -> Div {
    let mut el = div().w_full().flex().flex_col().gap(px(8.));
    match &field.control {
        Control::Number(entity, _) => el = el.child(entity.clone()),
        Control::Vector(parts) => {
            for (i, entity) in parts.iter().enumerate() {
                el = el.child(arg_row(["U", "V", "Z"][i], entity.clone()));
            }
        }
        Control::Direction(parts) => {
            for (i, entity) in parts.iter().enumerate() {
                el = el.child(arg_row(["Turn", "Tilt"][i], entity.clone()));
            }
        }
        Control::Curve(entity, ..) | Control::Gradient(entity) => el = el.child(entity.clone()),
        Control::NoisePreview(entity) => el = el.child(entity.clone()),
        Control::Choice(options) => {
            let options = options.clone();
            let labels: Vec<_> = options.iter().map(|(s, _)| *s).collect();
            let current = options
                .iter()
                .position(|(_, v)| {
                    if field.label == "Band" {
                        v["from_hz"] == field.value["from_hz"] && v["to_hz"] == field.value["to_hz"]
                    } else {
                        v == &field.value
                    }
                })
                .or_else(|| {
                    if field.label == "Events" && field.value.get("every").is_some() {
                        Some(if numeric(&field.value["every"]) == 0. {
                            1
                        } else {
                            2
                        })
                    } else if field.label == "Motion"
                        || field.label == "Space"
                        || field.label == "Duration"
                    {
                        Some(usize::from(!field.value.is_null()))
                    } else {
                        None
                    }
                });
            let target = field.target.clone();
            let edit = target.clone();
            let shown = current.map_or("Custom", |i| labels[i]).to_string();
            el = el.child(select(
                state,
                app,
                &target,
                &field.label,
                &shown,
                &labels,
                move |i, this, cx| {
                    let chosen = options[i].1.clone();
                    if edit.path.ends_with("/source/kind") {
                        let parent = Target {
                            path: edit.path.trim_end_matches("/source/kind").into(),
                            ..edit.clone()
                        };
                        parent.change(this, cx, |axis| {
                            axis["source"] = json!({"kind":chosen});
                            let round = matches!(chosen.as_str(), Some("radial" | "angle"));
                            if round {
                                axis["plane"] = json!({"kind":"auto"});
                                axis["mirror"] = Json::Null;
                            } else {
                                axis["plane"] = Json::Null;
                            }
                            if matches!(chosen.as_str(), Some("order" | "random")) {
                                axis["mirror"] = Json::Null;
                            }
                        });
                    } else if chosen.get("from_hz").is_some() {
                        edit.change(this, cx, |value| {
                            value["from_hz"] = chosen["from_hz"].clone();
                            value["to_hz"] = chosen["to_hz"].clone();
                        });
                    } else {
                        edit.set(this, cx, chosen);
                    }
                },
            ));
        }
        Control::None | Control::Heading => {}
    }
    let mut first_heading = true;
    for child in &field.children {
        if matches!(child.control, Control::Heading) {
            el = el.child(
                luma_ui::float::group_heading(child.label.clone(), first_heading)
                    .agent_node(Role::Text, child.label.clone()),
            );
            first_heading = false;
            continue;
        }
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(luma_ui::caption(child.label.clone()))
            .when(!child.sources.is_empty(), |el| {
                el.child(source_select(
                    state,
                    app,
                    &child.target,
                    &child.value,
                    child.units,
                    &child.sources,
                ))
            });
        el = el.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(header)
                .child(grouped(child, field_control(child, state, app)))
                .agent_node(Role::Row, child.label.clone()),
        );
    }
    el
}
/// A source's own settings sit under it, behind a rule, so a nested Events
/// or Every never reads as the parent's, and a top-level source's rows read
/// as its own.
fn grouped(field: &Field, control: Div) -> Div {
    control.when(!field.children.is_empty(), |el| {
        el.border_l_2()
            .border_color(luma_ui::ladder::border())
            .pl(px(10.))
    })
}
fn sync_field(field: &mut Field, raw: &Json, cx: &mut Context<Luma>) {
    let value = value_at(raw, &field.target.path, &field.value);
    field.value = value.clone();
    match &field.control {
        Control::Number(entity, tagged) => {
            let n = if *tagged {
                numeric(&value)
            } else {
                value.as_f64().unwrap_or(0.)
            };
            entity.update(cx, |f, cx| f.set_value(n, cx));
        }
        Control::Vector(parts) => {
            for (i, entity) in parts.iter().enumerate() {
                entity.update(cx, |f, cx| {
                    f.set_value(value["value"][i].as_f64().unwrap_or(0.), cx)
                });
            }
        }
        Control::Direction(parts) => {
            let a = &value["value"];
            let u = a[0].as_f64().unwrap_or(0.);
            let v = a[1].as_f64().unwrap_or(0.);
            let z = a[2].as_f64().unwrap_or(0.);
            for (entity, value) in parts
                .iter()
                .zip([u.atan2(v).to_degrees(), z.atan2(u.hypot(v)).to_degrees()])
            {
                entity.update(cx, |f, cx| f.set_value((value * 100.).round() / 100., cx));
            }
        }
        Control::Curve(entity, range, component) => {
            let curve = if value["type"] == "time" {
                &value["value"]
            } else if value["type"] == "space" {
                &value["value"]["curve"]
            } else {
                &value
            };
            if let Ok(curve) =
                serde_json::from_value::<p::Keyframes>(json!({"points":curve["points"]}))
            {
                let value = if curve.is_color() && component.is_none() {
                    StripValue::Colors(curve)
                } else {
                    StripValue::Number(normal_curve(&curve, *range, *component))
                };
                entity.update(cx, |e, cx| e.set_value(value, cx));
            }
        }
        Control::NoisePreview(entity) => {
            let source = serde_json::from_value(value["value"].clone()).ok();
            entity.update(cx, |preview, cx| preview.set_value(source, cx));
        }
        Control::Gradient(entity) => {
            let value = if value["type"] == "space" {
                &value["value"]["gradient"]
            } else {
                &value
            };
            if let Ok(g) = serde_json::from_value::<p::Gradient>(value.clone()) {
                entity.update(cx, |e, cx| {
                    e.set_value(StripValue::Gradient(ui_gradient(&g)), cx)
                });
            }
        }
        _ => {}
    }
    for child in &mut field.children {
        sync_field(child, raw, cx);
    }
}
pub(super) fn resync(
    slot: &mut Slot,
    def: &PatternArgDef,
    widget: &mut Widget,
    stored: &Json,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> bool {
    let Ok(value) = decode(slot.spec.value_type, stored) else {
        return false;
    };
    let Ok(raw) = serde_json::to_value(&value) else {
        return false;
    };
    match widget {
        Widget::Form(field) if field.shape == structure(&raw) => {
            sync_field(field, &raw, cx);
            true
        }
        Widget::Color(_) if matches!(value, p::Value::Color(_)) => false,
        _ => {
            *widget = self::widget(slot, def, stored, window, cx, subs);
            true
        }
    }
}
pub(super) fn rows(state: &Editor, built: &Built, app: &Entity<Luma>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    let base = built
        .cells
        .iter()
        .find(|c| c.def.id == "base")
        .and_then(|c| c.synced.as_str())
        .unwrap_or("direction");
    for (index, cell) in built.cells.iter().enumerate() {
        let Some(slot) = &cell.form else {
            rows.extend(arg_rows(state, app, index, cell));
            continue;
        };
        if slot.form == "aim@1" {
            if built.blend == BlendMode::Offset
                && matches!(slot.key, "base" | "direction" | "point")
            {
                continue;
            }
            if (slot.key == "direction" && base != "direction")
                || (slot.key == "point" && base != "point")
            {
                continue;
            }
        }
        let control = match &cell.widget {
            Widget::Form(field) => grouped(field, field_control(field, state, app)),
            Widget::Color(entity) => div().child(entity.clone()),
            Widget::Invalid(error) => div().child(error.clone()),
            _ => continue,
        };
        let mut accessories = Vec::new();
        let fixed_position = slot.form == "aim@1"
            && matches!(slot.key, "direction" | "point")
            && decode(slot.spec.value_type, &cell.synced)
                .is_ok_and(|value| value.source_kind().is_none());
        if !fixed_position && !slot.spec.promotable.is_empty() {
            let target = Target {
                form: slot.form,
                def: cell.def.clone(),
                spec: slot.spec,
                path: String::new(),
            };
            if let Ok(value) = decode(slot.spec.value_type, &cell.synced) {
                accessories.push(
                    source_select(
                        state,
                        app,
                        &target,
                        &serde_json::to_value(value).unwrap_or(Json::Null),
                        units(slot),
                        &slot.spec.promotable,
                    )
                    .into_any_element(),
                );
            }
        }
        rows.push(sheet_row(&slot.spec.name, accessories, control));
    }
    rows
}
fn ui_gradient(gradient: &p::Gradient) -> Gradient {
    Gradient::new(gradient.stops.iter().map(|stop| GradientStop {
        t: stop.t as f32,
        color: Light {
            a: stop.alpha as f32,
            ..Light::opaque(stop.color)
        },
    }))
}
fn pattern_gradient(gradient: &Gradient) -> p::Gradient {
    let mut stops: Vec<_> = gradient
        .stops()
        .iter()
        .map(|stop| p::ColorStop {
            t: f64::from(stop.t).clamp(0., 1.),
            color: stop.color.channels().map(|v| v.clamp(0., 1.)),
            alpha: f64::from(stop.color.a).clamp(0., 1.),
        })
        .collect();
    stops.sort_by(|a, b| a.t.total_cmp(&b.t));
    p::Gradient { stops }
}
