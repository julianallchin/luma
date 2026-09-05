//! A reusable normalized curve editor. Hosts persist only committed point sets.
use crate::{
    ladder,
    node::{Instrument, Role},
};
use gpui::prelude::*;
use gpui::{
    canvas, div, point, px, Bounds, Context, EventEmitter, MouseButton, PathBuilder, Pixels, Point,
    Window,
};

#[derive(Clone, Debug)]
pub struct EnvelopeChanged(pub Vec<[f64; 2]>);
pub struct EnvelopeEditor {
    points: Vec<[f64; 2]>,
    bounds: Option<Bounds<Pixels>>,
    dragging: Option<usize>,
}
impl EventEmitter<EnvelopeChanged> for EnvelopeEditor {}
impl EnvelopeEditor {
    pub fn new(points: Vec<[f64; 2]>) -> Self {
        Self {
            points: valid_points(points),
            bounds: None,
            dragging: None,
        }
    }
    pub fn set_value(&mut self, points: Vec<[f64; 2]>, cx: &mut Context<Self>) {
        self.points = valid_points(points);
        self.dragging = None;
        cx.notify();
    }
    fn position(&self, p: Point<Pixels>) -> Option<[f64; 2]> {
        let b = self.bounds?;
        Some([
            (f32::from(p.x - b.origin.x) / f32::from(b.size.width)).clamp(0., 1.) as f64,
            (1. - f32::from(p.y - b.origin.y) / f32::from(b.size.height)).clamp(0., 1.) as f64,
        ])
    }
    fn move_point(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(i), Some(mut p)) = (self.dragging, self.position(p)) else {
            return;
        };
        p[0] = if i == 0 {
            0.
        } else if i + 1 == self.points.len() {
            1.
        } else {
            let (prev, next) = (self.points[i - 1][0], self.points[i + 1][0]);
            let margin = (next - prev) * 1e-6;
            if prev + margin > prev && next - margin < next {
                p[0].clamp(prev + margin, next - margin)
            } else {
                self.points[i][0]
            }
        };
        self.points[i] = p;
        cx.notify();
    }
    fn commit(&mut self, cx: &mut Context<Self>) {
        if self.dragging.take().is_some() {
            cx.emit(EnvelopeChanged(self.points.clone()));
        }
        cx.notify();
    }
}
impl Render for EnvelopeEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let points = self.points.clone();
        let this = cx.entity();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .w_full()
                    .h(px(110.))
                    .flex_none()
                    .border_1()
                    .border_color(ladder::foreground().opacity(0.3))
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                this.update(cx, |this, _| this.bounds = Some(bounds));
                            },
                            move |bounds, _, window, _| {
                                let at = |p: [f64; 2]| {
                                    point(
                                        bounds.origin.x + bounds.size.width * p[0] as f32,
                                        bounds.origin.y + bounds.size.height * (1. - p[1]) as f32,
                                    )
                                };
                                let mut path = PathBuilder::stroke(px(2.));
                                if let Some(p) = points.first() {
                                    path.move_to(at(*p));
                                }
                                for p in points.iter().skip(1) {
                                    path.line_to(at(*p));
                                }
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, ladder::foreground());
                                }
                                for p in &points {
                                    let p = at(*p);
                                    let mut dot = PathBuilder::fill();
                                    dot.move_to(point(p.x - px(3.), p.y - px(3.)));
                                    dot.line_to(point(p.x + px(3.), p.y - px(3.)));
                                    dot.line_to(point(p.x + px(3.), p.y + px(3.)));
                                    dot.line_to(point(p.x - px(3.), p.y + px(3.)));
                                    dot.close();
                                    if let Ok(path) = dot.build() {
                                        window.paint_path(path, ladder::foreground());
                                    }
                                }
                            },
                        )
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                            let Some(p) = this.position(e.position) else {
                                return;
                            };
                            let nearest = this
                                .points
                                .iter()
                                .enumerate()
                                .min_by(|(_, a), (_, b)| {
                                    ((a[0] - p[0]).powi(2) + (a[1] - p[1]).powi(2))
                                        .total_cmp(&((b[0] - p[0]).powi(2) + (b[1] - p[1]).powi(2)))
                                })
                                .map(|(i, _)| i)
                                .unwrap();
                            let near = this.points[nearest];
                            if (near[0] - p[0]).hypot(near[1] - p[1]) < 0.08 {
                                this.dragging = Some(nearest);
                            } else if p[0] > 1e-6
                                && p[0] < 1. - 1e-6
                                && this.points.iter().all(|v| (v[0] - p[0]).abs() > 2e-6)
                            {
                                let i = this.points.partition_point(|v| v[0] < p[0]);
                                this.points.insert(i, p);
                                this.dragging = Some(i);
                            }
                            this.move_point(e.position, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| {
                        this.move_point(e.position, cx)
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
                            if let Some(i) = (1..this.points.len() - 1).min_by(|a, b| {
                                (this.points[*a][0] - p[0])
                                    .abs()
                                    .total_cmp(&(this.points[*b][0] - p[0]).abs())
                            }) {
                                if (this.points[i][0] - p[0]).abs() < 0.05 {
                                    this.points.remove(i);
                                    cx.emit(EnvelopeChanged(this.points.clone()));
                                    cx.notify();
                                }
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .agent_node(Role::Card, "Envelope curve"),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .child("Drag points · Click to add · Right-click to remove"),
            )
            .child(
                div().flex().flex_wrap().gap(px(4.)).children(
                    [
                        ("Hard", vec![[0., 1.], [1., 1.]]),
                        ("Soft", vec![[0., 0.], [0.15, 1.], [0.85, 1.], [1., 0.]]),
                        ("Triangle", vec![[0., 0.], [0.5, 1.], [1., 0.]]),
                        ("Ramp up", vec![[0., 0.], [1., 1.]]),
                        ("Ramp down", vec![[0., 1.], [1., 0.]]),
                    ]
                    .into_iter()
                    .map(|(label, points)| {
                        crate::luma_button(label, crate::Enabled::Yes)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.points = points.clone();
                                    this.dragging = None;
                                    cx.emit(EnvelopeChanged(this.points.clone()));
                                    cx.notify();
                                }),
                            )
                            .agent_node(Role::Button, label)
                    }),
                ),
            )
    }
}

fn valid_points(points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    if points.len() >= 2
        && points[0][0] == 0.
        && points.last().unwrap()[0] == 1.
        && points
            .iter()
            .flatten()
            .all(|v| v.is_finite() && (0. ..=1.).contains(v))
        && points.windows(2).all(|p| p[0][0] < p[1][0])
    {
        points
    } else {
        vec![[0., 1.], [1., 1.]]
    }
}
