//! The terms page: what an account accepts once, before the app opens for it.
//!
//! Part of the gate, like [`crate::signin`]: a state of the app on the bare
//! page, with nothing behind it. It follows a verified code when the server
//! holds no acceptance for the account — a new account, in practice — and
//! leaves when the server has recorded one.
//!
//! The server's row is the record. This device keeps only [`PENDING`], which
//! says the signed-in account was shown the page and has not accepted, so
//! quitting on the page and launching again lands back on it rather than in
//! the app. An account that signed in before the page existed has no such
//! item and is not asked until it signs in again.

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{glass, ladder, pill, radius, Enabled};

use crate::{keymap, signin, Luma};

/// The version of [`SECTIONS`] an acceptance is recorded against. Raise it
/// when the text changes in a way people should be asked about again.
const VERSION: i32 = 1;

/// The session item, per account, that says the page is still owed.
const PENDING: &str = "terms_pending";

/// Where complaints, deletion requests and questions go.
macro_rules! contact {
    () => {
        "legal@luma.show"
    };
}

const TITLE: &str = "Terms of Service";
const TITLE_SIZE: f32 = 28.0;

/// The text box: wider than the capsules under it, because it holds
/// paragraphs, and no taller than a page someone would scroll.
const BOX_WIDTH: f32 = 600.0;
const BOX_MAX_HEIGHT: f32 = 560.0;

const REFUSED: &str = "Could not record that. Check your connection and try again.";

/// The terms, as headed sections of paragraphs.
const SECTIONS: &[(&str, &[&str])] = &[
    (
        "Using Luma",
        &[
            "These terms are an agreement between you and Luma. By pressing Accept you agree to them.",
            "You must be 18 or older to use Luma. If you use Luma for a venue, company or other organization, you confirm that you may accept these terms for it.",
            "Keep your sign-in email secure. You are responsible for what happens under your account.",
        ],
    ),
    (
        "Your content",
        &[
            "You own the scores, venues and other content you make in Luma.",
            "You grant Luma a license to host, process and use that content to run and improve Luma, including to train models. For content already used in training, the license continues after you delete the content or close your account.",
        ],
    ),
    (
        "Audio",
        &[
            "You confirm that you have the right to upload any audio you upload, and to share it with anyone you share it with.",
            "Luma may process uploaded audio, and may train on it and on data derived from it.",
        ],
    ),
    (
        "Sharing",
        &[
            "Sharing is by invitation only, and you are responsible for who you invite. People you invite can see, use and copy what you share with them, and may keep copies after you stop sharing.",
        ],
    ),
    (
        "AI features",
        &[
            "Agent chats and score content are sent to third-party AI providers so they can be processed.",
            "You may use what the model and the agent generate for you, including commercially. Generated output can be wrong, and other people may receive similar output. Check it before you rely on it in a show.",
        ],
    ),
    (
        "Hardware and safety",
        &[
            "You are responsible for the safe and legal operation of all hardware you connect to Luma. That includes laser safety and permits, strobe use, and warning your audience.",
            "Luma's own display can flash and strobe. Take care if you or people near your screen are sensitive to flashing light.",
            "You must not use Luma to control pyrotechnics, flame, hoists, motorized rigging or any other device that could injure someone if it failed or fired at the wrong time.",
        ],
    ),
    (
        "No warranty",
        &[
            "Luma is provided as is, with no warranty of any kind. It may fail, including during a show, and you should keep a manual fallback.",
            "Content can be lost. Keep your own copies of anything you cannot afford to lose.",
            "Luma updates itself, and features may change or be removed.",
        ],
    ),
    (
        "Liability",
        &[
            "Luma's liability to you is limited to the maximum extent the law allows.",
            "If you break these terms, you will cover the costs and losses Luma incurs as a result, including claims other people bring.",
        ],
    ),
    (
        "Removal and closing accounts",
        &[
            concat!(
                "Luma may remove content and may suspend or close accounts, including in response to copyright complaints and for accounts that infringe repeatedly. Send copyright complaints to ",
                contact!(),
                "."
            ),
            "You may stop using Luma at any time, and you can ask for your account to be closed.",
            "The license for content already used in training, the limits on liability and your duty to cover Luma's costs continue after your account is closed.",
        ],
    ),
    (
        "Your data",
        &[
            "Luma collects the email address you sign in with, the content you make and upload, and the technical records needed to run and sync the service.",
            concat!(
                "Luma does not sell your data. You can ask for your account and data to be deleted by writing to ",
                contact!(),
                "."
            ),
        ],
    ),
    (
        "Changes",
        &[
            "These terms can change. Luma will give you notice of a new version, and continuing to use Luma after you accept it means you agree to it.",
        ],
    ),
    (
        "General",
        &[
            "California law applies to these terms.",
            "If part of these terms cannot be enforced, the rest still applies. These terms are the whole agreement between you and Luma about the service.",
            concat!("Questions about these terms go to ", contact!(), "."),
        ],
    ),
];

/// The page while it is up.
pub(crate) struct Terms {
    /// The acceptance is on its way to the server.
    busy: bool,
    error: Option<SharedString>,
    /// Where the keyboard rests, so the page has a dispatch path to be on.
    screen_focus: FocusHandle,
    accept_focus: FocusHandle,
    sign_out_focus: FocusHandle,
    focus_pending: bool,
}

impl Luma {
    /// Open the app for the account a stored session proves: the terms page
    /// if this device still owes it one, its venue otherwise.
    pub(crate) fn enter(&mut self, cx: &mut Context<Self>) {
        self.restoring_venue = true;
        let pending = self.library.get_session_item(&self.account_key(PENDING));
        cx.notify();
        cx.spawn(async move |this, cx| {
            // A store that cannot be read owes nothing: the server was asked
            // at the sign-in, and a launch must not hang on this.
            let owed = pending.await.ok().flatten().is_some();
            this.update(cx, |this, cx| {
                if this.left_the_gate() {
                    return;
                }
                if owed {
                    this.show_terms(cx);
                } else {
                    this.restore_venue(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Open the app for an account that has just signed in: ask the server
    /// whether it has accepted the terms, and show them if it has not.
    ///
    /// The page is marked owed before the server is asked, so there is no
    /// moment at which quitting skips it. An answer that never arrives is
    /// treated as "not accepted": accepting twice is harmless, and passing
    /// someone who never accepted is not.
    pub(crate) fn enter_after_sign_in(&mut self, cx: &mut Context<Self>) {
        self.restoring_venue = true;
        let key = self.account_key(PENDING);
        let marked = self.library.set_session_item(&key, "1");
        let asked = self.library.terms_accepted();
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = marked.await {
                log::warn!("could not mark the terms as owed: {error}");
            }
            let accepted = asked.await;
            this.update(cx, |this, cx| {
                if this.left_the_gate() {
                    return;
                }
                match accepted {
                    Ok(true) => this.terms_settled(&key, cx),
                    Ok(false) => this.show_terms(cx),
                    Err(error) => {
                        log::warn!("could not ask whether the terms were accepted: {error}");
                        this.show_terms(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Whether the account these steps were opening the app for has since
    /// been signed out, which hands the window back to the sign-in screen.
    fn left_the_gate(&mut self) -> bool {
        if self.sign_in.is_some() {
            self.restoring_venue = false;
            return true;
        }
        false
    }

    fn show_terms(&mut self, cx: &mut Context<Self>) {
        self.restoring_venue = false;
        self.terms = Some(Terms {
            busy: false,
            error: None,
            screen_focus: cx.focus_handle(),
            accept_focus: cx.focus_handle().tab_stop(true),
            sign_out_focus: cx.focus_handle().tab_stop(true),
            focus_pending: true,
        });
        cx.notify();
    }

    /// The server holds this account's acceptance: forget that the page was
    /// owed, and go on to the account's venue.
    fn terms_settled(&mut self, key: &str, cx: &mut Context<Self>) {
        self.terms = None;
        let clear = self.library.remove_session_item(key);
        cx.spawn(async move |_, _| {
            if let Err(error) = clear.await {
                log::warn!("could not clear the owed terms: {error}");
            }
        })
        .detach();
        self.restore_venue_once_synced(cx);
    }

    fn accept_terms(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.terms.as_mut() else {
            return;
        };
        if state.busy {
            return;
        }
        state.busy = true;
        state.error = None;
        let key = self.account_key(PENDING);
        // Whose acceptance this is: a sign-out raises the gate, and the next
        // account's page must not be closed by this one's answer.
        let generation = self.sign_in_generation;
        let pending = self.library.accept_terms(VERSION);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if this.sign_in_generation != generation || this.terms.is_none() {
                    return;
                }
                match result {
                    Ok(()) => this.terms_settled(&key, cx),
                    Err(error) => {
                        log::warn!("could not record the terms acceptance: {error}");
                        if let Some(state) = this.terms.as_mut() {
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

/// The whole window while the terms are owed: the title, the text in its
/// scrolling box, and the two ways off the page.
pub(crate) fn screen(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let signing_out = app.account_action.signing_out;
    let Some(state) = app.terms.as_mut() else {
        return div().into_any_element();
    };
    if std::mem::take(&mut state.focus_pending) {
        window.focus(&state.screen_focus, cx);
    }
    let (accept, sign_out, keys) = (entity.clone(), entity.clone(), entity);
    let accept_label = if state.busy { "Accepting…" } else { "Accept" };
    let sign_out_label = if signing_out {
        "Signing out…"
    } else {
        "Sign out"
    };
    let accept_focus = state.accept_focus.clone();
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
                .flex_none()
                .text_size(px(TITLE_SIZE))
                .font_weight(FontWeight::NORMAL)
                .child(TITLE)
                .agent_node(Role::Text, TITLE),
        )
        .child(text_box())
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
                .child(
                    pill::primary(accept_label, Enabled::from(!state.busy))
                        .id("terms-accept")
                        .when(!state.busy, |capsule| {
                            capsule.track_focus(&state.accept_focus).tab_index(0)
                        })
                        .on_click(move |_, _, cx| {
                            accept.update(cx, |this, cx| this.accept_terms(cx));
                        })
                        .agent_node(Role::Button, "Accept")
                        .agent_focused(state.accept_focus.is_focused(window)),
                )
                // Declining is leaving: the account stays signed out of this
                // device until someone accepts for it.
                .child(
                    pill::link(sign_out_label)
                        .id("terms-sign-out")
                        .track_focus(&state.sign_out_focus)
                        .tab_index(0)
                        .on_click(move |_, _, cx| {
                            sign_out.update(cx, |this, cx| this.sign_out(cx));
                        })
                        .agent_node(Role::Button, "Sign out")
                        .agent_focused(state.sign_out_focus.is_focused(window)),
                ),
        );
    signin::ground(window, column)
        .key_context(keymap::context::SIGN_IN)
        .track_focus(&state.screen_focus)
        // Enter accepts only from the capsule itself: agreeing is a press on
        // the thing that says so, not whatever key lands on the page.
        .on_key_down(move |event, window, cx| {
            if event.keystroke.key == "enter" && accept_focus.is_focused(window) {
                keys.update(cx, |this, cx| this.accept_terms(cx));
            }
        })
        .into_any_element()
}

/// The terms in their box. The box scrolls; the page around it does not.
fn text_box() -> impl IntoElement {
    div()
        .id("terms-text")
        .w(px(BOX_WIDTH))
        .max_w_full()
        .flex_1()
        .min_h_0()
        .max_h(px(BOX_MAX_HEIGHT))
        .overflow_y_scroll()
        .rounded(px(radius::CARD))
        .bg(glass::ink(0.04))
        .border_1()
        .border_color(glass::hairline(0.12))
        .px(px(28.0))
        .py(px(24.0))
        .flex()
        .flex_col()
        .gap(px(22.0))
        .text_size(px(13.5))
        .line_height(px(21.0))
        .text_color(ladder::foreground_alpha(0.78))
        .children(SECTIONS.iter().map(|(heading, paragraphs)| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ladder::foreground())
                        .child(*heading)
                        .agent_node(Role::Text, *heading),
                )
                .children(paragraphs.iter().map(|paragraph| div().child(*paragraph)))
        }))
        .agent_node(Role::Card, "Terms")
}
