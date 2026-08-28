//! The sign-in gate: email, then the six-digit code Supabase mailed back.
//!
//! Route content inside the shell's one dialog host, exactly like the venue
//! picker — the host owns the scrim, the backdrop samples and the focus trap,
//! and this module owns only which of the two steps is showing. The palette is
//! the same one every dialog wears: a header band carrying the field and the
//! committing chip, a body, a footer legend.
//!
//! # It is a gate, not a wall
//!
//! Escape works, and the chip beside it says so. A signed-out Luma is not a
//! broken Luma: guest rows carry no `uid`, and the app database's admission
//! triggers admit those unconditionally, so the whole local library — venues,
//! tracks, patterns, scores — opens and edits without a session. What being
//! signed out costs is the cloud: sync, shared venues, anything that needs a
//! principal to own the row. So this dialog offers the door and holds it open;
//! it does not bar it.
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

use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::IconName;
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float;
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};

use crate::{shell::Overlay, Luma};

/// One size for both steps. Asking for an address and asking for the code that
/// address received are the same question a step apart, so the card holds
/// still and the morph spends its whole span on the content.
const GATE_SIZE: MorphSize = MorphSize::new(520.0, 296.0);

/// What Supabase mails. Anything longer is a paste of something else.
const CODE_LENGTH: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Email,
    Code,
}

impl Route {
    fn descriptor(self) -> RouteDescriptor<Self> {
        RouteDescriptor::exact(self, GATE_SIZE.width, GATE_SIZE.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusTarget {
    Email,
    Code,
}

/// One sign-in attempt.
///
/// `sent_to` is deliberately not `email`: the code belongs to the address it
/// was mailed to, and a user who edits the field on the way back would
/// otherwise verify a code against an address that never received one.
pub(crate) struct SignIn {
    /// Correlates a submit with the dialog that made it. A slow answer landing
    /// after the user backed out is a stale answer, not the current state.
    generation: u64,
    morph: MorphDialog<Route>,
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
    /// The Code route's Back cap. Email is the root of this dialog.
    leading_focus: FocusHandle,
    action_focus: FocusHandle,
    close_focus: FocusHandle,
    focus_pending: Option<FocusTarget>,
    _field_subscriptions: [Subscription; 2],
}

impl SignIn {
    fn new(generation: u64, cx: &mut Context<Luma>) -> Self {
        let email_field = cx.new(|cx| TextInput::search("you@example.com", cx));
        let code_field = cx.new(|cx| TextInput::search("000000", cx));
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
            morph: MorphDialog::new(Route::Email.descriptor(), GATE_SIZE),
            email: String::new(),
            code: String::new(),
            sent_to: None,
            busy: false,
            error: None,
            email_field,
            code_field,
            email_focus,
            code_focus,
            leading_focus: cx.focus_handle().tab_stop(true),
            action_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
            focus_pending: Some(FocusTarget::Email),
            _field_subscriptions: subscriptions,
        }
    }

    /// The route the gate is on, or arriving at. Read through the morph so a
    /// request mid-flight already reports its destination.
    fn route(&self) -> Route {
        *self.morph.target_key()
    }

    fn go(&mut self, route: Route, reduced: bool) {
        self.morph
            .request(route.descriptor(), Instant::now(), reduced);
    }

    /// Whether the committing chip may be pressed on this route.
    fn can_submit(&self) -> bool {
        !self.busy
            && match self.route() {
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
    /// Put the gate up. The shell keeps whatever is under it; signing in does
    /// not rebuild the app, it only changes who owns the rows it writes next.
    pub(crate) fn show_sign_in(&mut self, cx: &mut Context<Self>) {
        if matches!(self.overlay.as_open(), Some(Overlay::SignIn(_))) {
            return;
        }
        self.sign_in_generation = self.sign_in_generation.wrapping_add(1);
        let generation = self.sign_in_generation;
        self.overlay
            .open(Overlay::SignIn(Box::new(SignIn::new(generation, cx))));
        cx.notify();
    }

    /// Leave the gate and use the library as a guest. The venue restore that a
    /// signed-in launch would have run happens here instead, so both ways in
    /// land on the same screen.
    pub(crate) fn continue_offline(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.overlay.as_open(), Some(Overlay::SignIn(_))) {
            return;
        }
        self.close_overlay(cx);
        self.restore_venue(cx);
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

    /// The gate's keyboard, in the same navigator shape as the other dialogs:
    /// Enter commits the route, ← / ⌫-on-empty step back, Escape works
    /// offline.
    fn sign_in_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(Overlay::SignIn(state)) = self.overlay.as_open() else {
            return;
        };
        let route = state.route();
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
        let reduced = luma_ui::motion::reduced_motion(cx);
        self.with_sign_in(cx, move |state| {
            if state.busy {
                return;
            }
            state.go(Route::Email, reduced);
            state.error = None;
            state.focus_pending = Some(FocusTarget::Email);
        });
    }

    /// Commit whichever step is showing: mail a code, or spend one.
    fn sign_in_submit(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::SignIn(state)) = self.overlay.as_open() else {
            return;
        };
        if !state.can_submit() {
            return;
        }
        match state.route() {
            Route::Email => self.send_login_code(cx),
            Route::Code => self.verify_login_code(cx),
        }
    }

    fn send_login_code(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::SignIn(state)) = self.overlay.open_mut() else {
            return;
        };
        let email = state.email.trim().to_string();
        let generation = state.generation;
        state.busy = true;
        state.error = None;
        let pending = self.library.send_login_code(&email);
        let reduced = luma_ui::motion::reduced_motion(cx);
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
                            state.go(Route::Code, reduced);
                            state.focus_pending = Some(FocusTarget::Code);
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
        let Some(Overlay::SignIn(state)) = self.overlay.open_mut() else {
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
                    // gate leaves and the launch it was standing in front of
                    // resumes. No restart — nothing above the seam cached the
                    // old identity except `Library::user_id`, and the sign-in
                    // wrote through it.
                    Ok(_) => {
                        let current = matches!(
                            this.overlay.as_open(),
                            Some(Overlay::SignIn(state)) if state.generation == generation
                        );
                        if current {
                            this.close_overlay(cx);
                            this.restore_venue(cx);
                        }
                    }
                    Err(error) => this.with_current_sign_in(generation, cx, |state| {
                        state.busy = false;
                        state.code.clear();
                        state.error = Some(error.to_string());
                        state.focus_pending = Some(FocusTarget::Code);
                    }),
                }
            })
            .ok();
        })
        .detach();
    }

    fn with_sign_in(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut SignIn)) {
        if let Some(Overlay::SignIn(state)) = self.overlay.open_mut() {
            edit(state);
            cx.notify();
        }
    }

    /// Apply `edit` only if the gate that made request `generation` is still
    /// the one on screen.
    fn with_current_sign_in(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut SignIn),
    ) {
        if let Some(Overlay::SignIn(state)) = self.overlay.open_mut() {
            if state.generation == generation {
                edit(state);
                cx.notify();
            }
        }
    }
}

/// Advance the morph and commit focus, never while a route is in flight — an
/// in-flight copy carries no focus handles (see [`field`]).
pub(crate) fn tick(
    state: &mut SignIn,
    dialog_focus: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<Luma>,
) {
    let now = Instant::now();
    if state.morph.tick(now, luma_ui::motion::reduced_motion(cx)) {
        window.request_animation_frame();
    }
    if state.morph.sample(now).animating {
        window.focus(dialog_focus, cx);
        return;
    }
    if let Some(route) = state.morph.take_focus_after_commit() {
        state.focus_pending = Some(match route {
            Route::Email => FocusTarget::Email,
            Route::Code => FocusTarget::Code,
        });
    }
    match state.focus_pending.take() {
        Some(FocusTarget::Email) => window.focus(&state.email_focus, cx),
        Some(FocusTarget::Code) => window.focus(&state.code_focus, cx),
        None => {}
    }
}

/// The gate's card. Like the other dialogs, it owns its own `morph::card` —
/// the shell hands it no box to live in.
pub(crate) fn render(
    state: &SignIn,
    app: &Entity<Luma>,
    window: &Window,
    _cx: &mut gpui::App,
) -> AnyElement {
    let sample = state.morph.sample(Instant::now());
    let app = app.clone();
    morph::card(&sample, "Sign-in dialog", move |route, mode| {
        route_body(state, *route, mode, &app, window)
    })
}

fn route_body(
    state: &SignIn,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let keys = app.clone();
    let mut frame = div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground());
    if mode == ContentMode::Interactive {
        // No `track_focus`: the dialog host already owns this card's trap.
        frame = frame.on_key_down(move |event, _, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.sign_in_key(&event, cx));
        });
    }
    frame
        .child(header(state, route, mode, app, window))
        .child(div().flex_1().min_h_0().flex().child(body(state, route)))
        .child(footer(route))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Bands
// ---------------------------------------------------------------------------

fn header(
    state: &SignIn,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let interactive = mode == ContentMode::Interactive;
    let mut band = float::header_band();

    if route == Route::Code {
        let back = app.clone();
        let pressable = interactive && !state.busy;
        let cap = float::key_cap();
        let cap = if pressable {
            float::key_cap_pressable(cap)
        } else {
            cap
        };
        band = band.child(
            cap.id("sign-in-back")
                .when(interactive, |cap| {
                    cap.track_focus(&state.leading_focus).tab_index(0)
                })
                .when(pressable, |cap| {
                    cap.on_click(move |_, _, cx| {
                        back.update(cx, |this, cx| this.sign_in_back(cx));
                    })
                })
                .child(gpui_component::Icon::new(IconName::ArrowLeft).size(px(12.5)))
                .agent_node(Role::Button, "Back")
                .agent_disabled(!pressable)
                .agent_focused(interactive && state.leading_focus.is_focused(window)),
        );
    }

    band = band.child(field(state, route, mode, window));
    band = band.child(submit_chip(state, route, mode, app, window));

    let offline = app.clone();
    let cap = if interactive {
        float::key_cap_pressable(float::key_cap())
    } else {
        float::key_cap()
    };
    band.child(
        cap.id("sign-in-offline")
            .when(interactive, |cap| {
                cap.track_focus(&state.close_focus)
                    .tab_index(0)
                    .on_click(move |_, _, cx| {
                        offline.update(cx, |this, cx| this.continue_offline(cx));
                    })
            })
            .child("esc")
            .agent_node(Role::Button, "Work offline")
            .agent_disabled(!interactive)
            .agent_focused(interactive && state.close_focus.is_focused(window)),
    )
}

/// The header's one field: the address on the first step, the code on the
/// second. Same slot, because it is the same question one step apart.
fn field(state: &SignIn, route: Route, mode: ContentMode, window: &Window) -> AnyElement {
    let slot = div().flex_1().min_w_0().text_size(px(14.0));
    let (entity, focus, value, placeholder) = match route {
        Route::Email => (
            &state.email_field,
            &state.email_focus,
            state.email.as_str(),
            "you@example.com",
        ),
        Route::Code => (
            &state.code_field,
            &state.code_focus,
            state.code.as_str(),
            "000000",
        ),
    };
    // The semantic label is the VALUE once there is one: a driver asking what
    // this field says wants what it says, not what it would say if empty.
    let label = if value.is_empty() { placeholder } else { value };
    if mode != ContentMode::Interactive {
        // An in-flight copy paints the text; a live field would register a
        // focus handle, which the morph contract forbids of a paint-only layer.
        return slot
            .text_color(if value.is_empty() {
                ladder::muted_foreground().into()
            } else {
                ladder::foreground_alpha(1.0)
            })
            .child(label.to_string())
            .agent_node(Role::Input, label.to_string())
            .agent_disabled(true)
            .into_any_element();
    }
    slot.child(entity.clone())
        .agent_node(Role::Input, label.to_string())
        .agent_focused(focus.is_focused(window))
        .into_any_element()
}

/// The committing chip. One label per route, and its glyph must match the
/// chord the footer legend promises.
fn submit_chip(
    state: &SignIn,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let interactive = mode == ContentMode::Interactive;
    let label = match (route, state.busy) {
        (Route::Email, false) => "Send code",
        (Route::Email, true) => "Sending…",
        (Route::Code, false) => "Sign in",
        (Route::Code, true) => "Verifying…",
    };
    let enabled = state.can_submit();
    let submit = app.clone();
    float::btn_primary_chip()
        .id("sign-in-submit")
        .when(enabled && interactive, |chip| {
            chip.track_focus(&state.action_focus)
                .tab_index(0)
                .on_click(move |_, _, cx| {
                    submit.update(cx, |this, cx| this.sign_in_submit(cx));
                })
        })
        .when(!enabled || !interactive, |chip| {
            chip.opacity(float::INERT_OPACITY)
        })
        .child("↵")
        .child(label)
        .agent_node(Role::Button, label)
        .agent_disabled(!enabled || !interactive)
        .agent_focused(interactive && state.action_focus.is_focused(window))
        .into_any_element()
}

fn footer(route: Route) -> Div {
    let mut band = float::footer_band();
    if route == Route::Code {
        band = band.child(float::key_hint(IconName::ArrowLeft, "Back"));
    }
    band.child(float::key_hint_text(
        "↵",
        match route {
            Route::Email => "Send code",
            Route::Code => "Sign in",
        },
    ))
    .child(float::key_hint_text("esc", "Work offline"))
    .child(div().flex_1().min_w_0())
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

/// The message. Leaving the gate is offered by the `esc` cap and the footer
/// legend, and once by each: a third affordance for one gesture would make the
/// card argue with itself about which one is the way out.
fn body(state: &SignIn, route: Route) -> AnyElement {
    let heading = match route {
        Route::Email => "Sign in to Luma",
        Route::Code => "Check your email",
    };
    let subtitle = match route {
        Route::Email => "Enter your email and we'll send a six-digit code.".to_string(),
        Route::Code => match &state.sent_to {
            Some(address) => format!("Enter the six-digit code sent to {address}."),
            None => "Enter the six-digit code we sent you.".to_string(),
        },
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .px(px(40.0))
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(FontWeight::LIGHT)
                .child(heading)
                .agent_node(Role::Text, heading),
        )
        .child(
            div()
                .text_size(px(12.5))
                .text_color(ladder::muted_foreground())
                .text_center()
                .child(subtitle),
        )
        // Bounded and clipped: GoTrue's prose is short, but a proxy or a
        // captive portal can answer with a page, and one enormous token has no
        // wrap point. The semantic node keeps the whole string.
        .when_some(state.error.clone(), |column, error| {
            column.child(
                div()
                    .w_full()
                    .max_w(px(400.0))
                    .h(px(20.0))
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_center()
                            .text_size(px(12.5))
                            .text_color(ladder::danger())
                            .child(error.clone())
                            .agent_node(Role::Text, error),
                    ),
            )
        })
        .into_any_element()
}
