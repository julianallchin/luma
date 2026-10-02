//! Each tab owns its chat. A score tab's chats are that score's; the venue
//! tab's chats are the venue's (`docs/specs/venue-tabs.md` rules 3 and 4).
//!
//! Every panel shares the app's one [`RunningTurns`], so a turn belongs to its
//! thread and not to whichever tab is in front: switching tabs or venues lets
//! go of the view and leaves the turn running.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{div, px, App, Context, Entity, Subscription};
use luma_chat::{AgentChat, RunningEvent, RunningTurns};
use luma_lib::agent::{ThreadScope, TurnContext, TurnEvent};
use luma_ui::ladder;
use luma_ui::node::{Instrument, Role};

use crate::shell::Body;
use crate::tabs::Target;
use crate::Luma;

/// One tab's chat panel, and what the tab's status dot needs beyond it.
pub(crate) struct TabChat {
    pub(crate) panel: Entity<AgentChat>,
    /// The panel's turn ended while the tab was not in front. Cleared when the
    /// tab is viewed.
    unseen: bool,
    _requests: Subscription,
}

/// What a tab's dot says. There is no "needs input": no turn event asks the
/// reader anything yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabStatus {
    Working,
    Finished,
}

impl TabStatus {
    /// The stronger of two: a working tab outranks one that finished.
    fn max(self, other: Self) -> Self {
        if self == Self::Working || other == Self::Working {
            Self::Working
        } else {
            Self::Finished
        }
    }
}

/// The dot itself, in the size and recipe of the sidebar's coverage dot.
/// `subject` names it in the automation tree.
pub(crate) fn status_dot(status: TabStatus, subject: &str) -> impl IntoElement {
    let (color, word) = match status {
        TabStatus::Working => (ladder::status_warn(), "working"),
        TabStatus::Finished => (ladder::status_ok(), "finished"),
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
    }
}

/// What a message sent from `target`'s chat is about: the tab's subject, and
/// how its timeline stands when it is a score.
fn turn_context(app: &Luma, target: &Target) -> TurnContext {
    let editor = match app.parked.body(&app.workspace, target) {
        Some(Body::TrackEditor(editor)) => Some(editor.agent_context()),
        _ => None,
    };
    TurnContext {
        scope: Some(scope(target)),
        editor,
    }
}

impl Body {
    pub(crate) fn chat(&self) -> &TabChat {
        match self {
            Self::TrackEditor(state) => &state.chat,
            Self::Patch(state) => &state.chat,
        }
    }

    fn chat_mut(&mut self) -> &mut TabChat {
        match self {
            Self::TrackEditor(state) => &mut state.chat,
            Self::Patch(state) => &mut state.chat,
        }
    }
}

impl Luma {
    /// A chat panel for `target`. It shows `thread` when one is named — the
    /// chat a restored tab had open — and else the subject's newest chat.
    pub(crate) fn tab_chat(
        &mut self,
        target: &Target,
        thread: Option<&str>,
        cx: &mut Context<Self>,
    ) -> TabChat {
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
                    },
                    |app| turn_context(app.read(cx), &source),
                )
            }));
            chat
        });
        let requests = cx.subscribe(&panel, |this, _, event, cx| match event {
            luma_chat::ChatEvent::HistoryRequested => this.show_chat_history(cx),
            luma_chat::ChatEvent::SubagentsRequested(child) => {
                this.show_subagents(child.clone(), cx);
            }
        });
        TabChat {
            panel,
            unseen: false,
            _requests: requests,
        }
    }

    /// The front tab's chat panel, which is the one on screen.
    pub(crate) fn front_chat(&self) -> Option<&Entity<AgentChat>> {
        self.workspace.active_body().map(|body| &body.chat().panel)
    }

    /// `target`'s dot, wherever its tab is. `None` for no dot, and for a
    /// target with no open tab.
    pub(crate) fn tab_status(&self, target: &Target, cx: &App) -> Option<TabStatus> {
        status(self.parked.body(&self.workspace, target)?.chat(), cx)
    }

    /// The dots of the venue on screen, by tab: what the sidebar's rows show.
    pub(crate) fn tab_statuses(&self, cx: &App) -> HashMap<Target, TabStatus> {
        self.workspace
            .iter()
            .filter_map(|tab| Some((tab.target.clone(), status(tab.body.chat(), cx)?)))
            .collect()
    }

    /// The strongest dot among each venue's tabs, for the venues not on
    /// screen: what the venue picker shows beside them.
    pub(crate) fn venue_statuses(&self, cx: &App) -> HashMap<String, TabStatus> {
        let mut statuses = HashMap::new();
        for (venue, tabs) in self.parked.sets(&self.workspace) {
            if Some(venue) == self.parked.current() {
                continue;
            }
            if let Some(found) = tabs
                .iter()
                .filter_map(|tab| status(tab.body.chat(), cx))
                .reduce(TabStatus::max)
            {
                statuses.insert(venue.to_string(), found);
            }
        }
        statuses
    }

    /// Viewing a tab clears its "finished" dot. Done at draw, for the reason
    /// [`Luma::sync_venue_tabs`] is: every gesture that brings a tab to the
    /// front only has to select it.
    pub(crate) fn see_front_tab(&mut self) {
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
                    .parked
                    .tabs(&self.workspace)
                    .filter(|tab| Some(&tab.target) != front.as_ref())
                    .filter(|tab| {
                        tab.body.chat().panel.read(cx).thread_id() == Some(thread.as_str())
                    })
                    .map(|tab| tab.target.clone())
                    .collect();
                for target in ended {
                    if let Some(body) = self.parked.body_mut(&mut self.workspace, &target) {
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
        self.agent_stale_tabs = self.all_targets();
        self.refresh_agent_tabs(cx);
        self.reload_stage(cx);
        self.refresh_score_listing(cx);
    }

    fn all_targets(&self) -> Vec<Target> {
        self.parked
            .tabs(&self.workspace)
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
        self.agent_stale_tabs = self.all_targets();
        self.refresh_agent_tabs(cx);
        self.reload_stage(cx);
        self.refresh_score_listing(cx);
        // The venue list, when it is the thing on screen: a venue shared with
        // this account arrives as rows and nothing else would say so.
        if matches!(self.overlay.get(), Some(crate::shell::Overlay::Venues(_))) {
            self.show_venues(cx);
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
    pub(crate) fn refresh_agent_tabs(&mut self, cx: &mut Context<Self>) {
        if self.agent_stale_tabs.is_empty() {
            return;
        }
        let targets: Vec<_> = self
            .workspace
            .iter()
            .map(|tab| tab.target.clone())
            .collect();
        for target in targets {
            if !self.agent_stale_tabs.contains(&target) {
                continue;
            }
            self.agent_stale_tabs.retain(|stale| stale != &target);
            match &target {
                Target::Score { .. } => self.reload_score_contents(target.clone(), cx),
                Target::Venue { venue } => self.reload_patch(venue.clone(), cx),
            }
        }
    }
}

fn status(chat: &TabChat, cx: &App) -> Option<TabStatus> {
    if chat.panel.read(cx).is_streaming() {
        Some(TabStatus::Working)
    } else {
        chat.unseen.then_some(TabStatus::Finished)
    }
}
