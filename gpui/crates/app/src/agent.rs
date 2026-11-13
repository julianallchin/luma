//! Each mode body owns its chat, and the agent panel shows the one on screen.
//! The open score's chats are that score's; Rig's are the venue's, and Live's
//! are its Perform set's (`docs/specs/comet-shell.md` §3).
//!
//! Every panel shares the app's one [`RunningTurns`], so a turn belongs to its
//! thread and not to whichever mode is on screen: switching modes or venues
//! lets go of the view and leaves the turn running.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{div, px, App, Context, Entity, Subscription};
use luma_chat::{AgentChat, RunningEvent, RunningTurns};
use luma_lib::agent::{ThreadScope, TurnContext, TurnEvent};
use luma_lib::database::local::feedback::{Target as FeedbackTarget, Verdict};
use luma_ui::ladder;
use luma_ui::node::{Instrument, Role};

use crate::shell::Body;
use crate::workspace::Target;
use crate::Luma;

/// One body's chat panel, and what its status dot needs beyond it.
pub(crate) struct BodyChat {
    pub(crate) panel: Entity<AgentChat>,
    /// The panel's turn ended while its body was not on screen. Cleared when
    /// the body is viewed.
    unseen: bool,
    _requests: Subscription,
}

/// What a tab's dot says. There is no "needs input": no turn event asks the
/// reader anything yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatStatus {
    Working,
    Finished,
}

impl ChatStatus {
    /// The stronger of two: a working tab outranks one that finished.
    pub(crate) fn max(self, other: Self) -> Self {
        if self == Self::Working || other == Self::Working {
            Self::Working
        } else {
            Self::Finished
        }
    }
}

/// The dot itself, in the size and recipe of the song list's coverage dot.
/// `subject` names it in the automation tree.
pub(crate) fn status_dot(status: ChatStatus, subject: &str) -> impl IntoElement {
    let (color, word) = match status {
        ChatStatus::Working => (ladder::status_warn(), "working"),
        ChatStatus::Finished => (ladder::status_ok(), "finished"),
    };
    div()
        .flex_none()
        .size(px(6.))
        .rounded_full()
        .bg(color)
        .agent_node(Role::Text, format!("{subject} {word}"))
}

/// The conversations a tab's chats belong to.
pub(crate) fn scope(target: &Target) -> ThreadScope {
    match target {
        Target::Score {
            venue,
            track,
            score,
        } => ThreadScope::track(track, venue, score),
        Target::Venue { venue } => ThreadScope::venue(venue),
        Target::Perform { venue } => ThreadScope::perform(venue),
    }
}

/// What a message sent from `target`'s chat is about: the tab's subject, and
/// how its timeline stands when it is a score, or its page and what plays
/// when it is Perform.
fn turn_context(app: &Luma, target: &Target) -> TurnContext {
    let body = app.venues.body(&app.workspace, target);
    TurnContext {
        scope: Some(scope(target)),
        editor: match body {
            Some(Body::TrackEditor(editor)) => Some(editor.agent_context()),
            _ => None,
        },
        perform: match body {
            Some(Body::Perform(perform)) => Some(perform.agent_context()),
            _ => None,
        },
    }
}

impl Body {
    pub(crate) fn chat(&self) -> &BodyChat {
        match self {
            Self::TrackEditor(state) => &state.chat,
            Self::Patch(state) => &state.chat,
            Self::Perform(state) => &state.chat,
        }
    }

    fn chat_mut(&mut self) -> &mut BodyChat {
        match self {
            Self::TrackEditor(state) => &mut state.chat,
            Self::Patch(state) => &mut state.chat,
            Self::Perform(state) => &mut state.chat,
        }
    }
}

impl Luma {
    /// A chat panel for `target`. It shows `thread` when one is named — the
    /// chat a restored tab had open — and else the subject's newest chat.
    pub(crate) fn body_chat(
        &mut self,
        target: &Target,
        thread: Option<&str>,
        cx: &mut Context<Self>,
    ) -> BodyChat {
        let agent = self.library.agent();
        let running = self.running.clone();
        // Weak, so the panel the app owns does not own the app back. The panel
        // sends from its own handlers, never while the app is being updated.
        let app = cx.weak_entity();
        let source = target.clone();
        let subject = scope(target);
        let panel = cx.new(|cx| {
            let mut chat = AgentChat::new(agent, running, subject.clone(), thread, cx);
            chat.set_context_source(Box::new(move |cx| {
                app.upgrade().map_or(
                    TurnContext {
                        scope: Some(subject.clone()),
                        editor: None,
                        perform: None,
                    },
                    |app| turn_context(app.read(cx), &source),
                )
            }));
            chat
        });
        let requests = cx.subscribe(&panel, |this, panel, event, cx| match event {
            luma_chat::ChatEvent::HistoryRequested => this.show_chat_history(cx),
            luma_chat::ChatEvent::SubagentsRequested(child) => {
                this.show_subagents(child.clone(), cx);
            }
            luma_chat::ChatEvent::FeedbackRequested(thread) => {
                this.read_turn_feedback(&panel, thread.clone(), cx);
            }
            luma_chat::ChatEvent::FeedbackGiven { target, verdict } => {
                this.store_turn_feedback(&panel, target.clone(), *verdict, cx);
            }
        });
        BodyChat {
            panel,
            unseen: false,
            _requests: requests,
        }
    }

    /// Hand `panel` the account's stored verdicts on `thread`'s turns.
    fn read_turn_feedback(
        &mut self,
        panel: &Entity<AgentChat>,
        thread: String,
        cx: &mut Context<Self>,
    ) {
        let stored = self.library.thread_feedback(&thread);
        let panel = panel.downgrade();
        cx.spawn(async move |_, cx| match stored.await {
            Ok(feedback) => {
                panel
                    .update(cx, |chat, cx| chat.set_verdicts(&thread, feedback, cx))
                    .ok();
            }
            // The thumbs work without it; they only start unlit.
            Err(error) => log::warn!("Could not read the chat's feedback: {error}"),
        })
        .detach();
    }

    /// Store a verdict `panel` is already showing. One that could not be
    /// stored is said so, and the panel is put back to what is stored.
    fn store_turn_feedback(
        &mut self,
        panel: &Entity<AgentChat>,
        target: FeedbackTarget,
        verdict: Option<Verdict>,
        cx: &mut Context<Self>,
    ) {
        let FeedbackTarget::Turn { thread_id, .. } = &target else {
            return;
        };
        let thread = thread_id.clone();
        let stored = self.library.set_feedback(target, verdict);
        let panel = panel.downgrade();
        cx.spawn(async move |this, cx| {
            let Err(error) = stored.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.report_failure("Could not save your feedback", error, cx);
                if let Some(panel) = panel.upgrade() {
                    this.read_turn_feedback(&panel, thread, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The chat of the body on screen, which is the agent panel's.
    pub(crate) fn front_chat(&self) -> Option<&Entity<AgentChat>> {
        self.workspace.active_body().map(|body| &body.chat().panel)
    }

    /// `target`'s dot, wherever its body is. `None` for no dot, and for a
    /// target with nothing open.
    pub(crate) fn body_status(&self, target: &Target, cx: &App) -> Option<ChatStatus> {
        status(self.venues.body(&self.workspace, target)?.chat(), cx)
    }

    /// The dots of the venue on screen, by body: what the mode switch and the
    /// song list show.
    pub(crate) fn body_statuses(&self, cx: &App) -> HashMap<Target, ChatStatus> {
        self.workspace
            .iter()
            .filter_map(|slot| Some((slot.target.clone(), status(slot.body.chat(), cx)?)))
            .collect()
    }

    /// The strongest dot among each venue's bodies, for the venues not on
    /// screen: what the venue menu shows beside them.
    pub(crate) fn venue_statuses(&self, cx: &App) -> HashMap<String, ChatStatus> {
        let mut statuses = HashMap::new();
        for (venue, tabs) in self.venues.sets(&self.workspace) {
            if Some(venue) == self.venues.current() {
                continue;
            }
            if let Some(found) = tabs
                .iter()
                .filter_map(|tab| status(tab.body.chat(), cx))
                .reduce(ChatStatus::max)
            {
                statuses.insert(venue.to_string(), found);
            }
        }
        statuses
    }

    /// Viewing a body clears its "finished" dot. Done at draw, for the reason
    /// [`Luma::sync_venue_modes`] is: every gesture that brings a body on
    /// screen only has to select its mode.
    pub(crate) fn see_front_body(&mut self) {
        if let Some(body) = self.workspace.active_body_mut() {
            body.chat_mut().unseen = false;
        }
    }

    /// Hear the app's running turns: a commit reloads what it changed, and a
    /// turn that ends behind another tab marks its own tab.
    pub(crate) fn running_event(
        &mut self,
        running: Entity<RunningTurns>,
        event: &RunningEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            // A subagent's own commits land on its private draft; its parent
            // announces the change when the draft is published.
            RunningEvent::Event {
                thread,
                event: TurnEvent::DocumentChanged,
            } if running.read(cx).is_running(thread) => self.agent_documents_changed(cx),
            RunningEvent::Ended { thread } => {
                let front = self.workspace.active().cloned();
                let ended: Vec<Target> = self
                    .venues
                    .slots(&self.workspace)
                    .filter(|tab| Some(&tab.target) != front.as_ref())
                    .filter(|tab| {
                        tab.body.chat().panel.read(cx).thread_id() == Some(thread.as_str())
                    })
                    .map(|tab| tab.target.clone())
                    .collect();
                for target in ended {
                    if let Some(body) = self.venues.body_mut(&mut self.workspace, &target) {
                        body.chat_mut().unseen = true;
                    }
                }
                cx.notify();
            }
            RunningEvent::Event { .. } => {}
        }
    }

    fn agent_documents_changed(&mut self, cx: &mut Context<Self>) {
        // A commit can touch several documents, including delegated work. The
        // event deliberately carries no single editor subject.
        self.agent_stale = self.all_targets();
        self.refresh_agent_bodies(cx);
        self.reload_stage(cx);
        self.refresh_score_listing(cx);
    }

    fn all_targets(&self) -> Vec<Target> {
        self.venues
            .slots(&self.workspace)
            .map(|tab| tab.target.clone())
            .collect()
    }

    /// Rows arrived from another device. The same reload, for the same reason:
    /// what is on screen was read from rows that have since changed under it,
    /// and nothing on this device saw the write.
    ///
    /// `tables` is what moved, and it is only used to skip work — a download of
    /// `drafts` alone is a subagent's scratch copy and costs the UI nothing.
    pub(crate) fn replica_changed(&mut self, tables: &[String], cx: &mut Context<Self>) {
        const UNINTERESTING: &[&str] = &["drafts"];
        if tables
            .iter()
            .all(|table| UNINTERESTING.contains(&table.as_str()))
        {
            return;
        }
        self.agent_stale = self.all_targets();
        self.refresh_agent_bodies(cx);
        self.reload_stage(cx);
        self.refresh_score_listing(cx);
        if tables
            .iter()
            .any(|table| table == "venues" || table == "venue_members")
        {
            self.check_open_venue(cx);
        }
        // The venue list, when it is the thing on screen: a venue shared with
        // this account arrives as rows and nothing else would say so. In
        // place, so a download never throws away a half-typed code or the
        // venue being shared.
        if matches!(self.overlay.get(), Some(crate::shell::Overlay::Venues(_))) {
            self.refresh_venues(cx);
        }
        // The chat list is a listing of rows, so it is stale the moment a
        // conversation arrives — but only while it is the thing on screen.
        if matches!(
            self.overlay.get(),
            Some(crate::shell::Overlay::ChatHistory(_))
        ) {
            self.refresh_chat_history(cx);
        }
    }

    /// Reload the live tabs an agent commit or a sync left stale. Parked tabs
    /// stay marked until their venue comes back on screen.
    pub(crate) fn refresh_agent_bodies(&mut self, cx: &mut Context<Self>) {
        if self.agent_stale.is_empty() {
            return;
        }
        let targets: Vec<_> = self
            .workspace
            .iter()
            .map(|tab| tab.target.clone())
            .collect();
        for target in targets {
            if !self.agent_stale.contains(&target) {
                continue;
            }
            self.agent_stale.retain(|stale| stale != &target);
            match &target {
                Target::Score { .. } => self.reload_score_contents(target.clone(), cx),
                Target::Venue { venue } => self.reload_patch(venue.clone(), cx),
                Target::Perform { .. } => self.reload_perform_set(&target, cx),
            }
        }
    }
}

fn status(chat: &BodyChat, cx: &App) -> Option<ChatStatus> {
    if chat.panel.read(cx).is_streaming() {
        Some(ChatStatus::Working)
    } else {
        chat.unseen.then_some(ChatStatus::Finished)
    }
}
