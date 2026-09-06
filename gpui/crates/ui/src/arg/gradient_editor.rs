//! Reusable gradient value control for graph inputs. Selection is editor state;
//! the host receives only the ordered color stops.
use super::{
    color::{ColorArg, ColorArgEditor, ColorArgEvent},
    gradient::{luma_gradient_bar, Gradient, GradientEvent},
};
use crate::node::{Instrument, Role};
use gpui::prelude::*;
use gpui::{div, px, Context, Entity, EventEmitter, Rgba, Subscription, Window};

pub struct GradientChanged(pub Gradient);
pub struct GradientEditor {
    value: Gradient,
    selected: usize,
    color: Entity<ColorArgEditor>,
    _subscription: Subscription,
}
impl EventEmitter<GradientChanged> for GradientEditor {}
impl GradientEditor {
    pub fn new(value: Gradient, cx: &mut Context<Self>) -> Self {
        let c = value.stops()[0].color;
        let color = cx.new(|cx| {
            ColorArgEditor::new("Stop color", ColorArg::decode([c.r, c.g, c.b], 1.), cx).rgb_only()
        });
        let subscription = cx.subscribe(&color, |this, _, event: &ColorArgEvent, cx| {
            let ColorArgEvent::Changed(color) = event;
            this.value.set_color(
                this.selected,
                Rgba {
                    r: color.rgb[0],
                    g: color.rgb[1],
                    b: color.rgb[2],
                    a: 1.,
                },
            );
            cx.emit(GradientChanged(this.value.clone()));
            cx.notify();
        });
        Self {
            value,
            selected: 0,
            color,
            _subscription: subscription,
        }
    }
    pub fn set_value(&mut self, value: Gradient, cx: &mut Context<Self>) {
        self.value = value;
        self.selected = self.selected.min(self.value.stops().len() - 1);
        self.sync_color(cx);
        cx.notify();
    }
    fn sync_color(&self, cx: &mut Context<Self>) {
        let c = self.value.stops()[self.selected].color;
        self.color.update(cx, |color, cx| {
            color.set_value(ColorArg::decode([c.r, c.g, c.b], 1.), cx)
        });
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
                                this.selected = index;
                                this.sync_color(cx);
                                cx.notify();
                                return;
                            }
                            GradientEvent::Move { index, t } => {
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
            .child(self.color.clone())
            .when(self.value.stops().len() > 2, |view| {
                view.child(
                    div()
                        .id("remove-gradient-stop")
                        .cursor_pointer()
                        .child("Remove stop")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.value.remove(this.selected);
                            this.selected = this.selected.min(this.value.stops().len() - 1);
                            this.sync_color(cx);
                            cx.emit(GradientChanged(this.value.clone()));
                            cx.notify();
                        }))
                        .agent_node(Role::Button, "Remove gradient stop"),
                )
            })
    }
}
