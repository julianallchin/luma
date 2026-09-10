//! The shell keeps one conversation open. Editor context seeds the first
//! conversation; navigation updates the working context without replacing it.

use gpui::{AppContext as _, Context, Window};
use luma_chat::AgentChat;
use luma_lib::agent::ThreadScope;

use crate::shell::Body;
use crate::Luma;

/// Working context for the next turn, independent of the conversation.
pub(crate) fn scope_for(app: &Luma) -> Option<ThreadScope> {
    if let Some(scope) = current_track(app) {
        return Some(scope);
    }
    if let Some(Body::Graph(editor)) = app.workspace.active_body() {
        if let Some((track, venue, score)) = editor.score_subject() {
            return Some(ThreadScope::track(track, venue, score));
        }
    }
    app.sidebar
        .as_ref()
        .map(|browser| ThreadScope::venue(browser.venue_id()))
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
    /// Keep one chat entity and refresh its working context for the next turn.
    pub(crate) fn sync_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = scope_for(self);
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_editor_context(wanted, cx));
            return;
        }
        let agent = self.library.agent();
        let chat = cx.new(|cx| AgentChat::new(agent, wanted, window, cx));
        self.chat_subscription =
            Some(
                cx.subscribe_in(&chat, window, |this, _, event, window, cx| match event {
                    luma_chat::ChatEvent::DocumentChanged => this.agent_documents_changed(cx),
                    luma_chat::ChatEvent::HistoryRequested => this.show_chat_history(cx),
                    luma_chat::ChatEvent::SubagentsRequested(child) => {
                        this.show_subagents(child.clone(), window, cx);
                    }
                }),
            );
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
                Some(Body::Graph(_)) => {}
                Some(Body::Patch(_)) => {
                    if let Some(venue) = target.venue() {
                        self.reload_patch(venue.to_owned(), cx);
                    }
                }
                _ => {}
            }
        }
    }
}
