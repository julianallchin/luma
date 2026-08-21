//! Which conversation the screen implies, and the panel that shows it.
//!
//! The chat is **orthogonal to [`Screen`]**, not a variant of it: it opens
//! over whatever is showing, and the screen only decides what the conversation
//! is *about*. That decision is [`scope_for`] and nowhere else, so the rule
//! that `pattern_graph` requires an implementation and `track_copilot` forbids
//! one is stated once — the durable model enforces the same rule, and two
//! statements of it would be two chances to disagree.

use gpui::{AppContext as _, Context, Window};
use luma_chat::AgentChat;
use luma_lib::agent::{AgentKind, SubjectKind, ThreadScope};

use crate::{Luma, Screen};

/// The conversation `screen` is about, or `None` for a screen that is not
/// about anything an agent can work on.
///
/// The track editor is deliberately absent: its state does not publish the
/// track and venue a scope needs, and widening it is that screen's change to
/// make, not this one's. When it does, the arm is three lines here.
pub(crate) fn scope_for(screen: &Screen) -> Option<ThreadScope> {
    match screen {
        Screen::Graph(editor) => {
            let (pattern, implementation) = editor.subject()?;
            Some(ThreadScope {
                agent_kind: AgentKind::PatternGraph,
                subject_kind: SubjectKind::Pattern,
                subject_id: pattern,
                implementation_id: Some(implementation),
                venue_id: None,
                score_id: None,
            })
        }
        _ => None,
    }
}

impl Luma {
    /// Show or hide the chat over the current screen.
    ///
    /// The panel is built on first use and then *kept*: closing it is a width
    /// of zero, not a teardown, so reopening a conversation does not re-read
    /// it — and a turn that is running keeps running while it is hidden,
    /// because the turn belongs to the thread and not to the panel.
    pub(crate) fn toggle_agent_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.toggle(cx));
            cx.notify();
            return;
        }
        let Some(scope) = scope_for(&self.screen) else {
            return;
        };
        let agent = self.library.agent();
        self.chat = Some(cx.new(|cx| AgentChat::new(agent, scope, window, cx)));
        cx.notify();
    }

    /// Drop a panel whose conversation the screen no longer implies.
    ///
    /// Done at draw rather than at every navigation, for the reason
    /// [`Luma::take_focus`] is: a navigation is a field assignment, and a
    /// screen that forgot to ask would be a screen showing somebody else's
    /// conversation. Comparing the whole scope, not just its presence, is what
    /// stops a chat about one pattern following the eye to the next.
    pub(crate) fn retire_agent_chat(&mut self, cx: &mut Context<Self>) {
        let wanted = scope_for(&self.screen);
        let stale = self
            .chat
            .as_ref()
            .is_some_and(|chat| Some(chat.read(cx).scope()) != wanted.as_ref());
        if stale {
            self.chat = None;
        }
    }
}
