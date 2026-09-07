//! The shell keeps one conversation open. Editor context seeds the first
//! conversation and the next explicit new chat; navigation never replaces it.

use gpui::{AppContext as _, Context, Window};
use luma_chat::AgentChat;
use luma_lib::agent::{AgentKind, SubjectKind, ThreadScope};

use crate::shell::Body;
use crate::tabs::Target;
use crate::Luma;

/// Context for a new conversation, chosen from the visible editor.
pub(crate) fn scope_for(app: &Luma) -> Option<ThreadScope> {
    match app.workspace.active() {
        Some(Target::Patch { venue }) => return Some(ThreadScope::venue(venue.clone())),
        _ => {}
    }
    if let Some(Body::Graph(editor)) = app.workspace.active_body() {
        if let Some((track, venue, score)) = editor.score_subject() {
            return Some(ThreadScope::track(track, venue, score));
        }
        let (pattern, implementation) = editor.subject()?;
        return Some(ThreadScope {
            agent_kind: AgentKind::PatternGraph,
            subject_kind: SubjectKind::Pattern,
            subject_id: pattern,
            implementation_id: Some(implementation),
            venue_id: None,
            score_id: None,
        });
    }
    current_track(app).or_else(|| {
        app.sidebar
            .as_ref()
            .map(|browser| ThreadScope::venue(browser.venue_id()))
    })
}

/// The track the workspace is about: the focused editor if one is focused,
/// otherwise the last track editor opened.
///
/// The fallback is what makes the rule hold while a graph tab is in front —
/// without it, switching away from a track would read as "no track", and the
/// thread would drift to whatever the new tab named.
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
    /// Initialize the chat once and update only the context for its + button.
    pub(crate) fn sync_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = scope_for(self);
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_new_thread_scope(wanted, cx));
            return;
        }
        let agent = self.library.agent();
        let chat = cx.new(|cx| AgentChat::new(agent, wanted, window, cx));
        self.chat_subscription =
            Some(
                cx.subscribe_in(&chat, window, |this, _, event, window, cx| match event {
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
