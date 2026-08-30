//! The one confirmation dialog: a question, and the two answers to it.
//!
//! # Why the action is an enum and not a closure
//!
//! A confirmation is state that outlives the gesture that raised it — it sits
//! in [`crate::shell::Overlay`] across frames, across the dialog's own exit
//! animation, and it has to survive the list it was raised from being
//! rebuilt. A boxed callback in that slot would be a piece of the raising
//! screen's `render` held alive by the shell, and running it means handing it
//! the `&mut Luma` the shell is already inside. [`Action`] is instead a
//! *description* of what was agreed to, matched in one place
//! ([`Luma::confirmed`]) — which is also the only place a reader has to look
//! to find out what this app will destroy if you press the red word.
//!
//! Adding a second confirmation is a variant and an arm. That is deliberately
//! more friction than a closure: every destructive act in the app should be
//! greppable from one enum.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, FocusHandle, SharedString};

use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::float;
use luma_ui::glass;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::Luma;

/// What pressing the confirming button does.
///
/// One variant per destructive act the app can raise a dialog for — see the
/// module docs for why this is a closed list.
#[derive(Clone)]
pub(crate) enum Action {
    /// Unpatch fixtures that are standing in the room. Both rows go — the
    /// paperwork and the node — so the question is worth asking.
    UnpatchFixtures {
        venue_id: SharedString,
        fixture_ids: Vec<String>,
    },
    /// Re-derive every address in a venue, discarding the hand-set ones.
    AutoPatch { venue_id: SharedString },
    /// Let the allocator move a fixture so a wider mode fits.
    RepatchMode {
        venue_id: SharedString,
        fixture_id: SharedString,
        mode_name: SharedString,
    },
    /// Archive a score, and take the editor off it if it was the open one.
    DeleteScore {
        score_id: SharedString,
        /// The track whose scores level raised this, so the listing can be
        /// re-read against the level that is still up.
        track_id: SharedString,
        /// Which room's timeline might be showing it. A score belongs to one
        /// venue, and the tab that has to be taken off it is keyed by the
        /// pair.
        venue_id: SharedString,
    },
}

/// A question the user has to answer before something is destroyed.
///
/// The prose is resolved when the dialog is raised, not when it is drawn: the
/// count it quotes is what the operator saw on the row they right-clicked, and
/// a dialog that re-derived it every frame would silently change the sentence
/// under them while they read it.
pub(crate) struct Confirm {
    pub(crate) title: SharedString,
    pub(crate) body: SharedString,
    /// The verb on the destructive button. "Delete score", not "OK" — the
    /// button says what it does, so the dialog is answerable without the
    /// title.
    pub(crate) verb: SharedString,
    pub(crate) action: Action,
}

impl Luma {
    /// Raise `confirm` over the shell.
    pub(crate) fn ask(&mut self, confirm: Confirm, cx: &mut gpui::Context<Self>) {
        self.overlay.open(crate::shell::Overlay::Confirm(confirm));
        cx.notify();
    }

    /// The confirming button. Closes the dialog first, so the act runs against
    /// a shell with nothing over it — a delete that re-reads a listing must
    /// not be racing its own dialog's exit.
    pub(crate) fn confirmed(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(crate::shell::Overlay::Confirm(confirm)) = self.overlay.as_open() else {
            return;
        };
        let action = confirm.action.clone();
        self.close_overlay(cx);
        match action {
            Action::UnpatchFixtures {
                venue_id,
                fixture_ids,
            } => self.run_unpatch(venue_id.to_string(), fixture_ids, cx),
            Action::AutoPatch { venue_id } => self.run_auto_patch(venue_id.to_string(), cx),
            Action::RepatchMode {
                venue_id,
                fixture_id,
                mode_name,
            } => self.set_patch_mode(
                venue_id.to_string(),
                fixture_id.to_string(),
                mode_name.to_string(),
                true,
                cx,
            ),
            Action::DeleteScore {
                score_id,
                track_id,
                venue_id,
            } => self.delete_score(&track_id, &score_id, &venue_id, cx),
        }
    }
}

/// The card: the question, the sentence under it, and the two answers.
pub(crate) fn render(
    state: &Confirm,
    app: &Entity<Luma>,
    cancel_focus: &FocusHandle,
    cancel_focused: bool,
    confirm_focus: &FocusHandle,
    confirm_focused: bool,
) -> AnyElement {
    let dismissed = app.clone();
    let confirmed = app.clone();
    let body = div()
        .size_full()
        .flex()
        .flex_col()
        .justify_between()
        .p(px(20.))
        .gap(px(16.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(15.))
                        .text_color(glass::ink(0.95))
                        .child(state.title.clone())
                        .agent_node(Role::Text, state.title.clone()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(glass::ink(0.6))
                        .child(state.body.clone())
                        .agent_node(Role::Text, state.body.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(8.))
                .child(
                    float::btn("Cancel", "confirm-cancel")
                        .id("confirm-cancel")
                        .track_focus(cancel_focus)
                        .tab_stop(true)
                        .on_click(move |_, _, cx| {
                            dismissed.update(cx, |this, cx| this.close_overlay(cx));
                        })
                        .agent_node(Role::Button, "Cancel")
                        .agent_focused(cancel_focused),
                )
                .child(
                    // The danger tone is on the *word*, not on a second button
                    // fill: this card already has one primary shape, and a red
                    // slab beside a quiet one would be a third button style.
                    float::btn(state.verb.clone(), "confirm-act")
                        .id("confirm-act")
                        .track_focus(confirm_focus)
                        .tab_stop(true)
                        .text_color(luma_ui::ladder::danger())
                        .on_click(move |_, _, cx| {
                            confirmed.update(cx, |this, cx| this.confirmed(cx));
                        })
                        .agent_node(Role::Button, state.verb.clone())
                        .agent_focused(confirm_focused),
                ),
        );
    morph::fixed_card(
        "Confirm dialog",
        MorphSize::new(WIDTH, HEIGHT),
        body.into_any_element(),
    )
}

/// A sentence's worth of width and two lines of height. Fixed, because the
/// prose is one sentence by construction — a confirmation that needed a
/// paragraph is a confirmation asking the wrong question.
const WIDTH: f32 = 400.0;
const HEIGHT: f32 = 176.0;
