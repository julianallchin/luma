//! Component editing for constant numerical inputs, shared by graph and clip
//! inspectors. Larger tensors remain readable without creating thousands of fields.
use super::{
    color::{ColorArg, ColorArgEditor, ColorArgEvent},
    number::{DraftedNumber, NumberEvent},
};
use crate::node::{Instrument, Role};
use crate::Enabled;
use gpui::prelude::*;
use gpui::{div, px, Context, Entity, EventEmitter, SharedString, Subscription, Window};
use luma_patterns::{Channels, Signal, Unit};

const PAGE: usize = 8;
#[derive(Clone, Debug)]
pub struct SignalChanged(pub Signal);

pub struct SignalEditor {
    name: SharedString,
    value: Signal,
    offset: usize,
    width: f32,
    fields: Vec<Entity<DraftedNumber>>,
    color: Option<Entity<ColorArgEditor>>,
    subscriptions: Vec<Subscription>,
}
impl EventEmitter<SignalChanged> for SignalEditor {}
impl SignalEditor {
    pub fn new(
        name: impl Into<SharedString>,
        value: Signal,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = Self {
            name: name.into(),
            value,
            offset: 0,
            width,
            fields: Vec::new(),
            color: None,
            subscriptions: Vec::new(),
        };
        editor.rebuild(window, cx);
        editor
    }
    fn constant(&self) -> bool {
        self.value.fixtures().is_none()
            && self.value.values().dim().0 == 1
            && self.value.values().dim().1 == 1
    }
    fn color_value(&self) -> Option<ColorArg> {
        (self.constant()
            && *self.value.channels() == Channels::Rgb
            && self.value.unit() == Unit::Proportion
            && self.value.values().iter().all(|v| (0.0..=1.0).contains(v)))
        .then(|| {
            ColorArg::decode(
                std::array::from_fn(|c| self.value.values()[[0, 0, c]] as f32),
                1.,
            )
        })
    }
    pub fn set_value(&mut self, value: Signal, window: &mut Window, cx: &mut Context<Self>) {
        if self.value == value {
            return;
        }
        let changed_layout = self.value.values().dim() != value.values().dim()
            || self.value.channels() != value.channels()
            || self.value.unit() != value.unit()
            || self.value.fixtures() != value.fixtures();
        self.value = value;
        if changed_layout || self.color.is_some() != self.color_value().is_some() {
            self.offset = 0;
            self.rebuild(window, cx);
        } else if let (Some(field), Some(color)) = (&self.color, self.color_value()) {
            field.update(cx, |field, cx| field.set_value(color, cx));
        } else {
            for (index, field) in self.fields.iter().enumerate() {
                let value = self.value.values()[[0, 0, self.offset + index]];
                field.update(cx, |field, cx| field.set_value(value, cx));
            }
        }
        cx.notify();
    }
    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fields.clear();
        self.color = None;
        self.subscriptions.clear();
        if !self.constant() {
            return;
        }
        if let Some(color) = self.color_value() {
            let field = cx.new(|cx| ColorArgEditor::new(self.name.clone(), color, cx).rgb_only());
            self.subscriptions
                .push(cx.subscribe(&field, |this, _, event, cx| {
                    let ColorArgEvent::Changed(color) = *event;
                    let mut values = this.value.values().clone();
                    for (c, value) in color.rgb.into_iter().enumerate() {
                        values[[0, 0, c]] = f64::from(value);
                    }
                    this.value =
                        Signal::new(values, this.value.unit(), *this.value.channels(), None)
                            .expect("color editor preserves the constant signal layout");
                    cx.emit(SignalChanged(this.value.clone()));
                    cx.notify();
                }));
            self.color = Some(field);
            return;
        }
        let count = self.value.channels().count();
        for index in self.offset..(self.offset + PAGE).min(count) {
            let label = match self.value.channels() {
                Channels::Rgb => ["Red", "Green", "Blue"][index].to_string(),
                Channels::PanTilt => ["Pan", "Tilt"][index].to_string(),
                _ => format!("{}", index + 1),
            };
            let field = cx.new(|cx| {
                DraftedNumber::new(
                    if count == 1 {
                        self.name.to_string()
                    } else {
                        format!("{}: {label}", self.name)
                    },
                    self.value.values()[[0, 0, index]],
                    // Signals permit all finite values, including headroom
                    // above one. Editing must not clamp their stored samples.
                    f64::MIN,
                    f64::MAX,
                    self.width,
                    window,
                    cx,
                )
            });
            self.subscriptions
                .push(cx.subscribe(&field, move |this, _, event, cx| {
                    let NumberEvent::Committed(value) = *event;
                    let mut values = this.value.values().clone();
                    values[[0, 0, index]] = value;
                    this.value =
                        Signal::new(values, this.value.unit(), *this.value.channels(), None)
                            .expect("number editor preserves the constant signal layout");
                    cx.emit(SignalChanged(this.value.clone()));
                    cx.notify();
                }));
            self.fields.push(field);
        }
    }
}
impl Render for SignalEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.value.channels().count();
        let mut out = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(self.color.iter().cloned())
            .children(self.fields.iter().cloned());
        if !self.constant() {
            let (heads, times, channels) = self.value.values().dim();
            return out.child(format!(
                "{heads} heads × {times} samples × {channels} channels"
            ));
        }
        if count > PAGE {
            out = out.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(format!(
                        "{}–{} of {count}",
                        self.offset + 1,
                        (self.offset + PAGE).min(count)
                    ))
                    .child(
                        crate::button(
                            "Previous",
                            if self.offset > 0 {
                                Enabled::Yes
                            } else {
                                Enabled::No
                            },
                        )
                        .id("signal-previous")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.offset = this.offset.saturating_sub(PAGE);
                            this.rebuild(window, cx);
                            cx.notify();
                        }))
                        .agent_node(Role::Button, format!("{}: Previous channels", self.name)),
                    )
                    .child(
                        crate::button(
                            "Next",
                            if self.offset + PAGE < count {
                                Enabled::Yes
                            } else {
                                Enabled::No
                            },
                        )
                        .id("signal-next")
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.offset + PAGE < this.value.channels().count() {
                                this.offset += PAGE;
                                this.rebuild(window, cx);
                                cx.notify();
                            }
                        }))
                        .agent_node(Role::Button, format!("{}: Next channels", self.name)),
                    ),
            );
        }
        out
    }
}
