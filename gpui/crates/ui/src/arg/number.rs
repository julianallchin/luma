//! The drafted numeric input: a text field that owns a *draft*, and a host
//! that only ever hears numbers.
//!
//! Typing edits the draft alone. Enter and blur try to commit — parse, clamp
//! into the range, reformat — and an unparseable draft reverts to the last
//! committed value instead of emitting anything. Escape reverts outright. The
//! host therefore never sees `"1."`, `""`, or `"-"` mid-keystroke; the whole
//! point of the widget is that those states cannot leave it.
//!
//! Built on [`TextInput`] in its search mode, under [`DRAFT_CONTEXT`] while a
//! draft differs from the value: that context binds `enter` to submit and
//! `escape` to cancel ahead of every binding around the field, and this
//! wrapper gives them their meaning.

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, Window,
};

use crate::float;
use crate::node::{AgentNode, Instrument, Role};
use crate::text_input::{self, TextInput, DRAFT_CONTEXT};

/// Parse a draft against its range. `None` is "revert": empty, unparseable,
/// or non-finite input has no number in it to commit. A finite number outside
/// the range commits clamped — the pointer analog (a slider) clamps, and a
/// typed `999` meaning "all the way up" should not bounce back.
#[must_use]
pub fn parse_draft(draft: &str, min: f64, max: f64) -> Option<f64> {
    let value: f64 = draft.trim().parse().ok()?;
    value.is_finite().then(|| value.clamp_value(min, max))
}

/// The one spelling a committed value shows as — also what a revert restores,
/// so draft-vs-value comparison is string equality on this.
#[must_use]
pub fn format_value(value: f64) -> String {
    format!("{value}")
}

/// A reciprocal as the field shows it: four decimals at most, as 1/3 beat
/// shows as 3 per beat and 0.3 beats as 3.3333 per beat.
fn format_per<T: DraftValue>(value: T) -> String {
    let text = format!("{value:.4}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// The two number domains share drafting, focus and commit behavior. Integer
/// seeds never pass through a floating-point conversion.
pub trait DraftValue: Copy + PartialEq + std::fmt::Display + 'static {
    fn clamp_value(self, min: Self, max: Self) -> Self;
    fn parse(draft: &str, min: Self, max: Self) -> Option<Self>;
    /// The value in the field's other unit: beats as hits per beat and back.
    /// Zero stays zero. Only a fractional domain has one.
    fn per(self) -> Self {
        self
    }
    /// A draft typed in the other unit, as a value in this one. `None` reverts.
    fn parse_per(_draft: &str) -> Option<Self> {
        None
    }
    /// Whether the value reads better in the other unit: less than one beat,
    /// above zero.
    fn fractional(self) -> bool {
        false
    }
}
impl DraftValue for f64 {
    fn clamp_value(self, min: Self, max: Self) -> Self {
        self.clamp(min, max)
    }
    fn parse(draft: &str, min: Self, max: Self) -> Option<Self> {
        parse_draft(draft, min, max)
    }
    fn per(self) -> Self {
        if self == 0. {
            0.
        } else {
            self.recip()
        }
    }
    fn parse_per(draft: &str) -> Option<Self> {
        parse_draft(draft, 0., f64::MAX).map(f64::per)
    }
    fn fractional(self) -> bool {
        self > 0. && self < 1.
    }
}
impl DraftValue for u64 {
    fn clamp_value(self, min: Self, max: Self) -> Self {
        self.clamp(min, max)
    }
    fn parse(draft: &str, min: Self, max: Self) -> Option<Self> {
        draft.trim().parse::<Self>().ok().map(|v| v.clamp(min, max))
    }
}

/// What the field tells its host: a draft became a number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumberEvent<T = f64> {
    Committed(T),
}

/// The entity. Emits [`NumberEvent::Committed`] only when a commit lands on a
/// *different* value — enter-then-blur is one commit, not two.
pub struct DraftedNumber<T: DraftValue = f64> {
    /// Names the field's automation node, so two number cells in one strip
    /// stay tellable apart — the same contract as a slider's id.
    id: SharedString,
    input: Entity<TextInput>,
    value: T,
    min: T,
    max: T,
    width: f32,
    /// A dim unit shown after the digits: "%", "beats".
    unit: Option<&'static str>,
    /// The other unit a press on the unit switches to, and whether the field
    /// shows it now: "per beat" for a field in beats.
    per: Option<(&'static str, bool)>,
    _subs: [Subscription; 2],
}

impl<T: DraftValue> EventEmitter<NumberEvent<T>> for DraftedNumber<T> {}

impl<T: DraftValue> DraftedNumber<T> {
    pub fn new(
        id: impl Into<SharedString>,
        value: T,
        min: T,
        max: T,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value = value.clamp_value(min, max);
        let input = cx.new(|cx| {
            let mut input = TextInput::search("", cx);
            input.set_text(value.to_string(), cx);
            input
        });
        // Blur is a commit point. The subscription lives on this entity, so a
        // dropped editor stops listening with it.
        let this = cx.entity().downgrade();
        let handle = input.focus_handle(cx);
        let focus_out = window.on_focus_out(&handle, cx, move |_, window, cx| {
            this.update(cx, |editor, cx| editor.commit(window, cx)).ok();
        });
        // A press elsewhere commits at once; focus-out may come a frame later.
        // Enter and escape come from the draft context the field declares.
        let keys = cx.subscribe_in(&input, window, |editor, _, event, window, cx| {
            match event {
                text_input::Event::Blurred | text_input::Event::Submitted => {
                    editor.commit(window, cx);
                }
                text_input::Event::Cancelled => editor.revert(cx),
                _ => {}
            }
            // Whether there is a draft decides the key context, so any change
            // to the text re-renders the field.
            cx.notify();
        });
        let _subs = [focus_out, keys];
        Self {
            id: id.into(),
            input,
            value,
            min,
            max,
            width,
            unit: None,
            per: None,
            _subs,
        }
    }

    /// Show `unit` after the digits, inside the field.
    #[must_use]
    pub fn with_unit(mut self, unit: &'static str) -> Self {
        self.unit = Some(unit);
        self
    }

    /// Show a value below one as its reciprocal in `unit`, as 0.0625 beats
    /// shows as 16 per beat. A press on the unit switches between the two.
    /// The stored value stays in the first unit.
    #[must_use]
    pub fn with_per_unit(mut self, unit: &'static str, cx: &mut Context<Self>) -> Self {
        self.per = Some((unit, false));
        self.pick_unit(cx);
        self
    }

    /// Show the value in the unit that suits it.
    fn pick_unit(&mut self, cx: &mut Context<Self>) {
        let fractional = self.value.fractional();
        if let Some((_, on)) = &mut self.per {
            *on = fractional;
            let text = self.shown();
            self.input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    /// The draft text of the committed value in the unit the field shows.
    fn shown(&self) -> String {
        match self.per {
            Some((_, true)) => format_per(self.value.per()),
            _ => self.value.to_string(),
        }
    }

    fn toggle_unit(&mut self, cx: &mut Context<Self>) {
        if let Some((_, on)) = &mut self.per {
            *on = !*on;
            let text = self.shown();
            self.input.update(cx, |input, cx| input.set_text(text, cx));
            cx.notify();
        }
    }

    #[must_use]
    pub fn value(&self) -> T {
        self.value
    }

    /// A host-side write. Stomps any draft in progress — the host is asserting
    /// the value moved under the field, and a draft over a stale value is the
    /// worse thing to keep.
    pub fn set_value(&mut self, value: T, cx: &mut Context<Self>) {
        let value = value.clamp_value(self.min, self.max);
        let changed = value != self.value;
        self.value = value;
        // A press on the unit holds until the value moves.
        if changed {
            self.pick_unit(cx);
        }
        let text = self.shown();
        self.input.update(cx, |input, cx| input.set_text(text, cx));
        cx.notify();
    }

    fn commit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let draft = self.input.read(cx).text().to_string();
        // A rounded reciprocal left as it was keeps the exact value.
        if draft == self.shown() {
            return;
        }
        let parsed = match self.per {
            Some((_, true)) => {
                T::parse_per(&draft).map(|value| value.clamp_value(self.min, self.max))
            }
            _ => T::parse(&draft, self.min, self.max),
        };
        match parsed {
            Some(value) => {
                let changed = value != self.value;
                self.value = value;
                if changed {
                    self.pick_unit(cx);
                }
                let text = self.shown();
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
        let text = self.shown();
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }
}

impl<T: DraftValue> Focusable for DraftedNumber<T> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl<T: DraftValue> Render for DraftedNumber<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reading = format!("{} = {}", self.id, self.input.read(cx).text());
        let focused = self.input.focus_handle(cx).is_focused(window);
        // While there is a draft, the draft context gives the field enter and
        // escape ahead of every binding in the surface around it. A clean
        // field has nothing to submit or drop, so its escape still closes the
        // dialog it sits in.
        let drafting = self.input.read(cx).text() != self.shown();
        float::field()
            .when(drafting, |field| field.key_context(DRAFT_CONTEXT))
            .w(px(self.width))
            // Never wider than its column: a field nested under a source's
            // rule gives up the rule's indent.
            .max_w_full()
            .font_family(crate::fonts::MONO)
            .gap(px(4.))
            .child(div().flex_1().min_w_0().child(self.input.clone()))
            .when_some(self.unit, |field, unit| {
                let label = div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(crate::ladder::foreground_alpha(0.45));
                match self.per {
                    None => field.child(label.child(unit)),
                    Some((per, on)) => {
                        let shown = if on { per } else { unit };
                        field.child(
                            label
                                .id(ElementId::Name(format!("{}:unit", self.id).into()))
                                .cursor_pointer()
                                .hover(|label| {
                                    label.text_color(crate::ladder::foreground_alpha(0.75))
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_unit(cx)))
                                .child(shown)
                                .agent_node(Role::Button, format!("{}: {shown}", self.id)),
                        )
                    }
                }
            })
            .agent_node(Role::Input, reading)
            .agent_focused(focused)
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

    /// Beats show as hits per beat, rounded, and zero stays zero.
    #[test]
    fn reciprocals_read_as_rates() {
        assert_eq!(format_per(0.0625_f64.per()), "16");
        assert_eq!(format_per((1. / 3.0_f64).per()), "3");
        assert_eq!(format_per(0.3_f64.per()), "3.3333");
        assert_eq!(format_per(0.0_f64.per()), "0");
        assert_eq!(32.0_f64.per().per(), 32.);
    }

    #[test]
    fn integer_drafts_preserve_every_bit_and_revert_invalid_values() {
        for value in [0, 9_007_199_254_740_993, u64::MAX - 1, u64::MAX] {
            assert_eq!(u64::parse(&value.to_string(), 0, u64::MAX), Some(value));
        }
        assert_eq!(u64::parse(" 42 ", 0, 100), Some(42));
        assert_eq!(u64::parse("101", 0, 100), Some(100));
        for draft in ["", "-1", "1.5", "NaN", "18446744073709551616"] {
            assert_eq!(u64::parse(draft, 0, u64::MAX), None);
        }
    }
}
