//! The "About you" page: two questions an account is asked once, after it
//! has accepted the terms and before the app opens for it.
//!
//! Part of the gate, like [`crate::terms`], and laid out the same way. It is
//! not a condition of entry: either question can be left open, and with
//! nothing taken the page's one capsule is Skip. The server's row is the record, and it records a skip too,
//! so nobody is asked twice.
//!
//! An account that was signed in before the page existed is not interrupted.
//! Each launch asks the server, behind the opening app, whether the account
//! has been asked, and leaves [`PENDING`] if it has not: the next launch
//! shows the page first.

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_lib::dispatch::handlers::auth::ProfileBackground;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::TextInput;
use luma_ui::{ladder, pill, Enabled};

use crate::{keymap, signin, Luma};

/// The session item, per account, that says the page is still owed.
pub(crate) const PENDING: &str = "about_you_pending";

const TITLE: &str = "About you";
const TITLE_SIZE: f32 = 28.0;
/// Why the page asks. It promises nothing the app does not do.
const LEAD: &str = "This helps us understand who uses Luma and make it better for people like you.";

const REFUSED: &str = "Could not record that. Check your connection and try again.";

/// The role that opens the field for the account's own word.
const OTHER: &str = "other";
/// What the field for it asks.
const OTHER_PROMPT: &str = "What do you do?";
/// How much of its own word the server keeps.
const OTHER_LIMIT: usize = 80;

/// One question: what it asks, whether more than one answer fits, and its
/// answers as the server's word and the label a person reads. The words are
/// the ones `supabase/migrations/20261113120000_provenance.sql` accepts.
struct Question {
    asks: &'static str,
    many: bool,
    answers: &'static [(&'static str, &'static str)],
}

/// The questions, in the order they are asked: what the person does first,
/// because how much of it they have done only means something next to that.
/// The second is one scale, of how often, with its top "it's my job".
const QUESTIONS: [Question; 2] = [
    Question {
        asks: "What describes you? Pick any that fit.",
        many: true,
        answers: &[
            ("lighting_designer", "Lighting designer"),
            ("dj", "DJ"),
            ("musician", "Musician"),
            ("venue_tech", "Venue tech"),
            ("hobbyist", "Hobbyist"),
            (OTHER, "Other"),
        ],
    },
    Question {
        asks: "How often have you programmed lights?",
        many: false,
        answers: &[
            ("none", "Never"),
            ("some", "A few times"),
            ("regular", "Often"),
            ("professional", "It's my job"),
        ],
    },
];

/// The page while it is up.
pub(crate) struct AboutYou {
    /// The answers taken to each of [`QUESTIONS`], as the server's words.
    /// Nothing is taken until the person takes it.
    chosen: [Vec<&'static str>; 2],
    /// The account's own word for [`OTHER`].
    other: Entity<TextInput>,
    /// The answer is on its way to the server.
    busy: bool,
    error: Option<SharedString>,
    /// The account has just signed in, so its library is still on its way
    /// down and the venue waits for it.
    after_sign_in: bool,
    /// Where the keyboard rests, so the page has a dispatch path to be on.
    screen_focus: FocusHandle,
    focus_pending: bool,
}

impl AboutYou {
    fn answered(&self) -> bool {
        self.chosen.iter().any(|taken| !taken.is_empty())
    }

    /// Take `word` for question `index`, or let go of it if it is held. A
    /// question with one answer lets go of the other first.
    fn toggle(&mut self, index: usize, word: &'static str) {
        let taken = &mut self.chosen[index];
        if let Some(held) = taken.iter().position(|&held| held == word) {
            taken.remove(held);
        } else {
            if !QUESTIONS[index].many {
                taken.clear();
            }
            taken.push(word);
        }
    }

    /// The answers as the server takes them.
    fn background(&self, cx: &App) -> ProfileBackground {
        let other = self.other.read(cx).text().trim();
        ProfileBackground {
            roles: self.chosen[0].iter().map(|&word| word.to_owned()).collect(),
            role_other: (self.chosen[0].contains(&OTHER) && !other.is_empty())
                .then(|| other.chars().take(OTHER_LIMIT).collect()),
            experience: self.chosen[1].first().map(|&word| word.to_owned()),
        }
    }
}

impl Luma {
    /// The terms are settled for an account that has just signed in: ask the
    /// server whether it has been asked about itself, and ask it if not. An
    /// answer that never arrives lets the account through; the next launch
    /// asks again.
    pub(crate) fn enter_past_terms(&mut self, cx: &mut Context<Self>) {
        self.restoring_venue = true;
        let asked = self.library.about_you_asked();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let asked = asked.await;
            this.update(cx, |this, cx| {
                if this.sign_in.is_some() {
                    this.restoring_venue = false;
                    return;
                }
                match asked {
                    Ok(false) => this.show_about_you(true, cx),
                    Ok(true) => this.restore_venue_once_synced(cx),
                    Err(error) => {
                        log::warn!(
                            "could not ask whether the account was asked about itself: {error}"
                        );
                        this.restore_venue_once_synced(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// A launch for a stored session went straight to the venue. Behind it,
    /// find out whether the account still owes the page, and leave the mark
    /// that shows it at the next launch.
    pub(crate) fn note_about_you_owed(&mut self, cx: &mut Context<Self>) {
        let key = self.account_key(PENDING);
        let asked = self.library.about_you_asked();
        cx.spawn(async move |this, cx| {
            if !matches!(asked.await, Ok(false)) {
                return;
            }
            let Ok(marked) = this.update(cx, |this, _| this.library.set_session_item(&key, "1"))
            else {
                return;
            };
            if let Err(error) = marked.await {
                log::warn!("could not mark the profile questions as owed: {error}");
            }
        })
        .detach();
    }

    pub(crate) fn show_about_you(&mut self, after_sign_in: bool, cx: &mut Context<Self>) {
        self.restoring_venue = false;
        self.about_you = Some(AboutYou {
            chosen: [Vec::new(), Vec::new()],
            other: cx.new(|cx| TextInput::search(OTHER_PROMPT, cx)),
            busy: false,
            error: None,
            after_sign_in,
            screen_focus: cx.focus_handle(),
            focus_pending: true,
        });
        cx.notify();
    }

    /// Leave the page for the account's venue, the mark that owed it gone.
    fn leave_about_you(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.about_you.take() else {
            return;
        };
        let clear = self.library.remove_session_item(&self.account_key(PENDING));
        cx.spawn(async move |_, _| {
            if let Err(error) = clear.await {
                log::warn!("could not clear the owed profile questions: {error}");
            }
        })
        .detach();
        if state.after_sign_in {
            self.restore_venue_once_synced(cx);
        } else {
            self.restore_venue(cx);
        }
    }

    /// Send the answers, or with `skip` none of them. A skip that cannot be
    /// recorded still lets the account in: the page is not a condition of
    /// entry, and the server is asked again at the next launch.
    fn answer_about_you(&mut self, skip: bool, cx: &mut Context<Self>) {
        let Some(state) = self.about_you.as_mut() else {
            return;
        };
        if state.busy {
            return;
        }
        state.busy = true;
        state.error = None;
        let background = if skip {
            ProfileBackground::default()
        } else {
            state.background(cx)
        };
        // Whose answer this is: a sign-out raises the gate, and the next
        // account's page must not be closed by this one's.
        let generation = self.sign_in_generation;
        let pending = self.library.answer_about_you(background);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if this.sign_in_generation != generation || this.about_you.is_none() {
                    return;
                }
                match result {
                    Ok(()) => this.leave_about_you(cx),
                    Err(error) => {
                        log::warn!("could not record the profile answers: {error}");
                        if skip {
                            return this.leave_about_you(cx);
                        }
                        if let Some(state) = this.about_you.as_mut() {
                            state.busy = false;
                            state.error = Some(REFUSED.into());
                        }
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

/// The whole window while the questions are up: the title and why it asks,
/// the two questions, and the one way off the page. Everything is the gate's
/// capsule width, so the page is one column.
pub(crate) fn screen(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let Some(state) = app.about_you.as_mut() else {
        return div().into_any_element();
    };
    if std::mem::take(&mut state.focus_pending) {
        window.focus(&state.screen_focus, cx);
    }
    let answered = state.answered();
    let names_other = state.chosen[0].contains(&OTHER);
    // The field's name is its value once it has one, as the sign-in's is.
    let other = state.other.read(cx);
    let other_label: SharedString = if other.is_empty() {
        OTHER_PROMPT.into()
    } else {
        other.text().to_owned().into()
    };
    let other_focused = other.focus_handle(cx).is_focused(window);
    let questions = QUESTIONS.iter().enumerate().map(|(index, question)| {
        let choices = question.answers.iter().map(|&(word, label)| {
            let app = entity.clone();
            pill::choice(label, state.chosen[index].contains(&word))
                .id(SharedString::from(format!("about-you-{word}")))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        if let Some(state) = this.about_you.as_mut() {
                            state.toggle(index, word);
                            cx.notify();
                        }
                    });
                })
                .agent_node(Role::Toggle, label)
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .text_size(px(14.5))
                    .font_weight(FontWeight::MEDIUM)
                    .child(question.asks)
                    .agent_node(Role::Text, question.asks),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(8.0))
                    .children(choices),
            )
            .when(index == 0 && names_other, |column| {
                column.child(
                    pill::field()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(luma_ui::text_input::TEXT_SIZE))
                                .child(state.other.clone()),
                        )
                        .agent_node(Role::Input, other_label.clone())
                        .agent_focused(other_focused),
                )
            })
    });
    // One way off the page, which says what it will do: nothing taken is a
    // skip, and a quiet capsule; an answer is the page's one lit capsule.
    let leave = entity.clone();
    let leaving = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        leave.update(cx, |this, cx| this.answer_about_you(!answered, cx));
    };
    let way_off = if answered {
        let label = if state.busy { "Saving…" } else { "Continue" };
        pill::primary(label, Enabled::from(!state.busy))
            .id("about-you-leave")
            .on_click(leaving)
            .agent_node(Role::Button, "Continue")
            .agent_disabled(state.busy)
            .into_any_element()
    } else {
        pill::secondary("Skip")
            .id("about-you-leave")
            .on_click(leaving)
            .agent_node(Role::Button, "Skip")
            .agent_disabled(state.busy)
            .into_any_element()
    };
    let column = div()
        .flex_1()
        .min_h_0()
        .py(px(pill::HEAD_GAP))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(pill::HEAD_GAP))
        .child(
            div()
                .w(px(pill::WIDTH))
                .max_w_full()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .text_size(px(TITLE_SIZE))
                        .font_weight(FontWeight::NORMAL)
                        .child(TITLE)
                        .agent_node(Role::Text, TITLE),
                )
                .child(
                    div()
                        .text_size(px(13.5))
                        .line_height(px(20.0))
                        .text_color(ladder::foreground_alpha(0.62))
                        .child(LEAD),
                ),
        )
        .child(
            div()
                .w(px(pill::WIDTH))
                .max_w_full()
                .flex()
                .flex_col()
                .gap(px(26.0))
                .children(questions),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(pill::GAP))
                .when_some(state.error.clone(), |column, error| {
                    column.child(luma_ui::notice::line(error))
                })
                .child(way_off),
        );
    signin::ground(window, column)
        .key_context(keymap::context::SIGN_IN)
        .track_focus(&state.screen_focus)
        .into_any_element()
}
