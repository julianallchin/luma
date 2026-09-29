//! The curve strip: one editor for a value that changes along x, 0 to 1.
//!
//! A number is a curve in a 0–1 box: its points move in x and y, and each
//! segment has an ease, drawn with real Bézier handles. A color is a row of
//! stops over its own fill: stops move in x only. Color keyframes have an
//! ease per segment too; a gradient has none.
//!
//! Every value takes the same gestures: a double-click adds a point, a drag
//! moves one, a right-click removes one, a press selects one. The row under
//! the strip edits the selected point: its position, its value (a number
//! field or a swatch whose picker has hex and opacity) and, where the value
//! has eases, the ease of the segment from it.
//!
//! The host says what x is. Over time, a clock gives the beats the strip
//! spans and the playhead's phase: the strip draws a beat grid, bars
//! stronger, and the playhead, and follows the playhead itself while the
//! transport runs. Across space, the host gives each head's place on the
//! axis and the strip draws one tick per head in the color it gets.
use std::rc::Rc;

use super::color::{ColorArg, ColorArgEditor, ColorArgEvent, ColorOpacity};
use super::gradient::{gradient_fill, Gradient, GradientStop, Light};
use super::number::{format_value, DraftedNumber, NumberEvent};
use super::preset_picker::{luma_preset_picker, Thumb};
use super::select::MenuVisibility;
use crate::{
    ladder,
    node::{Instrument, Role},
};
use gpui::prelude::*;
use gpui::{
    canvas, div, fill, linear_color_stop, linear_gradient, point, px, size, App, Background,
    Bounds, Context, Entity, EventEmitter, MouseButton, PathBuilder, Pixels, Point, SharedString,
    Subscription, Window,
};
use luma_patterns::{CurvePoint, Ease, Envelope, Key, Keyframes};

/// A value the strip edits.
#[derive(Clone, Debug, PartialEq)]
pub enum StripValue {
    /// A number curve in the 0–1 box. Its ends stay at x 0 and 1.
    Number(Envelope),
    /// Color stops with opacity, and no eases. Any number of stops, from none.
    Gradient(Gradient),
    /// Color keyframes, blended in RGB (linear Rec. 2020), with an ease per
    /// segment. Its ends stay at x 0 and 1.
    Colors(Keyframes),
}

/// The strip's value after an edit: a drag let go, a point added or
/// removed, a field committed, a preset picked.
#[derive(Clone, Debug)]
pub struct StripChanged(pub StripValue);

/// Where a strip over time is, each frame it draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    /// Beats the strip spans from x 0 to x 1.
    pub beats: f64,
    /// The playhead, 0–1, while the track time is inside the span.
    pub phase: Option<f64>,
    /// The transport runs, so the playhead moves between notifies.
    pub playing: bool,
}

/// Reads the clock of a strip over time.
pub type ClockSource = Rc<dyn Fn(&App) -> Option<Clock>>;
/// Reads the place of each head on a strip's axis, 0–1, or `None` while x
/// is not that axis.
pub type HeadSource = Rc<dyn Fn(&App) -> Option<Rc<[f64]>>>;

/// What the x axis measures.
#[derive(Clone, Default)]
enum Axis {
    #[default]
    Plain,
    Time(ClockSource),
    Space(HeadSource),
}

/// The most stops a gradient takes, and points a curve takes.
const MAX_STOPS: usize = 64;
const MAX_POINTS: usize = 256;
/// The box's air around the drawing area.
const INSET: f32 = 9.;
/// The drawing area's height: a number's box, a color's fill.
const NUMBER_H: f32 = 120.;
const COLOR_H: f32 = 40.;
/// How near a press must land to take a point, in pixels.
const REACH: f64 = 9.;
/// A color stop's marker.
const STOP: f32 = 12.;
/// The row of head ticks under the drawing area.
const TICK_H: f32 = 10.;
/// The position field, in percent.
const PERCENT_W: f32 = 58.;
/// The number value field.
const VALUE_W: f32 = 76.;
/// The most beat lines drawn; past it only bars are, and past that none.
const MAX_LINES: f64 = 96.;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Point(usize),
    Handle(usize, usize),
}

/// The fields of the selected-point row, made on first render.
struct Fields {
    position: Entity<DraftedNumber>,
    value: ValueField,
    _subscriptions: Vec<Subscription>,
}

enum ValueField {
    Number(Entity<DraftedNumber>),
    Color(Entity<ColorArgEditor>),
}

pub struct CurveStrip {
    id: SharedString,
    value: StripValue,
    bounds: Option<Bounds<Pixels>>,
    /// The selected point.
    selected: usize,
    dragging: Option<Drag>,
    before_drag: Option<StripValue>,
    error: Option<String>,
    presets: Vec<(SharedString, StripValue)>,
    presets_menu: MenuVisibility,
    /// The span a number field shows, and its unit.
    scale: [f64; 2],
    unit: Option<&'static str>,
    axis: Axis,
    /// A next-frame check of a running playhead is pending.
    watching: bool,
    /// The playhead's x as last drawn, in pixels.
    drawn_phase: Option<f32>,
    fields: Option<Fields>,
}

impl EventEmitter<StripChanged> for CurveStrip {}

impl CurveStrip {
    /// `id` names the strip for the agent tree and its fields.
    pub fn new(id: impl Into<SharedString>, value: StripValue) -> Self {
        let value = value.checked();
        Self {
            id: id.into(),
            value,
            bounds: None,
            selected: 0,
            dragging: None,
            before_drag: None,
            error: None,
            presets: Vec::new(),
            presets_menu: MenuVisibility::Closed,
            scale: [0., 1.],
            unit: None,
            axis: Axis::Plain,
            watching: false,
            drawn_phase: None,
            fields: None,
        }
    }

    /// A chip over the strip that offers these values.
    pub fn with_presets(mut self, presets: Vec<(SharedString, StripValue)>) -> Self {
        self.presets = presets;
        self
    }

    /// What a number's 0–1 box means, for the value field: `low` at 0,
    /// `high` at 1.
    pub fn with_scale(mut self, [low, high]: [f64; 2], unit: Option<&'static str>) -> Self {
        self.scale = [low, high];
        self.unit = unit;
        self
    }

    /// x is time: a beat grid and a playhead from `clock`.
    pub fn over_time(mut self, clock: ClockSource) -> Self {
        self.axis = Axis::Time(clock);
        self
    }

    /// x is an axis of the heads: one tick per head from `heads`.
    pub fn across_space(mut self, heads: HeadSource) -> Self {
        self.axis = Axis::Space(heads);
        self
    }

    pub fn value(&self) -> &StripValue {
        &self.value
    }

    /// A host-side write: an undo, another clip, a preset picked elsewhere.
    pub fn set_value(&mut self, value: StripValue, cx: &mut Context<Self>) {
        self.value = value.checked();
        self.selected = self.selected.min(self.value.len().saturating_sub(1));
        self.dragging = None;
        self.before_drag = None;
        self.error = None;
        self.sync_fields(cx);
        cx.notify();
    }

    /// `p` in the drawing area as `[x, y]`, 0–1 with y up, clamped.
    fn position(&self, p: Point<Pixels>) -> Option<[f64; 2]> {
        let b = self.bounds?;
        Some([
            (f32::from(p.x - b.origin.x) / f32::from(b.size.width)).clamp(0., 1.) as f64,
            (1. - f32::from(p.y - b.origin.y) / f32::from(b.size.height)).clamp(0., 1.) as f64,
        ])
    }

    fn distance(&self, a: [f64; 2], b: [f64; 2]) -> f64 {
        let Some(bounds) = self.bounds else {
            return f64::INFINITY;
        };
        let dx = (a[0] - b[0]) * f32::from(bounds.size.width) as f64;
        if self.value.is_color() {
            // A stop spans the fill's height: only x counts.
            return dx.abs();
        }
        dx.hypot((a[1] - b[1]) * f32::from(bounds.size.height) as f64)
    }

    /// The segment whose ease the row and the handles edit.
    fn segment(&self) -> Option<usize> {
        let n = self.value.len();
        (n >= 2 && self.value.eased()).then(|| self.selected.min(n - 2))
    }

    fn hit(&self, p: [f64; 2]) -> Option<Drag> {
        if let (StripValue::Number(curve), Some(segment)) = (&self.value, self.segment()) {
            if curved(curve.ease(segment)) {
                let c = curve.controls(segment);
                for h in [1, 2] {
                    if self.distance(c[h], p) <= REACH {
                        return Some(Drag::Handle(segment, h));
                    }
                }
            }
        }
        (0..self.value.len())
            .filter_map(|i| {
                let d = self.distance(self.value.point(i), p);
                (d <= REACH).then_some((i, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| Drag::Point(i))
    }

    fn move_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(drag), Some(p)) = (self.dragging, self.position(position)) else {
            return;
        };
        let result = match (drag, &mut self.value) {
            (Drag::Point(i), StripValue::Number(curve)) => {
                let n = curve.points.len();
                let x = if i == 0 {
                    0.
                } else if i == n - 1 {
                    1.
                } else {
                    let (a, b) = (curve.points[i - 1].x, curve.points[i + 1].x);
                    let margin = (b - a) * 1e-6;
                    p[0].clamp(a + margin, b - margin)
                };
                curve.move_point(i, [x, p[1]]).map_err(|e| e.to_string())
            }
            (Drag::Handle(i, h), StripValue::Number(curve)) => {
                let mut c = curve.controls(i);
                c[h] = p;
                curve.set_handles(i, c[1], c[2]).map_err(|e| e.to_string())
            }
            (Drag::Point(i), value) => {
                value.move_x(i, p[0]);
                Ok(())
            }
            (Drag::Handle(..), _) => Ok(()),
        };
        self.error = result.err();
        self.sync_fields(cx);
        cx.notify();
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        self.dragging = None;
        if self.before_drag.take().is_some_and(|old| old != self.value) {
            cx.emit(StripChanged(self.value.clone()));
        }
        cx.notify();
    }

    /// An edit that took: the fields follow and the host hears it.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.sync_fields(cx);
        cx.emit(StripChanged(self.value.clone()));
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index.min(self.value.len().saturating_sub(1));
        self.sync_fields(cx);
    }

    fn add(&mut self, x: f64, cx: &mut Context<Self>) -> Option<usize> {
        match self.value.insert(x) {
            Ok(i) => {
                self.select(i, cx);
                self.error = None;
                Some(i)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.value.remove(index) {
            Ok(()) => {
                self.selected = self.selected.min(self.value.len().saturating_sub(1));
                self.changed(cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn set_ease(&mut self, ease: Ease, cx: &mut Context<Self>) {
        let Some(segment) = self.segment() else {
            return;
        };
        if self.value.ease(segment) == Some(ease) {
            return;
        }
        match self.value.set_ease(segment, ease) {
            Ok(()) => self.changed(cx),
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    /// A number's Straight or Curve. A new curve starts straight, with its
    /// handles on the line.
    fn choose_curve(&mut self, curve: bool, cx: &mut Context<Self>) {
        let Some(segment) = self.segment() else {
            return;
        };
        let now = self.value.ease(segment).unwrap_or_default();
        if curved(now) == curve && now != Ease::Hold {
            return;
        }
        self.set_ease(
            if curve {
                Ease::Bezier(Ease::Linear.handles().expect("a straight line has handles"))
            } else {
                Ease::Linear
            },
            cx,
        );
    }

    fn edit_selected(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut StripValue, usize)) {
        if self.selected >= self.value.len() {
            return;
        }
        let before = self.value.clone();
        edit(&mut self.value, self.selected);
        if self.value != before {
            self.changed(cx);
        } else {
            self.sync_fields(cx);
        }
    }

    /// The selected-point row's fields, made once per kind of value.
    fn ensure_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let color = self.value.is_color();
        if self
            .fields
            .as_ref()
            .is_some_and(|fields| matches!(fields.value, ValueField::Color(_)) == color)
        {
            return;
        }
        let id = self.id.clone();
        let position = cx.new(|cx| {
            DraftedNumber::new(
                format!("{id} position"),
                0.,
                0.,
                100.,
                PERCENT_W,
                window,
                cx,
            )
            .with_unit("%")
        });
        let mut subscriptions =
            vec![cx.subscribe(&position, |this, _, event: &NumberEvent, cx| {
                let NumberEvent::Committed(at) = *event;
                this.edit_selected(cx, |value, i| value.move_x(i, at / 100.));
            })];
        let value = if color {
            let alpha = matches!(self.value, StripValue::Gradient(_));
            let editor = cx.new(|cx| {
                let editor =
                    ColorArgEditor::new(format!("{id} point"), ColorArg::decode([1.; 3], 1.), cx)
                        .rgb_only();
                if alpha {
                    editor.with_opacity(1.)
                } else {
                    editor
                }
            });
            subscriptions.push(cx.subscribe(&editor, |this, _, event: &ColorArgEvent, cx| {
                let ColorArgEvent::Changed(color) = *event;
                this.edit_selected(cx, |value, i| {
                    let a = value.color(i).map_or(1., |c| c.a);
                    value.set_color(i, Light { rgb: color.rgb, a });
                });
            }));
            subscriptions.push(cx.subscribe(&editor, |this, _, event: &ColorOpacity, cx| {
                let ColorOpacity(a) = *event;
                this.edit_selected(cx, |value, i| {
                    if let Some(c) = value.color(i) {
                        value.set_color(i, Light { a, ..c });
                    }
                });
            }));
            ValueField::Color(editor)
        } else {
            let [low, high] = self.scale;
            let unit = self.unit;
            let field = cx.new(|cx| {
                let field = DraftedNumber::new(
                    format!("{id} value"),
                    low,
                    low.min(high),
                    low.max(high),
                    VALUE_W,
                    window,
                    cx,
                );
                match unit {
                    Some(unit) => field.with_unit(unit),
                    None => field,
                }
            });
            subscriptions.push(cx.subscribe(&field, |this, _, event: &NumberEvent, cx| {
                let NumberEvent::Committed(n) = *event;
                let [low, high] = this.scale;
                let y = if high == low {
                    0.
                } else {
                    ((n - low) / (high - low)).clamp(0., 1.)
                };
                this.edit_selected(cx, |value, i| {
                    if let StripValue::Number(curve) = value {
                        let x = curve.points[i].x;
                        let _ = curve.move_point(i, [x, y]);
                    }
                });
            }));
            ValueField::Number(field)
        };
        self.fields = Some(Fields {
            position,
            value,
            _subscriptions: subscriptions,
        });
        self.sync_fields(cx);
    }

    /// Show the selected point in the row's fields.
    fn sync_fields(&self, cx: &mut Context<Self>) {
        let Some(fields) = &self.fields else {
            return;
        };
        if self.selected >= self.value.len() {
            return;
        }
        let [x, y] = self.value.point(self.selected);
        let percent = (x * 1000.).round() / 10.;
        fields
            .position
            .update(cx, |field, cx| field.set_value(percent, cx));
        match &fields.value {
            ValueField::Number(field) => {
                let [low, high] = self.scale;
                let n = low + y * (high - low);
                field.update(cx, |field, cx| field.set_value(n, cx));
            }
            ValueField::Color(editor) => {
                let Some(c) = self.value.color(self.selected) else {
                    return;
                };
                let alpha = matches!(self.value, StripValue::Gradient(_));
                editor.update(cx, |editor, cx| {
                    editor.set_value(ColorArg::decode(c.rgb, 1.), cx);
                    if alpha {
                        editor.set_opacity(c.a, cx);
                    }
                });
            }
        }
    }

    /// While the transport runs, check the playhead each frame and redraw
    /// only when it has moved half a pixel. The strip's own notify is the
    /// only one playback makes here.
    fn watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.watching {
            return;
        }
        self.watching = true;
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |strip, cx| {
                strip.watching = false;
                let Axis::Time(clock) = &strip.axis else {
                    return;
                };
                let Some(reading) = clock(cx) else {
                    if strip.drawn_phase.is_some() {
                        cx.notify();
                    }
                    return;
                };
                let width = strip.bounds.map_or(0., |b| f32::from(b.size.width));
                let now = reading.phase.map(|phase| phase as f32 * width);
                let moved = match (now, strip.drawn_phase) {
                    (Some(a), Some(b)) => (a - b).abs() >= 0.5,
                    (a, b) => a.is_some() != b.is_some(),
                };
                if moved {
                    cx.notify();
                } else if reading.playing {
                    strip.watch(window, cx);
                }
            });
        });
    }

    fn preset_chip(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.presets.is_empty() {
            return None;
        }
        if self
            .presets_menu
            .tick_close(crate::motion::reduced_motion(cx))
        {
            window.request_animation_frame();
        }
        let current = self
            .presets
            .iter()
            .position(|(_, preset)| preset.close(&self.value));
        let options: Vec<(SharedString, Thumb)> = self
            .presets
            .iter()
            .map(|(name, value)| (name.clone(), value.thumb()))
            .collect();
        let toggle = cx.entity();
        let pick = cx.entity();
        Some(luma_preset_picker(
            format!("{}:presets", self.id),
            &self.value.thumb(),
            current,
            &options,
            false,
            self.presets_menu,
            move |_, cx| {
                toggle.update(cx, |this, cx| {
                    this.presets_menu.toggle();
                    cx.notify();
                })
            },
            move |picked, _, cx| {
                pick.update(cx, |this, cx| {
                    this.presets_menu.close();
                    if let Some((_, value)) = picked.and_then(|at| this.presets.get(at)) {
                        this.value = value.clone();
                        this.selected = 0;
                        this.changed(cx);
                    }
                    cx.notify();
                })
            },
        ))
    }
}

impl Render for CurveStrip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_fields(window, cx);
        let chip = self.preset_chip(window, cx);
        let id = self.id.clone();
        let value = self.value.clone();
        let color = value.is_color();
        let segment = self.segment();
        let this = cx.entity();
        let drag_view = cx.entity();
        let dragging = self.dragging.is_some();
        let pressed = self.before_drag.is_some();
        let clock = match &self.axis {
            Axis::Time(clock) => clock(cx),
            _ => None,
        };
        if clock.is_some_and(|clock| clock.playing) {
            self.watch(window, cx);
        }
        let width = self.bounds.map_or(0., |b| f32::from(b.size.width));
        self.drawn_phase = clock
            .and_then(|clock| clock.phase)
            .map(|phase| phase as f32 * width);
        let heads = match &self.axis {
            Axis::Space(heads) => heads(cx),
            _ => None,
        };
        let handles = match (&value, segment) {
            (StripValue::Number(curve), Some(segment)) if curved(curve.ease(segment)) => {
                let c = curve.controls(segment);
                vec![(segment, 1, c[1]), (segment, 2, c[2])]
            }
            _ => Vec::new(),
        };
        let beats = clock.map(|clock| clock.beats);
        let painted = value.clone();
        let area = div()
            .relative()
            .w_full()
            .h(px(if color { COLOR_H } else { NUMBER_H }))
            .flex_none()
            .when(color, |area| {
                area.child(
                    div()
                        .absolute()
                        .size_full()
                        .flex()
                        .rounded(px(crate::radius::CAP))
                        .border_1()
                        .border_color(crate::glass::hairline(0.10))
                        .bg(gpui::black())
                        .overflow_hidden()
                        .children(value.fill()),
                )
            })
            .child(
                canvas(
                    move |bounds, _, cx| {
                        this.update(cx, |this, _| this.bounds = Some(bounds));
                    },
                    move |bounds, _, window, _| {
                        // A drag follows the pointer anywhere in the window,
                        // clamped to the box, and ends on the first release
                        // anywhere.
                        if dragging {
                            let moving = drag_view.clone();
                            window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, _, cx| {
                                if phase.bubble() {
                                    moving.update(cx, |this, cx| this.move_drag(e.position, cx));
                                }
                            });
                        }
                        if pressed {
                            let release = drag_view.clone();
                            window.on_mouse_event(move |e: &gpui::MouseUpEvent, phase, _, cx| {
                                if phase.bubble() && e.button == MouseButton::Left {
                                    release.update(cx, |this, cx| this.release(cx));
                                }
                            });
                        }
                        if let Some(beats) = beats {
                            paint_grid(window, bounds, beats, color);
                        }
                        if let StripValue::Number(curve) = &painted {
                            paint_envelope(window, bounds, curve, px(1.5), ladder::foreground());
                            if let Some(segment) =
                                segment.filter(|segment| curved(curve.ease(*segment)))
                            {
                                let at = |p| at(bounds, p);
                                let c = curve.controls(segment);
                                let mut lines = PathBuilder::stroke(px(1.));
                                lines.move_to(at(c[0]));
                                lines.line_to(at(c[1]));
                                lines.move_to(at(c[3]));
                                lines.line_to(at(c[2]));
                                if let Ok(path) = lines.build() {
                                    window.paint_path(path, ladder::primary().opacity(0.6));
                                }
                            }
                        }
                    },
                )
                .size_full(),
            )
            .children((0..value.len()).map(|i| {
                let p = value.point(i);
                let chosen = i == self.selected;
                let marker = if color {
                    let c = value.color(i).unwrap_or(Light::WHITE);
                    div()
                        .absolute()
                        .left(gpui::relative(p[0] as f32))
                        .top(gpui::relative(0.5))
                        .ml(px(-STOP / 2.))
                        .mt(px(-STOP / 2.))
                        .size(px(STOP))
                        .rounded(px(3.))
                        .bg(Light { a: 1., ..c }.display())
                        .border_color(if chosen {
                            ladder::foreground()
                        } else {
                            ladder::control_border()
                        })
                        .when(chosen, |marker| marker.border_2())
                        .when(!chosen, |marker| marker.border_1())
                        .cursor_ew_resize()
                } else {
                    div()
                        .absolute()
                        .left(gpui::relative(p[0] as f32))
                        .top(gpui::relative((1. - p[1]) as f32))
                        .ml(px(-4.))
                        .mt(px(-4.))
                        .size(px(8.))
                        .rounded_full()
                        .border_1()
                        .border_color(if chosen {
                            ladder::foreground().into()
                        } else {
                            crate::glass::hairline(0.24)
                        })
                        .bg(ladder::foreground())
                };
                marker
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.before_drag = Some(this.value.clone());
                            this.dragging = Some(Drag::Point(i));
                            this.select(i, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .agent_node(Role::Slider, format!("{id} point {}", i + 1))
            }))
            .children(handles.into_iter().map(|(segment, h, p)| {
                div()
                    .absolute()
                    .left(gpui::relative(p[0] as f32))
                    .top(gpui::relative((1. - p[1]) as f32))
                    .ml(px(-4.))
                    .mt(px(-4.))
                    .size(px(8.))
                    .rounded_full()
                    .border_1()
                    .border_color(crate::glass::hairline(0.24))
                    .bg(ladder::primary())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.before_drag = Some(this.value.clone());
                            this.dragging = Some(Drag::Handle(segment, h));
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .agent_node(
                        Role::Slider,
                        format!("{id} segment {} handle {h}", segment + 1),
                    )
            }))
            .children(beats.map(|beats| {
                div().absolute().size_0().agent_node(
                    Role::Text,
                    format!("{id} beat grid = {}", format_value(beats)),
                )
            }))
            .children(clock.and_then(|clock| clock.phase).map(|phase| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(gpui::relative(phase as f32))
                    .ml(px(-0.75))
                    .w(px(1.5))
                    .bg(ladder::accent())
                    .agent_node(Role::Text, format!("{id} playhead"))
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                    let Some(p) = this.position(e.position) else {
                        return;
                    };
                    this.before_drag = Some(this.value.clone());
                    if let Some(hit) = this.hit(p) {
                        this.dragging = Some(hit);
                        if let Drag::Point(i) = hit {
                            this.select(i, cx);
                        }
                    } else if e.click_count == 2 {
                        if let Some(i) = this.add(p[0], cx) {
                            this.dragging = Some(Drag::Point(i));
                        }
                    } else {
                        let at = this.value.segment_at(p[0]);
                        this.select(at, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                    let Some(p) = this.position(e.position) else {
                        return;
                    };
                    if let Some(Drag::Point(i)) = this.hit(p) {
                        this.remove(i, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .agent_node(Role::Card, format!("{id} strip"));
        let ticks = heads.map(|heads| {
            div()
                .relative()
                .w_full()
                .h(px(TICK_H))
                .children(heads.iter().enumerate().map(|(i, &x)| {
                    let c = value.head_color(x);
                    div()
                        .absolute()
                        .left(gpui::relative(x.clamp(0., 1.) as f32))
                        .ml(px(-2.))
                        .w(px(4.))
                        .h_full()
                        .rounded(px(1.))
                        .border_1()
                        .border_color(crate::glass::hairline(0.24))
                        .bg(c.display())
                        .agent_node(Role::Text, format!("{id} head {}", i + 1))
                }))
        });
        let strip = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(INSET))
            .rounded(px(crate::radius::ROW))
            .border_1()
            .border_color(crate::glass::hairline(0.08))
            .bg(crate::glass::ink(0.03))
            .child(area)
            .children(ticks);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(chip)
            .child(strip)
            .child(self.point_row(cx))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(ladder::foreground_alpha(0.4))
                    .child(if self.value.len() == 0 {
                        "No colors · double-click adds one"
                    } else {
                        "Double-click adds a point · right-click removes"
                    }),
            )
            .children(self.error.as_ref().map(|error| {
                div()
                    .text_size(px(11.))
                    .text_color(ladder::foreground_alpha(0.6))
                    .child(error.clone())
            }))
    }
}

impl CurveStrip {
    /// The selected point's position, value and, where the value has eases,
    /// the ease of the segment from it.
    fn point_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let row = div()
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(8.));
        let Some(fields) = self.fields.as_ref().filter(|_| self.value.len() > 0) else {
            return row;
        };
        let value = match &fields.value {
            ValueField::Number(field) => labelled("Value", field.clone()).into_any_element(),
            ValueField::Color(editor) => editor.clone().into_any_element(),
        };
        let ease = self.segment().map(|segment| {
            let now = self.value.ease(segment).unwrap_or_default();
            let options: Vec<(&'static str, Ease, bool)> = match &self.value {
                StripValue::Number(_) => vec![
                    ("Straight", Ease::Linear, !curved(now)),
                    (
                        "Curve",
                        Ease::Bezier(Ease::Linear.handles().expect("handles")),
                        curved(now),
                    ),
                ],
                _ => vec![
                    ("Straight", Ease::Linear, now == Ease::Linear),
                    ("Smooth", Ease::EaseInOut, curved(now)),
                    ("Hold", Ease::Hold, now == Ease::Hold),
                ],
            };
            let number = matches!(self.value, StripValue::Number(_));
            crate::float::segmented().children(options.into_iter().map(|(label, ease, chosen)| {
                crate::float::segment(label, chosen, format!("{}-{label}", self.id))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if number {
                                this.choose_curve(label == "Curve", cx);
                            } else {
                                this.set_ease(ease, cx);
                            }
                        }),
                    )
                    .agent_node(Role::Button, format!("{} {label}", self.id))
            }))
        });
        row.child(labelled("Position", fields.position.clone()))
            .child(value)
            .children(ease)
    }
}

/// A field with a dim name before it, kept on one line.
fn labelled(name: &'static str, field: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(11.))
                .text_color(ladder::foreground_alpha(0.5))
                .child(name),
        )
        .child(field)
}

/// A line per beat over `beats`, a stronger one per bar of four.
pub(crate) fn paint_grid(window: &mut Window, bounds: Bounds<Pixels>, beats: f64, over_color: bool) {
    if !(beats.is_finite() && beats > 0.) {
        return;
    }
    let step = if beats <= MAX_LINES {
        1
    } else if beats / 4. <= MAX_LINES {
        4
    } else {
        return;
    };
    let alpha = |bar: bool| match (bar, over_color) {
        (true, true) => 0.45,
        (false, true) => 0.2,
        (true, false) => 0.16,
        (false, false) => 0.06,
    };
    let mut beat = step;
    while (beat as f64) < beats {
        let bar = beat % 4 == 0;
        let x = bounds.origin.x + bounds.size.width * (beat as f64 / beats) as f32;
        let line = Bounds {
            origin: point(x - px(0.5), bounds.origin.y),
            size: size(px(1.), bounds.size.height),
        };
        window.paint_quad(fill(line, ladder::foreground_alpha(alpha(bar))));
        beat += step;
    }
}

/// `p`, in the 0–1 box with y up, as a point in `bounds`.
fn at(bounds: Bounds<Pixels>, p: [f64; 2]) -> Point<Pixels> {
    point(
        bounds.origin.x + bounds.size.width * p[0] as f32,
        bounds.origin.y + bounds.size.height * (1. - p[1]) as f32,
    )
}

/// Whether `ease` is drawn with handles: every ease but a straight line and
/// a hold.
fn curved(ease: Ease) -> bool {
    !matches!(ease, Ease::Linear | Ease::Hold)
}

/// Stroke `value`'s line across `bounds`. The strip and the curve picker's
/// thumbnails draw the same line.
pub(crate) fn paint_envelope(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    value: &Envelope,
    width: Pixels,
    color: impl Into<Background>,
) {
    let at = |p| at(bounds, p);
    let mut path = PathBuilder::stroke(width);
    path.move_to(at(value.point(0)));
    for i in 0..value.points.len() - 1 {
        let c = value.controls(i);
        match value.ease(i) {
            Ease::Linear => path.line_to(at(c[3])),
            Ease::Hold => {
                path.line_to(at([c[3][0], c[0][1]]));
                path.line_to(at(c[3]));
            }
            _ => path.cubic_bezier_to(at(c[3]), at(c[1]), at(c[2])),
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn key_color(key: &Key) -> [f64; 3] {
    match *key {
        Key::Color(rgb) => rgb,
        Key::Number(v) => [v; 3],
    }
}

impl StripValue {
    /// The value as the strip keeps it: a number or color curve that does not
    /// validate becomes a flat one, so every edit starts from a valid value.
    fn checked(self) -> Self {
        match self {
            Self::Number(curve) if curve.validate().is_err() => {
                Self::Number(Envelope::linear(vec![[0., 0.], [1., 0.]]))
            }
            Self::Colors(keys) if keys.validate().is_err() || !keys.is_color() => Self::Colors(
                Keyframes::with_eases([(0., Key::Color([1.; 3])), (1., Key::Color([1.; 3]))], &[]),
            ),
            value => value,
        }
    }

    pub fn is_color(&self) -> bool {
        !matches!(self, Self::Number(_))
    }

    /// Whether each segment has an ease.
    fn eased(&self) -> bool {
        !matches!(self, Self::Gradient(_))
    }

    fn len(&self) -> usize {
        match self {
            Self::Number(curve) => curve.points.len(),
            Self::Gradient(gradient) => gradient.stops().len(),
            Self::Colors(keys) => keys.points.len(),
        }
    }

    /// Point `i` as `[x, y]`; a color's y is the middle of its fill.
    fn point(&self, i: usize) -> [f64; 2] {
        match self {
            Self::Number(curve) => curve.point(i),
            Self::Gradient(gradient) => [f64::from(gradient.stops()[i].t), 0.5],
            Self::Colors(keys) => [keys.points[i].x, 0.5],
        }
    }

    /// The point that starts the segment under `x`.
    fn segment_at(&self, x: f64) -> usize {
        (0..self.len())
            .take_while(|i| self.point(*i)[0] < x)
            .last()
            .unwrap_or(0)
    }

    fn ease(&self, segment: usize) -> Option<Ease> {
        match self {
            Self::Number(curve) => Some(curve.ease(segment)),
            Self::Colors(keys) => Some(keys.ease(segment)),
            Self::Gradient(_) => None,
        }
    }

    fn set_ease(&mut self, segment: usize, ease: Ease) -> Result<(), String> {
        match self {
            Self::Number(curve) => curve.set_ease(segment, ease).map_err(|e| e.to_string()),
            Self::Colors(keys) if segment + 1 < keys.points.len() => {
                keys.points[segment].ease = ease;
                Ok(())
            }
            _ => Err("this value has no eases".into()),
        }
    }

    /// Move point `i` to `x`, between its neighbours. The ends of a curve
    /// stay where they are; a gradient's stops may meet.
    fn move_x(&mut self, i: usize, x: f64) {
        match self {
            Self::Number(curve) => {
                let n = curve.points.len();
                if i == 0 || i + 1 >= n {
                    return;
                }
                let (a, b) = (curve.points[i - 1].x, curve.points[i + 1].x);
                let margin = (b - a) * 1e-6;
                let y = curve.points[i].value;
                let _ = curve.move_point(i, [x.clamp(a + margin, b - margin), y]);
            }
            Self::Gradient(gradient) => {
                if i < gradient.stops().len() {
                    gradient.move_stop(i, x as f32);
                }
            }
            Self::Colors(keys) => {
                let n = keys.points.len();
                if i == 0 || i + 1 >= n {
                    return;
                }
                let (a, b) = (keys.points[i - 1].x, keys.points[i + 1].x);
                let margin = (b - a) * 1e-6;
                keys.points[i].x = x.clamp(a + margin, b - margin);
            }
        }
    }

    /// Add a point at `x` that changes nothing until it is moved: on the
    /// curve, or in the color there. Returns its index.
    fn insert(&mut self, x: f64) -> Result<usize, String> {
        let most = match self {
            Self::Gradient(_) => MAX_STOPS,
            _ => MAX_POINTS,
        };
        if self.len() >= most {
            return Err(format!("this value holds at most {most} points"));
        }
        match self {
            Self::Number(curve) => curve.insert_point(x).map_err(|e| e.to_string()),
            Self::Gradient(gradient) => Ok(gradient.insert(x as f32)),
            Self::Colors(keys) => {
                if !(x > 0. && x < 1.) || keys.points.iter().any(|p| (p.x - x).abs() < 1e-9) {
                    return Err("a new point must lie inside a segment".into());
                }
                let i = keys.points.partition_point(|p| p.x < x) - 1;
                let ease = keys.ease(i);
                let color = keys.sample(x);
                keys.points
                    .insert(i + 1, CurvePoint::eased(x, Key::Color(color), ease));
                Ok(i + 1)
            }
        }
    }

    fn remove(&mut self, i: usize) -> Result<(), String> {
        match self {
            Self::Number(curve) => curve.remove_point(i).map_err(|e| e.to_string()),
            Self::Gradient(gradient) => {
                gradient.remove(i);
                Ok(())
            }
            Self::Colors(keys) => {
                if i == 0 || i + 1 >= keys.points.len() {
                    return Err("the end points cannot be removed".into());
                }
                keys.points.remove(i);
                Ok(())
            }
        }
    }

    /// A color point's color, with its opacity.
    fn color(&self, i: usize) -> Option<Light> {
        match self {
            Self::Number(_) => None,
            Self::Gradient(gradient) => gradient.stops().get(i).map(|stop| stop.color),
            Self::Colors(keys) => keys
                .points
                .get(i)
                .map(|p| Light::opaque(key_color(&p.value))),
        }
    }

    fn set_color(&mut self, i: usize, color: Light) {
        match self {
            Self::Number(_) => {}
            Self::Gradient(gradient) => gradient.set_color(i, color),
            Self::Colors(keys) => {
                keys.points[i].value = Key::Color(color.channels().map(|v| v.clamp(0., 1.)));
            }
        }
    }

    /// What a head at `x` gets: the color there, or white at the number's
    /// level.
    fn head_color(&self, x: f64) -> Light {
        match self {
            Self::Number(curve) => Light::opaque([curve.sample(x).clamp(0., 1.); 3]),
            Self::Gradient(gradient) => gradient.color_at(x as f32),
            Self::Colors(keys) => Light::opaque(keys.sample(x)),
        }
    }

    /// A color's fill, laid in a flex row the height of the fill. A
    /// gradient is exact in OKLab; keyframes blend in linear RGB, so each
    /// eased segment is cut in eight pieces and a hold is flat. Every color
    /// is shown mapped into sRGB.
    fn fill(&self) -> Vec<gpui::Div> {
        let radius = crate::radius::CAP;
        match self {
            Self::Number(_) => Vec::new(),
            Self::Gradient(gradient) => gradient_fill(gradient, radius),
            Self::Colors(keys) => {
                const PIECES: usize = 8;
                let mut pieces = Vec::new();
                for (i, pair) in keys.points.windows(2).enumerate() {
                    let (a, b) = (key_color(&pair[0].value), key_color(&pair[1].value));
                    let span = pair[1].x - pair[0].x;
                    let ease = keys.ease(i);
                    let mix = |t: f64| {
                        let share = ease.apply(t);
                        Light::opaque(std::array::from_fn(|ch| a[ch] + (b[ch] - a[ch]) * share))
                            .display()
                    };
                    if ease == Ease::Hold {
                        pieces.push(
                            div()
                                .h_full()
                                .w(gpui::relative(span as f32))
                                .bg(Light::opaque(a).display()),
                        );
                        continue;
                    }
                    for k in 0..PIECES {
                        let (t0, t1) = (k as f64 / PIECES as f64, (k + 1) as f64 / PIECES as f64);
                        pieces.push(
                            div()
                                .h_full()
                                .w(gpui::relative((span / PIECES as f64) as f32))
                                .bg(linear_gradient(
                                    90.,
                                    linear_color_stop(mix(t0), 0.),
                                    linear_color_stop(mix(t1), 1.),
                                )),
                        );
                    }
                }
                pieces
            }
        }
    }

    /// The picture a preset chip shows.
    fn thumb(&self) -> Thumb {
        match self {
            Self::Number(curve) => Thumb::Curve(curve.clone()),
            Self::Gradient(gradient) => Thumb::Gradient(gradient.clone()),
            Self::Colors(keys) => {
                Thumb::Gradient(Gradient::new(keys.points.iter().map(|p| GradientStop {
                    t: p.x as f32,
                    color: Light::opaque(key_color(&p.value)),
                })))
            }
        }
    }

    /// The same value, to within half an 8-bit step.
    fn close(&self, other: &Self) -> bool {
        let near = |a: f64, b: f64| (a - b).abs() <= 0.5 / 255. + 1e-4;
        let same_ease = |a: Ease, b: Ease| match (a, b) {
            (Ease::Bezier(a), Ease::Bezier(b)) => a.iter().zip(&b).all(|(a, b)| near(*a, *b)),
            (a, b) => a == b,
        };
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => {
                a.points.len() == b.points.len()
                    && a.points.iter().zip(&b.points).all(|(a, b)| {
                        near(a.x, b.x) && near(a.value, b.value) && same_ease(a.ease, b.ease)
                    })
            }
            (Self::Gradient(a), Self::Gradient(b)) => {
                a.stops().len() == b.stops().len()
                    && a.stops().iter().zip(b.stops()).all(|(a, b)| {
                        near(a.t.into(), b.t.into())
                            && a.color
                                .rgb
                                .iter()
                                .chain([&a.color.a])
                                .zip(b.color.rgb.iter().chain([&b.color.a]))
                                .all(|(a, b)| near((*a).into(), (*b).into()))
                    })
            }
            (Self::Colors(a), Self::Colors(b)) => {
                a.points.len() == b.points.len()
                    && a.points.iter().zip(&b.points).all(|(a, b)| {
                        let (ca, cb) = (key_color(&a.value), key_color(&b.value));
                        near(a.x, b.x)
                            && same_ease(a.ease, b.ease)
                            && ca.iter().zip(&cb).all(|(a, b)| near(*a, *b))
                    })
            }
            _ => false,
        }
    }
}

/// The shipped gradients, as strip values.
pub fn gradient_presets() -> Vec<(SharedString, StripValue)> {
    luma_patterns::presets()
        .gradients
        .iter()
        .map(|preset| {
            let stops = preset.gradient.stops.iter().map(|stop| GradientStop {
                t: stop.t as f32,
                color: Light {
                    a: stop.alpha as f32,
                    ..Light::opaque(stop.color)
                },
            });
            (
                preset.name.clone().into(),
                StripValue::Gradient(Gradient::new(stops)),
            )
        })
        .collect()
}

/// The shipped gradients as color keyframes: each stop a point, the end
/// colors held out to the ends.
pub fn color_curve_presets() -> Vec<(SharedString, StripValue)> {
    gradient_presets()
        .into_iter()
        .filter_map(|(name, value)| match value {
            StripValue::Gradient(gradient) => {
                Some((name, StripValue::Colors(color_keys(&gradient))))
            }
            _ => None,
        })
        .collect()
}

/// Gradient stops as color keyframes from 0 to 1, linear between. The end
/// colors hold out to the ends, and of two stops at one place the first is
/// kept.
pub fn color_keys(gradient: &Gradient) -> Keyframes {
    let mut points: Vec<(f64, Key)> = Vec::new();
    for stop in gradient.stops() {
        let t = f64::from(stop.t).clamp(0., 1.);
        let color = Key::Color(stop.color.channels());
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
    if points.len() < 2 {
        let color = points.first().map_or(Key::Color([1.; 3]), |(_, c)| *c);
        points = vec![(0., color), (1., color)];
    }
    Keyframes::with_eases(points, &[])
}

/// The editor's own curves, for a host with no curve picker of its own.
pub fn envelope_presets() -> Vec<(SharedString, StripValue)> {
    [
        ("Hard", Envelope::soft_edges(0.)),
        ("Soft", soft_preset()),
        ("Triangle", Envelope::soft_edges(1.)),
        ("Ramp up", Envelope::linear(vec![[0., 0.], [1., 1.]])),
        ("Ramp down", Envelope::linear(vec![[0., 1.], [1., 0.]])),
    ]
    .into_iter()
    .map(|(name, curve)| (name.into(), StripValue::Number(curve)))
    .collect()
}

fn soft_preset() -> Envelope {
    Envelope::eased(
        vec![[0., 0.], [0.2, 1.], [0.8, 1.], [1., 0.]],
        &[
            Ease::Bezier([0.35, 0., 0.65, 1.]),
            Ease::Linear,
            Ease::Bezier([0.35, 0., 0.65, 1.]),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> StripValue {
        StripValue::Colors(Keyframes::with_eases(
            [
                (0., Key::Color([1., 0., 0.])),
                (1., Key::Color([0., 0., 1.])),
            ],
            &[Ease::EaseIn],
        ))
    }

    #[test]
    fn a_preset_is_found_again() {
        for presets in [
            gradient_presets(),
            color_curve_presets(),
            envelope_presets(),
        ] {
            assert!(!presets.is_empty());
            for (at, (_, value)) in presets.iter().enumerate() {
                assert_eq!(presets.iter().position(|(_, p)| p.close(value)), Some(at));
            }
        }
        let StripValue::Gradient(mut edited) = gradient_presets()[0].1.clone() else {
            unreachable!()
        };
        edited.move_stop(1, 0.3);
        let edited = StripValue::Gradient(edited);
        assert!(!gradient_presets().iter().any(|(_, p)| p.close(&edited)));
    }

    /// Color stops move in x only, and a curve's ends stay put.
    #[test]
    fn color_points_move_in_x_between_their_neighbours() {
        let mut value = colors();
        let i = value.insert(0.5).unwrap();
        assert_eq!(i, 1);
        // The new point keeps the segment's ease and changes no color.
        assert_eq!(value.ease(1), Some(Ease::EaseIn));
        value.move_x(1, 2.);
        let x = value.point(1)[0];
        assert!(x < 1. && x > 0.99, "{x}");
        value.move_x(0, 0.5);
        assert_eq!(value.point(0), [0., 0.5]);
        assert!(value.remove(0).is_err());
        value.remove(1).unwrap();
        assert_eq!(value.len(), 2);
        let StripValue::Colors(keys) = &value else {
            unreachable!()
        };
        keys.validate().unwrap();
    }

    #[test]
    fn a_new_color_point_is_the_color_there() {
        let mut value = colors();
        let before = value.head_color(0.3);
        let i = value.insert(0.3).unwrap();
        let after = value.color(i).unwrap();
        assert!(
            (before.rgb[0] - after.rgb[0]).abs() < 1e-6
                && (before.rgb[2] - after.rgb[2]).abs() < 1e-6
        );
    }

    #[test]
    fn gradient_stops_become_keyframes_from_end_to_end() {
        let gradient = Gradient::new([
            GradientStop {
                t: 0.25,
                color: Light::BLACK,
            },
            GradientStop {
                t: 0.75,
                color: Light::WHITE,
            },
        ]);
        let keys = color_keys(&gradient);
        keys.validate().unwrap();
        assert_eq!(keys.points.len(), 4);
        assert_eq!(keys.sample(0.), [0.; 3]);
        assert_eq!(keys.sample(1.), [1.; 3]);
    }
}
