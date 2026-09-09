//! A compact value that scrubs relatively on drag and edits as text on click.
//! Pointer edits preview live and commit on release, keeping one undo step per gesture.
use crate::node::{Instrument, Role};
use crate::{arg::number::parse_draft, float, ladder, text_input::TextInput};
use gpui::prelude::*;
use gpui::*;
use std::rc::Rc;

type Change = Rc<dyn Fn(f64, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ScrubNumber {
    id: SharedString,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    width: f32,
    unit: SharedString,
    change: Change,
    preview: Option<Change>,
}
impl ScrubNumber {
    pub fn new(
        id: impl Into<SharedString>,
        value: f64,
        range: std::ops::RangeInclusive<f64>,
        step: f64,
        width: f32,
        unit: impl Into<SharedString>,
        change: impl Fn(f64, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            value,
            min: *range.start(),
            max: *range.end(),
            step,
            width,
            unit: unit.into(),
            change: Rc::new(change),
            preview: None,
        }
    }
    /// Preview transient changes without recording history or persisting them.
    pub fn on_preview(mut self, preview: impl Fn(f64, &mut Window, &mut App) + 'static) -> Self {
        self.preview = Some(Rc::new(preview));
        self
    }
}
struct Editor {
    config: ScrubNumber,
    input: Entity<TextInput>,
    editing: bool,
    press: Option<(Point<Pixels>, f64)>,
    dragged: bool,
    value: f64,
    _blur: Subscription,
}
impl Editor {
    fn finish(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        self.editing = false;
        if self.input.focus_handle(cx).is_focused(window) {
            window.blur();
        }
        if commit {
            if let Some(value) =
                parse_draft(self.input.read(cx).text(), self.config.min, self.config.max)
            {
                self.value = value;
                if value != self.config.value {
                    (self.config.change)(value, window, cx);
                }
            }
        }
        cx.notify();
    }
}
impl RenderOnce for ScrubNumber {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = ElementId::Name(self.id.clone());
        // The initializer owns a copy; every render refreshes the host value and callback.
        let initial = ScrubNumber {
            id: self.id.clone(),
            value: self.value,
            min: self.min,
            max: self.max,
            step: self.step,
            width: self.width,
            unit: self.unit.clone(),
            change: self.change.clone(),
            preview: self.preview.clone(),
        };
        let editor = window.use_keyed_state(id, cx, |window, cx| {
            let input = cx.new(|cx| TextInput::search("", cx));
            let weak = cx.entity().downgrade();
            let blur = window.on_focus_out(&input.focus_handle(cx), cx, move |_, window, cx| {
                weak.update(cx, |this: &mut Editor, cx| this.finish(true, window, cx))
                    .ok();
            });
            Editor {
                value: initial.value,
                config: initial,
                input,
                editing: false,
                press: None,
                dragged: false,
                _blur: blur,
            }
        });
        editor.update(cx, |editor, cx| {
            if !editor.editing && editor.press.is_none() && editor.value != self.value {
                editor.value = self.value;
                cx.notify();
            }
            editor.config = self;
        });
        editor
    }
}
impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.editing {
            return float::field()
                .w(px(self.config.width))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => this.finish(true, window, cx),
                        "escape" => this.finish(false, window, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .child(div().w_full().child(self.input.clone()))
                .agent_node(Role::Input, format!("{} input", self.config.id))
                .into_any_element();
        }
        let entity = cx.entity();
        let reading = format!("{:.2} {}", self.value, self.config.unit);
        float::field()
            .id(self.config.id.clone())
            .w(px(self.config.width))
            .cursor_ew_resize()
            .text_color(ladder::foreground())
            .font_family(crate::fonts::MONO)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.press = Some((event.position, this.value));
                    this.dragged = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(reading.clone())
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        let moving = entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() {
                                return;
                            }
                            moving.update(cx, |this, cx| {
                                let Some((start, value)) = this.press else {
                                    return;
                                };
                                let delta = f64::from(f32::from(event.position.x - start.x));
                                if delta.abs() >= 3.0 {
                                    this.dragged = true;
                                }
                                if this.dragged {
                                    let next = ((value + delta * this.config.step / 4.0)
                                        / this.config.step)
                                        .round()
                                        .mul_add(this.config.step, 0.0)
                                        .clamp(this.config.min, this.config.max);
                                    if next != this.value {
                                        this.value = next;
                                        if let Some(preview) = &this.config.preview {
                                            preview(next, window, cx);
                                        }
                                        cx.notify();
                                    }
                                }
                            });
                        });
                        let release = entity.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }
                            release.update(cx, |this, cx| {
                                let Some((_, original)) = this.press.take() else {
                                    return;
                                };
                                if this.dragged {
                                    if this.value != original {
                                        (this.config.change)(this.value, window, cx);
                                    }
                                } else {
                                    this.editing = true;
                                    this.input.update(cx, |input, cx| {
                                        input.set_text(format!("{:.2}", this.value), cx)
                                    });
                                    window.focus(&this.input.focus_handle(cx), cx);
                                }
                                cx.stop_propagation();
                                cx.notify();
                            });
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .agent_node(Role::Slider, format!("{} = {reading}", self.config.id))
            .into_any_element()
    }
}
