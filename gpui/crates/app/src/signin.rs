//! The sign-in screen: email, then the six-digit code Supabase mailed back.
//!
//! **A state of the app, not a plane over it.** While it is up the shell is
//! not rendered at all: there is no scrim, nothing behind it to reach, and
//! nothing under it to keep alive — signing in is a change of identity, and
//! the regions on the far side of it belong to whoever the app admits.
//!
//! # There is no card
//!
//! A dialog is a thing *over* something. This screen is over nothing, so it
//! wears no surface: the app's own ground edge to edge, the window's controls
//! in the corner, and one centred column — mark, title, and the capsules of
//! [`luma_ui::pill`]. A card here would draw a box around a room that is
//! already empty, and the box, not the question, would be what the eye lands
//! on.
//!
//! Because the column is the whole screen, the two routes are a *swap* rather
//! than a morph: nothing encloses the content, so there is no outline for a
//! tween to carry between two shapes.
//!
//! # It is a gate, not a wall
//!
//! Every route offers the way past it — the secondary capsule on the first
//! step, the quiet link on the second — and Escape does the same thing. A
//! signed-out Luma is not a broken Luma: guest rows carry no `uid`, and the
//! app database's admission triggers admit those unconditionally, so the whole
//! local library — venues, tracks, patterns, scores — opens and edits without
//! a session. What being signed out costs is the cloud: sync, shared venues,
//! anything that needs a principal to own the row. So this screen offers the
//! door and holds it open; it does not bar it.
//!
//! # Why the host performs the exchange
//!
//! Both halves of email OTP are dispatch commands (`send_login_code`,
//! `verify_login_code`). The `Library` is this binary's only route to Luma's
//! behavior, and a screen that reached past it for an HTTP call would need the
//! Supabase URL and key up here — where nothing else needs them — and would
//! then hand the session back down to be persisted anyway. Verifying a code is
//! how a session is obtained; admitting one is an identity switch, and the
//! host owns that.

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};
use luma_ui::{ladder, mark, pill, Enabled};

use crate::{chrome, keymap, Luma};

/// What Supabase mails. Anything longer is a paste of something else.
const CODE_LENGTH: usize = 6;

/// The mark's box, and the air under it before the title.
const MARK_SIZE: f32 = 44.0;
const MARK_GAP: f32 = 20.0;

/// The one heading. Large and regular rather than small and bold: it is the
/// only sentence on the screen, so it does not have to shout to be found.
const TITLE_SIZE: f32 = 28.0;

/// Between the title and the line under it that names the address.
const SUBTITLE_GAP: f32 = 8.0;

/// Between the field and the error it grew, and between the last capsule and
/// the link below it. Half a [`pill::GAP`] — an error belongs to the field
/// above it, and the link is not one of the capsules.
const TIGHT_GAP: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Email,
    Code,
}

/// One sign-in attempt.
///
/// `sent_to` is deliberately not `email`: the code belongs to the address it
/// was mailed to, and a user who edits the field on the way back would
/// otherwise verify a code against an address that never received one.
pub(crate) struct SignIn {
    /// Correlates a submit with the screen that made it. A slow answer landing
    /// after the user backed out is a stale answer, not the current state.
    generation: u64,
    route: Route,
    email: String,
    code: String,
    sent_to: Option<String>,
    /// A request is in flight. Both steps use it, because both are one
    /// round trip and neither may be sent twice.
    busy: bool,
    error: Option<String>,
    email_field: Entity<TextInput>,
    code_field: Entity<TextInput>,
    email_focus: FocusHandle,
    code_focus: FocusHandle,
    /// The screen's own handle. The keyboard rests here whenever no field
    /// holds it, so the root's key handler always has a dispatch path to be on.
    screen_focus: FocusHandle,
    action_focus: FocusHandle,
    secondary_focus: FocusHandle,
    /// The quiet link, which only the Code route carries.
    link_focus: FocusHandle,
    /// Which route's field should take the keyboard on the next frame.
    focus_pending: Option<Route>,
    _field_subscriptions: [Subscription; 2],
}

impl SignIn {
    fn new(generation: u64, cx: &mut Context<Luma>) -> Self {
        let email_field = cx.new(|cx| TextInput::search("Email", cx));
        let code_field = cx.new(|cx| TextInput::search("Code", cx));
        let email_focus = email_field.read(cx).focus_handle(cx);
        let code_focus = code_field.read(cx).focus_handle(cx);
        let subscriptions = [
            cx.subscribe(&email_field, |luma, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let email = field.read(cx).text().to_string();
                    luma.sign_in_email_changed(email, cx);
                } else {
                    cx.notify();
                }
            }),
            cx.subscribe(&code_field, |luma, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let code = field.read(cx).text().to_string();
                    luma.sign_in_code_changed(code, cx);
                } else {
                    cx.notify();
                }
            }),
        ];
        Self {
            generation,
            route: Route::Email,
            email: String::new(),
            code: String::new(),
            sent_to: None,
            busy: false,
            error: None,
            email_field,
            code_field,
            email_focus,
            code_focus,
            screen_focus: cx.focus_handle(),
            action_focus: cx.focus_handle().tab_stop(true),
            secondary_focus: cx.focus_handle().tab_stop(true),
            link_focus: cx.focus_handle().tab_stop(true),
            focus_pending: Some(Route::Email),
            _field_subscriptions: subscriptions,
        }
    }

    /// Whether the primary capsule may be pressed on this route.
    fn can_submit(&self) -> bool {
        !self.busy
            && match self.route {
                Route::Email => self.email.trim().contains('@'),
                Route::Code => self.code.trim().len() == CODE_LENGTH,
            }
    }
}

// -- navigation and the exchange ----------------------------------------------
//
// On `Luma` rather than on `SignIn` for the same reason the venue picker's are:
// a submit is a `Library` call and a screen transition, and `Luma` owns both.

impl Luma {
    /// Show the sign-in screen. Whatever the shell was showing stays in state
    /// and comes back untouched when the screen leaves — except any dialog,
    /// which goes now rather than animating out later over a shell the user
    /// has not looked at since (settings is the one route that gets here with
    /// an overlay up).
    pub(crate) fn show_sign_in(&mut self, cx: &mut Context<Self>) {
        if self.sign_in.is_some() {
            return;
        }
        self.overlay = luma_ui::dialog::Popup::default();
        self.sign_in_generation = self.sign_in_generation.wrapping_add(1);
        let generation = self.sign_in_generation;
        self.sign_in = Some(Box::new(SignIn::new(generation, cx)));
        cx.notify();
    }

    /// Leave the screen and use the library as a guest. The venue restore that
    /// a signed-in launch would have run happens here instead, so both ways in
    /// land on the same screen.
    pub(crate) fn continue_offline(&mut self, cx: &mut Context<Self>) {
        if self.sign_in.take().is_none() {
            return;
        }
        self.restore_venue(cx);
        cx.notify();
    }

    fn sign_in_email_changed(&mut self, email: String, cx: &mut Context<Self>) {
        self.with_sign_in(cx, |state| {
            state.email = email;
            state.error = None;
        });
    }

    fn sign_in_code_changed(&mut self, code: String, cx: &mut Context<Self>) {
        self.with_sign_in(cx, |state| {
            state.code = code;
            state.error = None;
        });
    }

    /// The screen's keyboard, in the same navigator shape as the dialogs':
    /// Enter commits the route, ← / ⌫-on-empty step back, Escape works
    /// offline.
    fn sign_in_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(state) = self.sign_in.as_ref() else {
            return;
        };
        let route = state.route;
        let field_empty = match route {
            Route::Email => state.email.is_empty(),
            Route::Code => state.code.is_empty(),
        };
        match (route, event.keystroke.key.as_str()) {
            (_, "escape") => self.continue_offline(cx),
            (_, "enter") => self.sign_in_submit(cx),
            (Route::Code, "left") => self.sign_in_back(cx),
            (Route::Code, "backspace") if field_empty => self.sign_in_back(cx),
            _ => {}
        }
    }

    fn sign_in_back(&mut self, cx: &mut Context<Self>) {
        self.with_sign_in(cx, |state| {
            if state.busy {
                return;
            }
            state.route = Route::Email;
            state.error = None;
            state.focus_pending = Some(Route::Email);
        });
    }

    /// Commit whichever step is showing: mail a code, or spend one.
    fn sign_in_submit(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.sign_in.as_ref() else {
            return;
        };
        if !state.can_submit() {
            return;
        }
        match state.route {
            Route::Email => self.send_login_code(cx),
            Route::Code => self.verify_login_code(cx),
        }
    }

    fn send_login_code(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.sign_in.as_mut() else {
            return;
        };
        let email = state.email.trim().to_string();
        let generation = state.generation;
        state.busy = true;
        state.error = None;
        let pending = self.library.send_login_code(&email);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.with_current_sign_in(generation, cx, |state| {
                    state.busy = false;
                    match result {
                        Ok(()) => {
                            state.sent_to = Some(email);
                            state.code.clear();
                            state.route = Route::Code;
                            state.focus_pending = Some(Route::Code);
                        }
                        Err(error) => state.error = Some(error.to_string()),
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    fn verify_login_code(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.sign_in.as_mut() else {
            return;
        };
        // The address the code was mailed to, never the field: see `sent_to`.
        let Some(email) = state.sent_to.clone() else {
            return;
        };
        let code = state.code.trim().to_string();
        let generation = state.generation;
        state.busy = true;
        state.error = None;
        let pending = self.library.verify_login_code(&email, &code);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                match result {
                    // Signed in: the library is now the principal's, so the
                    // screen leaves and the launch it was standing in front of
                    // resumes. No restart — nothing above the seam cached the
                    // old identity except `Library::user_id`, and the sign-in
                    // wrote through it.
                    Ok(_) => {
                        let current = this
                            .sign_in
                            .as_ref()
                            .is_some_and(|state| state.generation == generation);
                        if current {
                            this.sign_in = None;
                            this.restore_venue(cx);
                            cx.notify();
                        }
                    }
                    Err(error) => this.with_current_sign_in(generation, cx, |state| {
                        state.busy = false;
                        state.code.clear();
                        state.error = Some(error.to_string());
                        state.focus_pending = Some(Route::Code);
                    }),
                }
            })
            .ok();
        })
        .detach();
    }

    fn with_sign_in(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut SignIn)) {
        if let Some(state) = self.sign_in.as_mut() {
            edit(state);
            cx.notify();
        }
    }

    /// Apply `edit` only if the screen that made request `generation` is still
    /// the one showing.
    fn with_current_sign_in(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut SignIn),
    ) {
        if let Some(state) = self.sign_in.as_mut() {
            if state.generation == generation {
                edit(state);
                cx.notify();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

/// The whole window while nobody is signed in: the drag band, the window's
/// controls, and the centred column.
///
/// Called instead of `shell::regions`, not beside it — see the module docs.
pub(crate) fn screen(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    let entity = cx.entity();
    let Some(state) = app.sign_in.as_mut() else {
        return div().into_any_element();
    };
    match state.focus_pending.take() {
        Some(Route::Email) => window.focus(&state.email_focus, cx),
        Some(Route::Code) => window.focus(&state.code_focus, cx),
        None => {}
    }
    let state = app.sign_in.as_ref().expect("the screen is up");
    let keys = entity.clone();
    let viewport = f32::from(window.viewport_size().width);
    div()
        .size_full()
        .flex()
        .flex_col()
        // The app's ground, not a scrim: nothing is behind this screen.
        .bg(ladder::background())
        .font_family(luma_ui::fonts::FAMILY)
        .text_color(ladder::foreground())
        .key_context(keymap::context::SIGN_IN)
        .track_focus(&state.screen_focus)
        .on_key_down(move |event, _, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.sign_in_key(&event, cx));
        })
        // The window still has to move and close. A screen with no regions
        // has no region head band either, so it carries its own — see
        // [`crate::chrome`].
        .child(chrome::band(chrome::BandSpan {
            x: 0.0,
            width: viewport,
            viewport,
        }))
        .child(column(state, &entity, window))
        // Last, so nothing can cover the only controls that move and close the
        // window.
        .child(chrome::window_controls())
        .into_any_element()
}

/// Mark, title, and the capsules — the only thing on the screen.
fn column(state: &SignIn, app: &Entity<Luma>, window: &Window) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(pill::HEAD_GAP))
        // The band takes its height off the top of the flex, so the column
        // pays the same at the bottom: centred on the *window*, not on what
        // the chrome left over.
        .pb(px(luma_ui::dialog::TITLEBAR_CLEARANCE))
        .child(head(state))
        .child(stack(state, app, window))
}

fn head(state: &SignIn) -> Div {
    let title = match state.route {
        Route::Email => "Sign in to Luma",
        Route::Code => "Check your email",
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(MARK_GAP))
        .child(mark::luma(MARK_SIZE))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(SUBTITLE_GAP))
                .child(
                    div()
                        .text_size(px(TITLE_SIZE))
                        .font_weight(FontWeight::NORMAL)
                        .child(title)
                        .agent_node(Role::Text, title),
                )
                // Only the Code route has anything to add: which address is
                // waiting. The Email route's question is its own title.
                .when_some(subtitle(state), |column, line| {
                    column.child(
                        div()
                            .text_size(px(13.0))
                            .text_color(ladder::muted_foreground())
                            .child(line.clone())
                            .agent_node(Role::Text, line),
                    )
                }),
        )
}

fn subtitle(state: &SignIn) -> Option<SharedString> {
    match state.route {
        Route::Email => None,
        Route::Code => Some(SharedString::from(match &state.sent_to {
            Some(address) => format!("We sent a code to {address}"),
            None => "We sent you a six-digit code".to_string(),
        })),
    }
}

fn stack(state: &SignIn, app: &Entity<Luma>, window: &Window) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(pill::GAP))
        .child(field(state, window))
        .child(primary(state, app, window))
        .child(secondary(state, app, window))
        .when(state.route == Route::Code, |column| {
            column.child(link(state, app, window))
        })
}

/// The route's one field, inside its capsule: the address on the first step,
/// the code on the second.
fn field(state: &SignIn, window: &Window) -> Div {
    let (entity, focus, value, placeholder) = match state.route {
        Route::Email => (
            &state.email_field,
            &state.email_focus,
            state.email.as_str(),
            "Email",
        ),
        Route::Code => (
            &state.code_field,
            &state.code_focus,
            state.code.as_str(),
            "Code",
        ),
    };
    // The semantic label is the VALUE once there is one: a driver asking what
    // this field says wants what it says, not what it would say if empty.
    let label = if value.is_empty() { placeholder } else { value };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(TIGHT_GAP))
        .child(
            pill::field()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(luma_ui::text_input::TEXT_SIZE))
                        .child(entity.clone()),
                )
                .agent_node(Role::Input, label.to_string())
                .agent_focused(focus.is_focused(window)),
        )
        // One line, bounded and clipped: GoTrue's prose is short, but a proxy
        // or a captive portal can answer with a page, and one enormous token
        // has no wrap point. The semantic node keeps the whole string.
        .when_some(state.error.clone(), |column, error| {
            column.child(
                div()
                    .w(px(pill::WIDTH))
                    .truncate()
                    .text_center()
                    .text_size(px(12.5))
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error),
            )
        })
}

fn primary(state: &SignIn, app: &Entity<Luma>, window: &Window) -> AnyElement {
    let label = match (state.route, state.busy) {
        (Route::Email, false) => "Continue",
        (Route::Email, true) => "Sending…",
        (Route::Code, false) => "Sign in",
        (Route::Code, true) => "Verifying…",
    };
    let enabled = state.can_submit();
    let submit = app.clone();
    pill::primary(label, Enabled::from(enabled))
        .id("sign-in-submit")
        .when(enabled, |capsule| {
            capsule
                .track_focus(&state.action_focus)
                .tab_index(0)
                .on_click(move |_, _, cx| {
                    submit.update(cx, |this, cx| this.sign_in_submit(cx));
                })
        })
        .agent_node(Role::Button, label)
        .agent_disabled(!enabled)
        .agent_focused(state.action_focus.is_focused(window))
        .into_any_element()
}

/// The route's alternative: the door out on the first step, the way back on
/// the second. One outlined capsule either way, because both answer the same
/// question — "not this".
fn secondary(state: &SignIn, app: &Entity<Luma>, window: &Window) -> AnyElement {
    let (label, id) = match state.route {
        Route::Email => ("Work offline", "sign-in-offline"),
        Route::Code => ("Use a different email", "sign-in-back"),
    };
    let route = state.route;
    let pressed = app.clone();
    pill::secondary(label)
        .id(id)
        .track_focus(&state.secondary_focus)
        .tab_index(0)
        .on_click(move |_, _, cx| {
            pressed.update(cx, |this, cx| match route {
                Route::Email => this.continue_offline(cx),
                Route::Code => this.sign_in_back(cx),
            });
        })
        .agent_node(Role::Button, label)
        .agent_focused(state.secondary_focus.is_focused(window))
        .into_any_element()
}

/// The Code route's way out. A link rather than a third capsule: leaving is
/// still offered, but it is not one of the two things this step is about.
fn link(state: &SignIn, app: &Entity<Luma>, window: &Window) -> AnyElement {
    let leave = app.clone();
    pill::link("Work offline")
        .id("sign-in-offline")
        .mt(px(TIGHT_GAP))
        .track_focus(&state.link_focus)
        .tab_index(0)
        .on_click(move |_, _, cx| {
            leave.update(cx, |this, cx| this.continue_offline(cx));
        })
        .agent_node(Role::Button, "Work offline")
        .agent_focused(state.link_focus.is_focused(window))
        .into_any_element()
}
