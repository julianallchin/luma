//! Which conversation the shell implies, and the centre that shows it.
//!
//! The chat is a **region**, not a panel: it is the shell's centre, it cannot
//! be closed, and only its subject varies. What the conversation is *about* is
//! [`scope_for`] and nowhere else, so the rule that `pattern_graph` requires
//! an implementation and `track_copilot` forbids one is stated once — the
//! durable model enforces the same rule, and two statements of it would be two
//! chances to disagree.
//!
//! A shell that has *never* shown a subject has no scope, and the centre sits
//! *unattached*: an opening that says what it could attach to, with no
//! composer under it, because there is no thread for a send to land in. Once
//! a thread exists it is sticky: a tab that names no subject (the visualizer)
//! or an emptied workspace does not take the conversation away — only a
//! *different* subject re-points the centre.

use gpui::{AppContext as _, Context, Window};
use luma_chat::AgentChat;
use luma_lib::agent::{AgentKind, SubjectKind, ThreadScope};

use crate::shell::Body;
use crate::Luma;

/// The conversation the shell is about right now, or `None` when nothing an
/// agent can work on is up.
///
/// The visible tab decides: a track editor names a `(track, venue, score)`, a
/// graph names a `(pattern, implementation)`. The sidebar's selection alone
/// does not — a track thread's scope names the score it edits, and only the
/// editor knows one. When thread history lands (spec §7 P5) the sidebar row
/// will resolve its own score; until then the row opens the editor, which
/// amounts to the same conversation.
pub(crate) fn scope_for(app: &Luma) -> Option<ThreadScope> {
    // Each editor gets its companion agent, keyed on the tab in *front*: the
    // web app pairs the track sidebar with the track agent and the pattern
    // editor's Agent tab with the graph agent, and front-tab scoping is that
    // same contract in a single-chat-column shell. An active graph tab is the
    // pattern-graph conversation; tabs that name no agent of their own (the
    // patch) fall back to the track being worked on, so glancing at the rig
    // does not end a track conversation. Deliberately the dumbest switch that
    // makes both agents reachable: the eventual design injects the open
    // view's context into the user message instead of deriving agent identity
    // from a tab, and supersedes this.
    if let Some(Body::Graph(editor)) = app.workspace.active_body() {
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
    current_track(app)
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
    /// Keep the centre pointed at the shell's subject: build the chat on the
    /// first frame (its composer needs a `Window`), and re-point it when a
    /// different subject appears. A subject-less view does not erase an
    /// already attached conversation.
    ///
    /// Done at draw rather than at every navigation for the reason
    /// [`Luma::take_focus`] is: a navigation is a field assignment, and a
    /// gesture that forgot to ask would leave the centre showing somebody
    /// else's conversation. Comparing the whole scope, not just its presence,
    /// is what stops a chat about one pattern following the eye to the next.
    pub(crate) fn sync_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = scope_for(self);
        if let Some(chat) = &self.chat {
            let current = chat.read(cx).scope();
            if current == wanted.as_ref() {
                return;
            }
            // The conversation is stickier than the eye: a tab that names no
            // subject (the visualizer), or no tab at all, implies no *new*
            // conversation — it does not end the one in progress. Rebuilding
            // here would also drop a turn in flight, since a `TurnStream`
            // cancels on drop.
            if wanted.is_none() && current.is_some() {
                return;
            }
        }
        let agent = self.library.agent();
        let chat = cx.new(|cx| AgentChat::new(agent, wanted, window, cx));
        // The panel cannot open a modal — overlays are the shell's to mount —
        // so rewind arrives here as a request. Held with the chat: a re-pointed
        // centre builds a new entity, and a subscription to the old one would
        // be a button that stopped working after the first navigation.
        self.chat_subscription = Some(cx.subscribe(&chat, |this, _, event, cx| match event {
            luma_chat::ChatEvent::HistoryRequested => this.show_chat_history(cx),
        }));
        self.chat = Some(chat);
        cx.notify();
    }
}
