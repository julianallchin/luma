//! The drafted numeric input: a text field that owns a *draft*, and a host
//! that only ever hears numbers.
//!
//! Typing edits the draft alone. Enter and blur try to commit — parse, clamp
//! into the range, reformat — and an unparseable draft reverts to the last
//! committed value instead of emitting anything. Escape reverts outright. The
//! host therefore never sees `"1."`, `""`, or `"-"` mid-keystroke; the whole
//! point of the widget is that those states cannot leave it.
//!
//! Built on [`TextInput`] in its search mode: that keymap deliberately leaves
//! `enter` and `escape` unbound, so this wrapper hears them as plain key
//! events and gives them their meaning here — the same division of labor a
//! picker's filter field uses.

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Subscription, Window,
};

use crate::float;
use crate::node::{Instrument, Role};
use crate::text_input::TextInput;

/// Parse a draft against its range. `None` is "revert": empty, unparseable,
/// or non-finite input has no number in it to commit. A finite number outside
/// the range commits clamped — the pointer analog (a slider) clamps, and a
/// typed `999` meaning "all the way up" should not bounce back.
#[must_use]
pub fn parse_draft(draft: &str, min: f64, max: f64) -> Option<f64> {
    let value: f64 = draft.trim().parse().ok()?;
    value.is_finite().then(|| value.clamp(min, max))
}

/// The one spelling a committed value shows as — also what a revert restores,
/// so draft-vs-value comparison is string equality on this.
#[must_use]
pub fn format_value(value: f64) -> String {
    format!("{value}")
}

/// What the field tells its host: a draft became a number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumberEvent {
    Committed(f64),
}

/// The entity. Emits [`NumberEvent::Committed`] only when a commit lands on a
/// *different* value — enter-then-blur is one commit, not two.
pub struct DraftedNumber {
    /// Names the field's automation node, so two number cells in one strip
    /// stay tellable apart — the same contract as a slider's id.
    id: SharedString,
    input: Entity<TextInput>,
    value: f64,
    min: f64,
    max: f64,
    width: f32,
    _blur: Subscription,
}

impl EventEmitter<NumberEvent> for DraftedNumber {}

impl DraftedNumber {
    pub fn new(
        id: impl Into<SharedString>,
        value: f64,
        min: f64,
        max: f64,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value = value.clamp(min, max);
        let input = cx.new(|cx| {
            let mut input = TextInput::search("", cx);
            input.set_text(format_value(value), cx);
            input
        });
        // Blur is a commit point. The subscription lives on this entity, so a
        // dropped editor stops listening with it.
        let this = cx.entity().downgrade();
        let handle = input.focus_handle(cx);
        let _blur = window.on_focus_out(&handle, cx, move |_, window, cx| {
            this.update(cx, |editor, cx| editor.commit(window, cx)).ok();
        });
        Self {
            id: id.into(),
            input,
            value,
            min,
            max,
            width,
            _blur,
        }
    }

    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
    }

    /// A host-side write. Stomps any draft in progress — the host is asserting
    /// the value moved under the field, and a draft over a stale value is the
    /// worse thing to keep.
    pub fn set_value(&mut self, value: f64, cx: &mut Context<Self>) {
        self.value = value.clamp(self.min, self.max);
        let text = format_value(self.value);
        self.input.update(cx, |input, cx| input.set_text(text, cx));
        cx.notify();
    }

    fn commit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let draft = self.input.read(cx).text().to_string();
        match parse_draft(&draft, self.min, self.max) {
            Some(value) => {
                let changed = value != self.value;
                self.value = value;
                let text = format_value(value);
                if draft != text {
                    self.input.update(cx, |input, cx| input.set_text(text, cx));
                }
                if changed {
                    cx.emit(NumberEvent::Committed(value));
                }
            }
            None => self.revert(cx),
        }
    }

    fn revert(&mut self, cx: &mut Context<Self>) {
        let text = format_value(self.value);
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.commit(window, cx),
            "escape" => self.revert(cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Focusable for DraftedNumber {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for DraftedNumber {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reading = format!("{} = {}", self.id, self.input.read(cx).text());
        float::field()
            .on_key_down(cx.listener(Self::on_key_down))
            .w(px(self.width))
            .font_family(crate::fonts::MONO)
            .child(div().w_full().child(self.input.clone()))
            .agent_node(Role::Input, reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commit rule in one table: numbers commit (clamped), everything
    /// else reverts. This is the "never emits intermediate garbage" contract
    /// at its decision point.
    #[test]
    fn drafts_commit_or_revert() {
        assert_eq!(parse_draft("42", 0., 100.), Some(42.));
        assert_eq!(parse_draft("  3.5 ", 0., 100.), Some(3.5));
        assert_eq!(parse_draft("-7", -10., 10.), Some(-7.));
        // Out of range commits clamped, not rejected.
        assert_eq!(parse_draft("999", 0., 100.), Some(100.));
        assert_eq!(parse_draft("-999", 0., 100.), Some(0.));
        // The states a draft passes through while being typed.
        assert_eq!(parse_draft("", 0., 100.), None);
        assert_eq!(parse_draft("-", 0., 100.), None);
        assert_eq!(parse_draft("1.2.3", 0., 100.), None);
        assert_eq!(parse_draft("abc", 0., 100.), None);
        // Parseable but not a number anyone committed.
        assert_eq!(parse_draft("NaN", 0., 100.), None);
        assert_eq!(parse_draft("inf", 0., 100.), None);
    }

    /// One spelling per value, and reverting restores exactly it.
    #[test]
    fn formatting_is_stable() {
        assert_eq!(format_value(42.), "42");
        assert_eq!(format_value(3.5), "3.5");
        assert_eq!(format_value(-0.25), "-0.25");
    }
}
