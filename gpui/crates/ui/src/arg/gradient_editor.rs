//! Reusable gradient value control for graph inputs. Selection is editor state;
//! the host receives only the ordered color stops.
use super::{
    color::{ColorArg, ColorArgEditor, ColorArgEvent},
    gradient::{luma_gradient_bar, Gradient, GradientEvent},
    number::{DraftedNumber, NumberEvent},
};
use crate::node::{Instrument, Role};
use gpui::prelude::*;
use gpui::{div, px, Context, Entity, EventEmitter, Rgba, Subscription, Window};

pub struct GradientChanged(pub Gradient);
pub struct GradientEditor {
    value: Gradient,
    selected: usize,
    color: Entity<ColorArgEditor>,
    opacity: Entity<DraftedNumber>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<GradientChanged> for GradientEditor {}
impl GradientEditor {
    pub fn new(value: Gradient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let c = value
            .stops()
            .first()
            .map_or(gpui::white().into(), |stop| stop.color);
        let color = cx.new(|cx| {
            ColorArgEditor::new("Stop color", ColorArg::decode([c.r, c.g, c.b], 1.), cx).rgb_only()
        });
        let subscription = cx.subscribe(&color, |this, _, event: &ColorArgEvent, cx| {
            let ColorArgEvent::Changed(color) = event;
            let Some(stop) = this.value.stops().get(this.selected) else {
                return;
            };
            let alpha = stop.color.a;
            this.value.set_color(
                this.selected,
                Rgba {
                    r: color.rgb[0],
                    g: color.rgb[1],
                    b: color.rgb[2],
                    a: alpha,
                },
            );
            cx.emit(GradientChanged(this.value.clone()));
            cx.notify();
        });
        let opacity = cx
            .new(|cx| DraftedNumber::new("Stop opacity", f64::from(c.a), 0., 1., 256., window, cx));
        let opacity_subscription = cx.subscribe(&opacity, |this, _, event: &NumberEvent, cx| {
            let NumberEvent::Committed(alpha) = *event;
            let Some(stop) = this.value.stops().get(this.selected) else {
                return;
            };
            let mut color = stop.color;
            color.a = alpha as f32;
            this.value.set_color(this.selected, color);
            cx.emit(GradientChanged(this.value.clone()));
            cx.notify();
        });
        Self {
            value,
            selected: 0,
            color,
            opacity,
            _subscriptions: vec![subscription, opacity_subscription],
        }
    }
    pub fn set_value(&mut self, value: Gradient, cx: &mut Context<Self>) {
        self.value = value;
        self.selected = self
            .selected
            .min(self.value.stops().len().saturating_sub(1));
        self.sync_color(cx);
        cx.notify();
    }
    fn sync_color(&self, cx: &mut Context<Self>) {
        let Some(stop) = self.value.stops().get(self.selected) else {
            return;
        };
        let c = stop.color;
        self.color.update(cx, |color, cx| {
            color.set_value(ColorArg::decode([c.r, c.g, c.b], 1.), cx)
        });
        self.opacity
            .update(cx, |opacity, cx| opacity.set_value(f64::from(c.a), cx));
    }
}
impl Render for GradientEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .w_full()
            .child(luma_gradient_bar(
                "graph-gradient",
                &self.value,
                Some(self.selected),
                256.,
                move |event, _, cx| {
                    entity.update(cx, |this, cx| {
                        match event {
                            GradientEvent::Select(index) => {
                                if index >= this.value.stops().len() {
                                    return;
                                }
                                this.selected = index;
                                this.sync_color(cx);
                                cx.notify();
                                return;
                            }
                            GradientEvent::Move { index, t } => {
                                if index >= this.value.stops().len() {
                                    return;
                                }
                                this.value.move_stop(index, t);
                            }
                            GradientEvent::Add { t } => {
                                if this.value.stops().len() >= 64 {
                                    return;
                                }
                                this.selected = this.value.insert(t);
                                this.sync_color(cx);
                            }
                        }
                        cx.emit(GradientChanged(this.value.clone()));
                        cx.notify();
                    });
                },
            ))
            .when(self.value.stops().is_empty(), |view| {
                view.child(div().child("No colors").agent_node(Role::Text, "No colors"))
                    .child(
                        crate::button::button("Add color", crate::button::Enabled::Yes)
                            .id("add-gradient-color")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected = this.value.insert(0.);
                                this.sync_color(cx);
                                cx.emit(GradientChanged(this.value.clone()));
                                cx.notify();
                            }))
                            .agent_node(Role::Button, "Add gradient color"),
                    )
            })
            .when(!self.value.stops().is_empty(), |view| {
                view.child(self.color.clone())
                    .child(self.opacity.clone())
                    .child(
                        crate::button::button("Remove color", crate::button::Enabled::Yes)
                            .id("remove-gradient-stop")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.value.remove(this.selected) {
                                    return;
                                }
                                this.selected = this
                                    .selected
                                    .min(this.value.stops().len().saturating_sub(1));
                                this.sync_color(cx);
                                cx.emit(GradientChanged(this.value.clone()));
                                cx.notify();
                            }))
                            .agent_node(Role::Button, "Remove gradient stop"),
                    )
            })
    }
}
