//! The shell keeps one chat panel. It shows the open score's chats; the other
//! editors only supply the working context for each turn.

use gpui::{AppContext as _, Context, Window};
use luma_chat::{AgentChat, RunningEvent, RunningTurns};
use luma_lib::agent::{ThreadScope, TurnEvent};

use crate::shell::Body;
use crate::tabs::Target;
use crate::Luma;

/// Working context for the next turn, independent of the conversation.
pub(crate) fn scope_for(app: &Luma) -> Option<ThreadScope> {
    current_track(app)
}

/// Whose chats the thread shows: the score the picked track's editor has
/// open, or `None` when no score is open.
pub(crate) fn chat_subject(app: &Luma) -> Option<ThreadScope> {
    current_track(app)
}

/// An open track remains available while another editor tab is in front.
fn track_scope(body: &Body) -> Option<ThreadScope> {
    let Body::TrackEditor(state) = body else {
        return None;
    };
    let (track, venue, score) = state.subject()?;
    Some(ThreadScope::track(track, venue, score))
}

fn current_track(app: &Luma) -> Option<ThreadScope> {
    app.workspace
        .active_body()
        .and_then(track_scope)
        .or_else(|| {
            app.workspace
                .iter()
                .filter_map(|tab| track_scope(&tab.body))
                .last()
        })
}

impl Luma {
    /// Keep one chat entity on the open score's chats, and refresh its
    /// working context for the next turn.
    ///
    /// The venue page hides the thread and leaves it alone, so the score's
    /// chat, and any turn it is running, is there when the reader comes back.
    pub(crate) fn sync_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let context = scope_for(self);
        let subject = chat_subject(self);
        let follow = !self.venue_mode();
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| {
                chat.set_editor_context(context, cx);
                if follow {
                    chat.set_subject(subject, cx);
                }
            });
            return;
        }
        let agent = self.library.agent();
        // Owned by the chat, which lives as long as the app: quitting drops it,
        // and with it every running turn.
        let running = cx.new(|_| RunningTurns::default());
        let chat = cx.new(|cx| {
            let mut chat = AgentChat::new(agent, running.clone(), None, cx);
            chat.set_editor_context(context, cx);
            chat.set_subject(subject, cx);
            chat
        });
        let requests = cx.subscribe_in(&chat, window, |this, _, event, _, cx| match event {
            luma_chat::ChatEvent::HistoryRequested => this.show_chat_history(cx),
            luma_chat::ChatEvent::SubagentsRequested(child) => {
                this.show_subagents(child.clone(), cx);
            }
        });
        // Heard from the registry rather than the panel: a turn in the
        // background changes documents while the panel shows another chat.
        let commits = cx.subscribe(&running, |this, _, event, cx| {
            if let RunningEvent::Event {
                event: TurnEvent::DocumentChanged,
                ..
            } = event
            {
                this.agent_documents_changed(cx);
            }
        });
        self.chat_subscription = Some(gpui::Subscription::join(requests, commits));
        self.chat = Some(chat);
        cx.notify();
    }
}

impl Luma {
    fn agent_documents_changed(&mut self, cx: &mut Context<Self>) {
        // A commit can touch several documents, including delegated work. The
        // event deliberately carries no single editor subject.
        self.agent_stale_tabs = self.parked.targets(&self.workspace);
        self.refresh_agent_tabs(cx);
        self.reload_stage(cx);
        self.refresh_score_listing(cx);
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
        self.agent_stale_tabs = self.parked.targets(&self.workspace);
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
            self.show_chat_history(cx);
        }
    }

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
            match self.workspace.body(&target) {
                Some(Body::TrackEditor(editor)) => {
                    if let Some(score) = editor.score_id().map(str::to_owned) {
                        self.reload_score_contents(target, score, cx);
                    }
                }
                Some(Body::Patch(_)) => {
                    if let Target::Patch { venue } = &target {
                        self.reload_patch(venue.clone(), cx);
                    }
                }
                _ => {}
            }
        }
    }
}
