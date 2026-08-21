//! Which conversation the screen implies, and the panel that shows it.
//!
//! The chat is **orthogonal to [`Screen`]**, not a variant of it: it opens
//! over whatever is showing, and the screen only decides what the conversation
//! is *about*. That decision is [`scope_for`] and nowhere else, so the rule
//! that `pattern_graph` requires an implementation and `track_copilot` forbids
//! one is stated once — the durable model enforces the same rule, and two
//! statements of it would be two chances to disagree.
//!
//! A screen that is about nothing an agent can work on — the venue grid, a
//! track list, settings — has no scope, and the panel opens *unattached* over
//! it. Which is to say the chat is available everywhere and only its subject
//! varies, rather than the key being live on two screens out of six.

use gpui::{AppContext as _, Context, Window};
use luma_chat::AgentChat;
use luma_lib::agent::{AgentKind, SubjectKind, ThreadScope};

use crate::{Luma, Screen};

/// The conversation `screen` is about, or `None` for a screen that is not
/// about anything an agent can work on.
pub(crate) fn scope_for(screen: &Screen) -> Option<ThreadScope> {
    match screen {
        // A track with no score is a screen with nothing to talk about: the
        // track agent's scope names the score it edits, and `Editor::subject`
        // is `None` until there is one.
        Screen::TrackEditor { state, .. } => {
            let (track, venue, score) = state.subject()?;
            Some(ThreadScope::track(track, venue, score))
        }
        Screen::Graph { state: editor, .. } => {
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
        // `scope_for` may be `None`, and the panel opens anyway: a key that did
        // nothing on four of six screens is indistinguishable from a broken
        // one, and the panel can say what it would attach to far better than
        // silence can.
        self.open_agent_chat(scope_for(&self.screen), window, cx);
    }

    fn open_agent_chat(
        &mut self,
        scope: Option<ThreadScope>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = self.library.agent();
        self.chat = Some(cx.new(|cx| AgentChat::new(agent, scope, window, cx)));
        cx.notify();
    }

    /// Re-point a panel whose conversation the screen no longer implies.
    ///
    /// Done at draw rather than at every navigation, for the reason
    /// [`Luma::take_focus`] is: a navigation is a field assignment, and a
    /// screen that forgot to ask would be a screen showing somebody else's
    /// conversation. Comparing the whole scope, not just its presence, is what
    /// stops a chat about one pattern following the eye to the next.
    ///
    /// An *open* panel follows the eye instead of vanishing from under it: it
    /// is rebuilt on whatever the new screen is about, up to and including
    /// nothing. A closed one is dropped, because a panel at zero width has no
    /// reader to keep faith with and rebuilding it would re-read a thread that
    /// is not on screen.
    pub(crate) fn retire_agent_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = scope_for(&self.screen);
        let Some(chat) = self.chat.as_ref() else {
            return;
        };
        let chat = chat.read(cx);
        if chat.scope() == wanted.as_ref() {
            return;
        }
        let open = chat.is_open();
        if open {
            self.open_agent_chat(wanted, window, cx);
        } else {
            self.chat = None;
        }
    }
}
