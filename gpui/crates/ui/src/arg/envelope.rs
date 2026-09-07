//! The shared envelope value, edited with anchors and real Bézier handles.
use crate::{
    ladder,
    node::{Instrument, Role},
};
use gpui::prelude::*;
use gpui::{
    canvas, div, point, px, Bounds, Context, EventEmitter, MouseButton, PathBuilder, Pixels, Point,
    Window,
};
use luma_patterns::{Envelope, EnvelopeCurve};

#[derive(Clone, Debug)]
pub struct EnvelopeChanged(pub Envelope);
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Anchor(usize),
    Handle(usize, usize),
}
pub struct EnvelopeEditor {
    value: Envelope,
    bounds: Option<Bounds<Pixels>>,
    selected: usize,
    dragging: Option<Drag>,
    before_drag: Option<Envelope>,
    error: Option<String>,
}
impl EventEmitter<EnvelopeChanged> for EnvelopeEditor {}
impl EnvelopeEditor {
    pub fn new(value: Envelope) -> Self {
        assert!(
            value.validate().is_ok(),
            "EnvelopeEditor requires a validated envelope"
        );
        Self {
            value,
            bounds: None,
            selected: 0,
            dragging: None,
            before_drag: None,
            error: None,
        }
    }
    pub fn set_value(&mut self, value: Envelope, cx: &mut Context<Self>) {
        assert!(
            value.validate().is_ok(),
            "EnvelopeEditor requires a validated envelope"
        );
        self.value = value;
        self.selected = self.selected.min(self.value.points.len() - 2);
        self.dragging = None;
        self.before_drag = None;
        self.error = None;
        cx.notify();
    }
    fn position(&self, p: Point<Pixels>) -> Option<[f64; 2]> {
        let b = self.bounds?;
        Some([
            (f32::from(p.x - b.origin.x) / f32::from(b.size.width)).clamp(0., 1.) as f64,
            (1. - f32::from(p.y - b.origin.y) / f32::from(b.size.height)).clamp(0., 1.) as f64,
        ])
    }
    fn distance(&self, a: [f64; 2], b: [f64; 2]) -> f64 {
        let bounds = self.bounds.unwrap();
        ((a[0] - b[0]) * f32::from(bounds.size.width) as f64)
            .hypot((a[1] - b[1]) * f32::from(bounds.size.height) as f64)
    }
    fn hit(&self, p: [f64; 2]) -> Option<Drag> {
        if matches!(
            self.value.curve(self.selected),
            EnvelopeCurve::Bezier { .. }
        ) {
            let c = self.value.controls(self.selected);
            for h in [1, 2] {
                if self.distance(c[h], p) <= 9. {
                    return Some(Drag::Handle(self.selected, h));
                }
            }
        }
        self.value
            .points
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                let d = self.distance(*a, p);
                (d <= 9.).then_some((i, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| Drag::Anchor(i))
    }
    fn move_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(drag), Some(mut p)) = (self.dragging, self.position(position)) else {
            return;
        };
        let result = match drag {
            Drag::Anchor(i) => {
                p[0] = if i == 0 {
                    0.
                } else if i == self.value.points.len() - 1 {
                    1.
                } else {
                    let (a, b) = (self.value.points[i - 1][0], self.value.points[i + 1][0]);
                    let margin = (b - a) * 1e-6;
                    p[0].clamp(a + margin, b - margin)
                };
                self.value.move_point(i, p)
            }
            Drag::Handle(i, h) => {
                let mut c = self.value.controls(i);
                p[0] = if h == 1 {
                    p[0].clamp(c[0][0], c[2][0])
                } else {
                    p[0].clamp(c[1][0], c[3][0])
                };
                c[h] = p;
                self.value.set_curve(
                    i,
                    EnvelopeCurve::Bezier {
                        control1: c[1],
                        control2: c[2],
                    },
                )
            }
        };
        self.error = result.err().map(|e| e.to_string());
        cx.notify();
    }
    fn commit(&mut self, cx: &mut Context<Self>) {
        self.dragging = None;
        if self.before_drag.take().is_some_and(|old| old != self.value) {
            cx.emit(EnvelopeChanged(self.value.clone()));
        }
        cx.notify();
    }
    fn choose_curve(&mut self, curved: bool, cx: &mut Context<Self>) {
        let c = self.value.controls(self.selected);
        let curve = if curved {
            EnvelopeCurve::Bezier {
                control1: c[1],
                control2: c[2],
            }
        } else {
            EnvelopeCurve::Linear
        };
        match self.value.set_curve(self.selected, curve) {
            Ok(()) => {
                self.error = None;
                cx.emit(EnvelopeChanged(self.value.clone()));
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        cx.notify();
    }
}
impl Render for EnvelopeEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value = self.value.clone();
        let selected = self.selected;
        let this = cx.entity();
        let curved = matches!(self.value.curve(selected), EnvelopeCurve::Bezier { .. });
        let mut handles = self
            .value
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| (Drag::Anchor(i), *p))
            .collect::<Vec<_>>();
        if curved {
            let c = self.value.controls(selected);
            handles.extend([
                (Drag::Handle(selected, 1), c[1]),
                (Drag::Handle(selected, 2), c[2]),
            ]);
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .w_full()
                    .p(px(9.))
                    .border_1()
                    .border_color(ladder::foreground().opacity(0.3))
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(120.))
                            .flex_none()
                            .child(
                                canvas(
                                    move |bounds, _, cx| {
                                        this.update(cx, |this, _| this.bounds = Some(bounds));
                                    },
                                    move |bounds, _, window, _| {
                                        let at = |p: [f64; 2]| {
                                            point(
                                                bounds.origin.x + bounds.size.width * p[0] as f32,
                                                bounds.origin.y
                                                    + bounds.size.height * (1. - p[1]) as f32,
                                            )
                                        };
                                        let mut path = PathBuilder::stroke(px(1.5));
                                        path.move_to(at(value.points[0]));
                                        for i in 0..value.points.len() - 1 {
                                            let c = value.controls(i);
                                            match value.curve(i) {
                                                EnvelopeCurve::Linear => path.line_to(at(c[3])),
                                                EnvelopeCurve::Bezier { .. } => path
                                                    .cubic_bezier_to(at(c[3]), at(c[1]), at(c[2])),
                                            }
                                        }
                                        if let Ok(path) = path.build() {
                                            window.paint_path(path, ladder::foreground());
                                        }
                                        if curved {
                                            let c = value.controls(selected);
                                            let mut lines = PathBuilder::stroke(px(1.));
                                            lines.move_to(at(c[0]));
                                            lines.line_to(at(c[1]));
                                            lines.move_to(at(c[3]));
                                            lines.line_to(at(c[2]));
                                            if let Ok(path) = lines.build() {
                                                window.paint_path(
                                                    path,
                                                    ladder::primary().opacity(0.6),
                                                );
                                            }
                                        }
                                    },
                                )
                                .size_full(),
                            )
                            .children(handles.into_iter().map(|(handle, p)| {
                                let (role, label) = match handle {
                                    Drag::Anchor(i) => {
                                        (Role::Slider, format!("Envelope anchor {}", i + 1))
                                    }
                                    Drag::Handle(i, h) => (
                                        Role::Slider,
                                        format!("Envelope segment {} handle {h}", i + 1),
                                    ),
                                };
                                div()
                                    .absolute()
                                    .left(gpui::relative(p[0] as f32))
                                    .top(gpui::relative((1. - p[1]) as f32))
                                    .ml(px(-4.))
                                    .mt(px(-4.))
                                    .size(px(8.))
                                    .border_1()
                                    .border_color(ladder::control_border())
                                    .bg(if matches!(handle, Drag::Handle(..)) {
                                        ladder::primary()
                                    } else {
                                        ladder::foreground()
                                    })
                                    .when(matches!(handle, Drag::Handle(..)), |d| d.rounded_full())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.before_drag = Some(this.value.clone());
                                            this.dragging = Some(handle);
                                            if let Drag::Anchor(i) = handle {
                                                this.selected = i.min(this.value.points.len() - 2);
                                            }
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    )
                                    .agent_node(role, label)
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
                                        if let Drag::Anchor(i) = hit {
                                            this.selected = i.min(this.value.points.len() - 2);
                                        }
                                    } else if e.click_count == 2 {
                                        match this.value.insert_point(p[0]) {
                                            Ok(i) => {
                                                this.dragging = Some(Drag::Anchor(i));
                                                this.selected = i;
                                                this.error = None;
                                            }
                                            Err(e) => this.error = Some(e.to_string()),
                                        }
                                    } else {
                                        this.selected = this
                                            .value
                                            .points
                                            .partition_point(|a| a[0] < p[0])
                                            .saturating_sub(1)
                                            .min(this.value.points.len() - 2);
                                    }
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| {
                                this.move_drag(e.position, cx)
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.commit(cx)),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.commit(cx)),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                                    let Some(p) = this.position(e.position) else {
                                        return;
                                    };
                                    if let Some(Drag::Anchor(i)) = this.hit(p) {
                                        match this.value.remove_point(i) {
                                            Ok(()) => {
                                                this.selected =
                                                    this.selected.min(this.value.points.len() - 2);
                                                this.error = None;
                                                cx.emit(EnvelopeChanged(this.value.clone()));
                                            }
                                            Err(e) => this.error = Some(e.to_string()),
                                        }
                                    }
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .agent_node(Role::Card, "Envelope curve"),
                    ),
            )
            .child(
                div().flex().gap(px(4.)).children(
                    [("Straight", false), ("Curve", true)]
                        .into_iter()
                        .map(|(label, mode)| {
                            crate::luma_button(label, crate::Enabled::Yes)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| this.choose_curve(mode, cx)),
                                )
                                .agent_node(Role::Button, format!("Envelope {label}"))
                        }),
                ),
            )
            .child(div().text_size(px(11.)).child(
                "Drag points or handles · Double-click to add · Right-click a point to remove",
            ))
            .children(
                self.error
                    .as_ref()
                    .map(|error| div().text_size(px(11.)).child(error.clone())),
            )
            .child(
                div().flex().flex_wrap().gap(px(4.)).children(
                    [
                        ("Hard", Envelope::soft_edges(0.)),
                        ("Soft", soft_preset()),
                        ("Triangle", Envelope::soft_edges(1.)),
                        ("Ramp up", Envelope::linear(vec![[0., 0.], [1., 1.]])),
                        ("Ramp down", Envelope::linear(vec![[0., 1.], [1., 0.]])),
                    ]
                    .into_iter()
                    .map(|(label, value)| {
                        crate::luma_button(label, crate::Enabled::Yes)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.set_value(value.clone(), cx);
                                    this.selected = 0;
                                    cx.emit(EnvelopeChanged(this.value.clone()));
                                }),
                            )
                            .agent_node(Role::Button, label)
                    }),
                ),
            )
    }
}
fn soft_preset() -> Envelope {
    Envelope {
        points: vec![[0., 0.], [0.2, 1.], [0.8, 1.], [1., 0.]],
        curves: vec![
            EnvelopeCurve::Bezier {
                control1: [0.07, 0.],
                control2: [0.13, 1.],
            },
            EnvelopeCurve::Linear,
            EnvelopeCurve::Bezier {
                control1: [0.87, 1.],
                control2: [0.93, 0.],
            },
        ],
    }
}
