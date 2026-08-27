//! The group-expression editor: one line of selection algebra, highlighted in
//! place, autocompleted from the venue's group names, committed only whole.
//!
//! # Commit on Enter/blur only
//!
//! Evaluating a selection expression is expensive and a half-typed one is
//! invalid, so the host hears [`ExpressionEvent::Committed`] and nothing else.
//! The web version (src/features/universe/components/group-expression-editor
//! .tsx) draws a transparent `<input>` over a highlighted overlay — two
//! renderings of one string that must stay pixel-aligned. Here the field's own
//! shaper colors the tokens ([`TextInput::set_highlight`]), so there is one
//! rendering and nothing to keep aligned.
//!
//! # Token colors are the ladder's
//!
//! Group names take [`ladder::status_warn`]'s amber and operators
//! [`ladder::status_bad`]'s rose — the nearest rungs to the web side's
//! amber-400/rose-400, not new hues — and parens take [`ladder::param_label`],
//! which *is* the web's gray-400. Nothing here mints a color.

use std::ops::Range;

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, KeyDownEvent,
    SharedString, Subscription, Window,
};

use crate::float::Picker;
use crate::float::{self, RowState};
use crate::node::{Instrument, Role};
use crate::text_input::TextInput;
use crate::{fonts, ladder};

use crate::CONTROL_HEIGHT;

// -- tokenizer ---------------------------------------------------------------

/// What a span of the expression is. `Text` is anything the grammar doesn't
/// claim — unknown words, whitespace, stray characters — and draws in the
/// field's own ink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A known group name, or `all`.
    Group,
    /// `> | ^ & ~`
    Operator,
    /// `(` or `)`
    Paren,
    Text,
}

/// One colored span. Ranges are byte offsets into the tokenized string,
/// contiguous and covering it entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

const OPERATORS: [char; 5] = ['>', '|', '^', '&', '~'];

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Split an expression into colored spans. `is_group` answers for a word
/// (case-insensitively is the caller's choice); `all` is a group by grammar.
pub fn tokenize(text: &str, is_group: impl Fn(&str) -> bool) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut push = |range: Range<usize>, kind: TokenKind| {
        // Adjacent spans of one kind merge, so the shaper sees runs, not chars.
        if let Some(last) = tokens.last_mut() {
            if last.kind == kind && last.range.end == range.start {
                last.range.end = range.end;
                return;
            }
        }
        tokens.push(Token { range, kind });
    };
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if OPERATORS.contains(&c) {
            push(at..at + c.len_utf8(), TokenKind::Operator);
        } else if c == '(' || c == ')' {
            push(at..at + c.len_utf8(), TokenKind::Paren);
        } else if is_word(c) {
            let mut end = at + c.len_utf8();
            while let Some(&(next, nc)) = chars.peek() {
                if !is_word(nc) {
                    break;
                }
                end = next + nc.len_utf8();
                chars.next();
            }
            let word = &text[at..end];
            let lower = word.to_lowercase();
            let kind = if lower == "all" || is_group(&lower) {
                TokenKind::Group
            } else {
                TokenKind::Text
            };
            push(at..end, kind);
        } else {
            push(at..at + c.len_utf8(), TokenKind::Text);
        }
    }
    tokens
}

/// The ladder rung a token kind draws in — see the module docs.
#[must_use]
pub fn token_color(kind: TokenKind) -> Hsla {
    match kind {
        TokenKind::Group => ladder::status_warn().into(),
        TokenKind::Operator => ladder::status_bad().into(),
        TokenKind::Paren => ladder::param_label().into(),
        TokenKind::Text => ladder::foreground().into(),
    }
}

// -- autocomplete ------------------------------------------------------------

/// Byte range of the word the cursor is in (or touching); empty at the cursor
/// when it touches no word. What a suggestion replaces.
#[must_use]
pub fn current_word(text: &str, cursor: usize) -> Range<usize> {
    let cursor = cursor.min(text.len());
    let start = text[..cursor]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(cursor, |(at, _)| at);
    let end = cursor
        + text[cursor..]
            .char_indices()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map_or(0, |(at, c)| at + c.len_utf8());
    start..end
}

/// How many rows the menu shows, as on the web side.
pub const SUGGESTION_CAP: usize = 10;

/// The suggestion match rule the editor's [`Picker`] is built over: a
/// case-insensitive prefix on the option name. (An empty prefix suggests
/// everything — the "what can I type here" case — which the picker already
/// treats as no query.)
fn suggests(option: &SharedString, prefix: &str) -> bool {
    option.to_lowercase().starts_with(&prefix.to_lowercase())
}

// -- the entity --------------------------------------------------------------

/// What the editor tells its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionEvent {
    /// Enter (with no suggestion menu claiming it) or blur landed a changed
    /// expression. Fires only when the text differs from the last commit.
    Committed(String),
}

/// The entity: a search-mode [`TextInput`] with the tokenizer wired into its
/// shaper, plus the suggestion menu and its keyboard.
///
/// Search mode is the load-bearing choice: its keymap deliberately leaves the
/// bare arrows, `enter`, `tab` and `escape` unbound, so the menu's keyboard
/// lives here as plain key events instead of fighting the field's bindings.
pub struct GroupExpressionEditor {
    input: Entity<TextInput>,
    /// The suggestion loop over the group vocabulary — `all` first, then the
    /// group names as given. Its query is re-derived from the caret's word on
    /// every render; [`Picker`] ignores the no-change case, so the arrow keys'
    /// cursor survives a redraw.
    picker: Picker<SharedString>,
    focused: bool,
    committed: String,
    width: f32,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ExpressionEvent> for GroupExpressionEditor {}

impl GroupExpressionEditor {
    pub fn new(
        groups: impl IntoIterator<Item = SharedString>,
        initial: impl Into<String>,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = initial.into();
        let input = cx.new(|cx| {
            let mut input = TextInput::search("e.g. front_wash | back_movers", cx);
            input.set_text(initial.clone(), cx);
            input
        });
        let handle = input.focus_handle(cx);
        let this_in = cx.entity().downgrade();
        let this_out = cx.entity().downgrade();
        let subs = vec![
            // Re-render on every edit/caret move: the menu filters live.
            cx.subscribe(&input, |_, _, _, cx| cx.notify()),
            window.on_focus_in(&handle, cx, move |_, cx| {
                this_in
                    .update(cx, |editor, cx| {
                        editor.focused = true;
                        editor.picker.rewind();
                        cx.notify();
                    })
                    .ok();
            }),
            window.on_focus_out(&handle, cx, move |_, _, cx| {
                this_out
                    .update(cx, |editor, cx| {
                        editor.focused = false;
                        editor.commit(cx);
                        cx.notify();
                    })
                    .ok();
            }),
        ];
        let mut this = Self {
            input,
            picker: Picker::new(suggests).with_limit(SUGGESTION_CAP),
            focused: false,
            committed: initial,
            width,
            _subs: subs,
        };
        this.set_groups(groups, cx);
        this
    }

    /// The current (possibly uncommitted) text.
    #[must_use]
    pub fn text<'a>(&self, cx: &'a App) -> &'a str {
        self.input.read(cx).text()
    }

    /// Replace the expression from outside — a selection change re-pointing
    /// the strip. Not a commit; the host already knows this value.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        self.committed = text.clone();
        self.input.update(cx, |input, cx| input.set_text(text, cx));
        cx.notify();
    }

    /// Swap the group vocabulary: reseeds the autocomplete and rewires the
    /// highlighter, since both speak it.
    pub fn set_groups(
        &mut self,
        groups: impl IntoIterator<Item = SharedString>,
        cx: &mut Context<Self>,
    ) {
        let mut options: Vec<SharedString> = vec!["all".into()];
        options.extend(groups);
        let vocabulary: Vec<String> = options.iter().map(|g| g.to_lowercase()).collect();
        self.input.update(cx, |input, cx| {
            input.set_highlight(
                move |text| {
                    tokenize(text, |word| vocabulary.iter().any(|g| g == word))
                        .into_iter()
                        .filter(|token| token.kind != TokenKind::Text)
                        .map(|token| (token.range, token_color(token.kind)))
                        .collect()
                },
                cx,
            );
        });
        self.picker.set_rows(options);
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_string();
        if text != self.committed {
            self.committed = text.clone();
            cx.emit(ExpressionEvent::Committed(text));
        }
    }

    /// Point the picker's query at the word under the caret. Called wherever
    /// the text or caret may have moved; a no-change query costs nothing.
    fn requery(&mut self, cx: &App) {
        let input = self.input.read(cx);
        let word = current_word(input.text(), input.cursor_offset());
        let prefix = input.text()[word].to_string();
        self.picker.set_query(prefix);
    }

    fn apply_suggestion(&mut self, option: &SharedString, cx: &mut Context<Self>) {
        let (text, word) = {
            let input = self.input.read(cx);
            let text = input.text().to_string();
            let word = current_word(&text, input.cursor_offset());
            (text, word)
        };
        let replaced = format!("{}{}{}", &text[..word.start], option, &text[word.end..]);
        // `set_text` parks the caret at the end rather than after the inserted
        // token — a known coarseness; the field has no public caret setter and
        // growing one for this would widen its interface for a corner.
        self.input
            .update(cx, |input, cx| input.set_text(replaced, cx));
        self.picker.rewind();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.requery(cx);
        let live = self.focused && !self.picker.is_empty();
        match event.keystroke.key.as_str() {
            "down" if live => {
                self.picker.step(1);
            }
            "up" if live => {
                self.picker.step(-1);
            }
            "tab" | "enter" if live => {
                if let Some(option) = self.picker.current().cloned() {
                    self.apply_suggestion(&option, cx);
                }
            }
            "enter" => {
                self.commit(cx);
                window.blur();
            }
            "escape" => {
                let committed = self.committed.clone();
                self.input
                    .update(cx, |input, cx| input.set_text(committed, cx));
                window.blur();
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn menu(&self, live: &[SharedString], cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let cursor = self.picker.cursor();
        // Sized to the field, not the suggestions: a menu narrower than the
        // shell it hangs off reads as detached. An absolute width, the way
        // every float sizes itself.
        let card = live.iter().enumerate().fold(
            float::popover_card().min_w(px(self.width)),
            |menu, (index, option)| {
                let this = this.clone();
                let picked = option.clone();
                // `RowState` splits the two facts one paint used to blur:
                // hover marks the pointer, the cursor marks what Enter takes.
                let row = float::menu_row(
                    RowState::of(false, index == cursor),
                    format!("expr-suggestion:{option}"),
                )
                .id(index)
                .font_family(fonts::MONO)
                .text_size(px(12.))
                .text_color(ladder::foreground_90())
                .child(option.clone())
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    // Mouse-down, not click: a click waits for the
                    // mouse-up, and by then the field has blurred and
                    // the menu is gone — the web side ducks the same
                    // race with `onMouseDown`.
                    cx.stop_propagation();
                    this.update(cx, |editor, cx| {
                        editor.apply_suggestion(&picked, cx);
                        cx.notify();
                    });
                })
                .agent_node(Role::Button, option.to_string());
                menu.child(row)
            },
        );
        float::anchored_below(
            "expression-suggestions",
            CONTROL_HEIGHT,
            card.into_any_element(),
        )
    }
}

impl Focusable for GroupExpressionEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for GroupExpressionEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.requery(cx);
        let live: Vec<SharedString> = if self.focused {
            self.picker.shown().cloned().collect()
        } else {
            Vec::new()
        };
        let reading = format!("expression = {}", self.input.read(cx).text());
        let menu = (!live.is_empty()).then(|| self.menu(&live, cx));
        // The menu hangs off a wrapper *outside* the field's clip box: the
        // shell must clip its text, and a menu clipped with it would be a
        // one-row stub.
        div()
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .flex_shrink_0()
            .w(px(self.width))
            .child(
                float::field()
                    .w_full()
                    .font_family(fonts::MONO)
                    .child(div().w_full().child(self.input.clone())),
            )
            .children(menu)
            .agent_node(Role::Input, reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str, groups: &[&str]) -> Vec<(String, TokenKind)> {
        tokenize(text, |w| groups.contains(&w))
            .into_iter()
            .map(|t| (text[t.range].to_string(), t.kind))
            .collect()
    }

    /// The grammar's four inks, on a real expression: known groups and `all`
    /// are groups, the five operators are operators, parens are parens, and
    /// unknown words plus whitespace fall through to text.
    #[test]
    fn the_tokenizer_speaks_the_grammar() {
        let toks = kinds("front_wash | (all & ~back)", &["front_wash", "back"]);
        assert_eq!(
            toks,
            vec![
                ("front_wash".into(), TokenKind::Group),
                (" ".into(), TokenKind::Text),
                ("|".into(), TokenKind::Operator),
                (" ".into(), TokenKind::Text),
                ("(".into(), TokenKind::Paren),
                ("all".into(), TokenKind::Group),
                (" ".into(), TokenKind::Text),
                ("&".into(), TokenKind::Operator),
                (" ".into(), TokenKind::Text),
                ("~".into(), TokenKind::Operator),
                ("back".into(), TokenKind::Group),
                (")".into(), TokenKind::Paren),
            ]
        );
    }

    /// Group matching is case-insensitive and unknown words are plain text —
    /// an expression over groups that don't exist yet still renders honestly.
    #[test]
    fn unknown_words_are_text_and_case_does_not_matter() {
        let toks = kinds("Front_Wash | ghost", &["front_wash"]);
        assert_eq!(toks[0], ("Front_Wash".into(), TokenKind::Group));
        // The unknown word merges with the space before it — both are Text,
        // and adjacent same-kind spans coalesce into one run.
        assert_eq!(toks.last().unwrap(), &(" ghost".into(), TokenKind::Text));
    }

    /// Tokens cover the string contiguously — the shaper's precondition.
    #[test]
    fn tokens_tile_the_string() {
        let text = "a>b | (weird?? ^c)";
        let toks = tokenize(text, |_| false);
        let mut at = 0;
        for token in &toks {
            assert_eq!(token.range.start, at);
            at = token.range.end;
        }
        assert_eq!(at, text.len());
    }

    /// The word under the cursor, at its edges: inside, at either end, and in
    /// the gaps where there is no word to complete.
    #[test]
    fn the_current_word_tracks_the_cursor() {
        let text = "front | back";
        assert_eq!(current_word(text, 2), 0..5);
        assert_eq!(current_word(text, 0), 0..5);
        assert_eq!(current_word(text, 5), 0..5);
        assert_eq!(current_word(text, 6), 6..6);
        assert_eq!(current_word(text, 8), 8..12);
        assert_eq!(current_word(text, 12), 8..12);
        // Clamped, not panicking, past the end.
        assert_eq!(current_word(text, 99), 8..12);
    }

    /// Prefix filtering, through the picker exactly as the editor wires it:
    /// case-insensitive, order-preserving, capped, and an empty prefix offers
    /// the whole vocabulary.
    #[test]
    fn suggestions_filter_by_prefix() {
        let mut picker = Picker::new(suggests).with_limit(SUGGESTION_CAP);
        picker.set_rows(
            ["all", "back_movers", "back_wash", "front_wash"]
                .into_iter()
                .map(SharedString::from)
                .collect(),
        );
        let mut names = |prefix: &str| -> Vec<String> {
            picker.set_query(prefix);
            picker.shown().map(ToString::to_string).collect()
        };
        assert_eq!(names("ba"), vec!["back_movers", "back_wash"]);
        assert_eq!(names("BACK_W"), vec!["back_wash"]);
        assert_eq!(
            names(""),
            vec!["all", "back_movers", "back_wash", "front_wash"]
        );
        assert_eq!(names("zzz"), Vec::<String>::new());

        let mut many = Picker::new(suggests).with_limit(SUGGESTION_CAP);
        many.set_rows(
            (0..30)
                .map(|i| SharedString::from(format!("group_{i:02}")))
                .collect(),
        );
        many.set_query("group");
        assert_eq!(many.shown().count(), SUGGESTION_CAP);
    }
}
