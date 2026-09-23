//! Reusable gradient value control for graph inputs. Selection is editor state;
//! the host receives only the ordered color stops.
//!
//! One compact layout: a preset chip, the bar with a marker per stop, and one
//! row for the selected stop: its swatch, whose picker holds hex and opacity,
//! and its position.
use super::{
    color::{ColorArg, ColorArgEditor, ColorArgEvent, ColorOpacity},
    gradient::{luma_gradient_stops, Gradient, GradientEvent, GradientStop},
    number::{DraftedNumber, NumberEvent},
    preset_picker::{luma_preset_picker, Thumb},
    select::MenuVisibility,
};
use crate::node::{Instrument, Role};
use gpui::prelude::*;
use gpui::{div, px, Context, Entity, EventEmitter, Rgba, SharedString, Subscription, Window};

pub struct GradientChanged(pub Gradient);
pub struct GradientEditor {
    value: Gradient,
    selected: usize,
    /// The stop being dragged off the bar, if any.
    detached: Option<usize>,
    presets: MenuVisibility,
    alpha: bool,
    color: Entity<ColorArgEditor>,
    position: Entity<DraftedNumber>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<GradientChanged> for GradientEditor {}

/// The shipped gradients, as the editor's value type.
fn presets() -> Vec<(SharedString, Gradient)> {
    luma_patterns::presets()
        .gradients
        .iter()
        .map(|preset| {
            let stops = preset.gradient.stops.iter().map(|stop| GradientStop {
                t: stop.t as f32,
                color: Rgba {
                    r: stop.color[0] as f32,
                    g: stop.color[1] as f32,
                    b: stop.color[2] as f32,
                    a: stop.alpha as f32,
                },
            });
            (preset.name.clone().into(), Gradient::new(stops))
        })
        .collect()
}

/// The preset `value` equals, if any. Colors match to within half an 8-bit
/// step, since some hosts store them as hex.
fn preset_of(presets: &[(SharedString, Gradient)], value: &Gradient) -> Option<usize> {
    let close = |a: f32, b: f32| (a - b).abs() <= 0.5 / 255. + 1e-4;
    presets.iter().position(|(_, preset)| {
        preset.stops().len() == value.stops().len()
            && preset.stops().iter().zip(value.stops()).all(|(a, b)| {
                close(a.t, b.t)
                    && close(a.color.r, b.color.r)
                    && close(a.color.g, b.color.g)
                    && close(a.color.b, b.color.b)
                    && close(a.color.a, b.color.a)
            })
    })
}

impl GradientEditor {
    pub fn new(value: Gradient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let c = value
            .stops()
            .first()
            .map_or(gpui::white().into(), |stop| stop.color);
        let t = value.stops().first().map_or(0., |stop| stop.t);
        let color = cx.new(|cx| {
            ColorArgEditor::new("Stop color", ColorArg::decode([c.r, c.g, c.b], 1.), cx)
                .rgb_only()
                .with_opacity(c.a)
        });
        let position = cx.new(|cx| {
            DraftedNumber::new("Stop position", percent(t), 0., 100., PERCENT_W, window, cx)
                .with_unit("%")
        });
        let subscriptions = vec![
            cx.subscribe(&color, |this, _, event: &ColorArgEvent, cx| {
                let ColorArgEvent::Changed(color) = event;
                let [r, g, b] = color.rgb;
                this.edit_color(cx, |c| Rgba { r, g, b, a: c.a });
            }),
            cx.subscribe(&color, |this, _, event: &ColorOpacity, cx| {
                let ColorOpacity(alpha) = *event;
                this.edit_color(cx, |c| Rgba { a: alpha, ..c });
            }),
            cx.subscribe(&position, |this, _, event: &NumberEvent, cx| {
                let NumberEvent::Committed(at) = *event;
                if this.selected >= this.value.stops().len() {
                    return;
                }
                this.value.move_stop(this.selected, (at / 100.) as f32);
                this.changed(cx);
            }),
        ];
        Self {
            value,
            selected: 0,
            detached: None,
            presets: MenuVisibility::Closed,
            alpha: true,
            color,
            position,
            _subscriptions: subscriptions,
        }
    }
    /// The editor without the opacity field, for a host that stores colors
    /// without opacity.
    pub fn without_alpha(mut self, cx: &mut Context<Self>) -> Self {
        self.alpha = false;
        self.color.update(cx, |color, cx| color.without_opacity(cx));
        self
    }
    pub fn set_value(&mut self, value: Gradient, cx: &mut Context<Self>) {
        self.value = value;
        self.selected = self
            .selected
            .min(self.value.stops().len().saturating_sub(1));
        self.sync_fields(cx);
        cx.notify();
    }
    fn edit_color(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(Rgba) -> Rgba) {
        let Some(stop) = self.value.stops().get(self.selected) else {
            return;
        };
        self.value.set_color(self.selected, edit(stop.color));
        self.changed(cx);
    }
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.sync_fields(cx);
        cx.emit(GradientChanged(self.value.clone()));
        cx.notify();
    }
    fn sync_fields(&self, cx: &mut Context<Self>) {
        let Some(stop) = self.value.stops().get(self.selected).copied() else {
            return;
        };
        let c = stop.color;
        self.color.update(cx, |color, cx| {
            color.set_value(ColorArg::decode([c.r, c.g, c.b], 1.), cx)
        });
        let alpha = self.alpha;
        self.color.update(cx, |color, cx| {
            if alpha {
                color.set_opacity(c.a, cx);
            }
        });
        self.position
            .update(cx, |field, cx| field.set_value(percent(stop.t), cx));
    }
    fn on_stops(&mut self, event: GradientEvent, cx: &mut Context<Self>) {
        let count = self.value.stops().len();
        match event {
            GradientEvent::Select(index) if index < count => {
                self.selected = index;
                self.sync_fields(cx);
                cx.notify();
            }
            GradientEvent::Move { index, t } if index < count => {
                self.value.move_stop(index, t);
                self.changed(cx);
            }
            GradientEvent::Add { t } if count < 64 => {
                self.selected = self.value.insert(t);
                self.changed(cx);
            }
            GradientEvent::Detach { index, off } if index < count => {
                let detached = off.then_some(index);
                if detached != self.detached {
                    self.detached = detached;
                    cx.notify();
                }
            }
            GradientEvent::Release => {
                if let Some(index) = self.detached.take() {
                    self.remove(index, cx);
                }
            }
            GradientEvent::Remove(index) => self.remove(index, cx),
            _ => {}
        }
    }
    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.value.remove(index) {
            return;
        }
        self.selected = self
            .selected
            .min(self.value.stops().len().saturating_sub(1));
        self.changed(cx);
    }
}

fn percent(value: f32) -> f64 {
    (f64::from(value) * 1000.).round() / 10.
}

/// Both percent fields, so they line up.
const PERCENT_W: f32 = 58.;

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
                .text_color(crate::ladder::foreground_alpha(0.5))
                .child(name),
        )
        .child(field)
}

impl Render for GradientEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.presets.tick_close(crate::motion::reduced_motion(cx)) {
            window.request_animation_frame();
        }
        let stops = cx.entity();
        let toggle = cx.entity();
        let pick = cx.entity();
        let presets = presets();
        let current = preset_of(&presets, &self.value);
        let options: Vec<(SharedString, Thumb)> = presets
            .iter()
            .map(|(name, gradient)| (name.clone(), Thumb::Gradient(gradient.clone())))
            .collect();
        let chip = luma_preset_picker(
            format!("gradient-presets-{}", cx.entity_id()),
            &Thumb::Gradient(self.value.clone()),
            current,
            &options,
            false,
            self.presets,
            move |_, cx| {
                toggle.update(cx, |this, cx| {
                    this.presets.toggle();
                    cx.notify();
                })
            },
            move |picked, _, cx| {
                pick.update(cx, |this, cx| {
                    this.presets.close();
                    if let Some((_, gradient)) = picked.and_then(|at| presets.get(at)) {
                        this.value = gradient.clone();
                        this.selected = 0;
                        this.changed(cx);
                    }
                    cx.notify();
                })
            },
        );
        let empty = self.value.stops().is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .w_full()
            .child(chip)
            .child(luma_gradient_stops(
                "graph-gradient",
                &self.value,
                (!empty).then_some(self.selected),
                self.detached,
                move |event, _, cx| stops.update(cx, |this, cx| this.on_stops(event, cx)),
            ))
            .when(empty, |view| {
                view.child(div().child("No colors").agent_node(Role::Text, "No colors"))
                    .child(
                        crate::button::button("Add color", crate::button::Enabled::Yes)
                            .id("add-gradient-color")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected = this.value.insert(0.);
                                this.changed(cx);
                            }))
                            .agent_node(Role::Button, "Add gradient color"),
                    )
            })
            .when(!empty, |view| {
                view.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.))
                        .child(self.color.clone())
                        .child(labelled("Position", self.position.clone())),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_is_found_again() {
        let presets = presets();
        assert!(presets.len() >= 7);
        for (at, (_, gradient)) in presets.iter().enumerate() {
            assert_eq!(preset_of(&presets, gradient), Some(at));
        }
        let mut edited = presets[0].1.clone();
        edited.move_stop(1, 0.3);
        assert_eq!(preset_of(&presets, &edited), None);
        // A preset stored as hex is still found.
        let mut hex = presets[1].1.clone();
        let c = hex.stops()[0].color;
        let byte = |v: f32| (v * 255.).round() / 255.;
        hex.set_color(
            0,
            Rgba {
                r: byte(c.r),
                g: byte(c.g),
                b: byte(c.b),
                a: 1.,
            },
        );
        assert_eq!(preset_of(&presets, &hex), Some(1));
    }
}
