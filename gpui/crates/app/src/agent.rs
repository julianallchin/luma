//! Which conversation the shell implies, and the centre that shows it.
//!
//! The chat is a **region**, not a panel: it is the shell's centre, it cannot
//! be closed, and only its subject varies. What the conversation is *about* is
//! [`scope_for`] and nowhere else, so the rule that `pattern_graph` requires
//! an implementation and `track_copilot` forbids one is stated once — the
//! durable model enforces the same rule, and two statements of it would be two
//! chances to disagree.
//!
//! A shell showing nothing an agent can work on — no tab open — has no scope,
//! and the centre sits *unattached*: an opening that says what it could attach
//! to, with no composer under it, because there is no thread for a send to
//! land in.

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
    match app.workspace.active_body()? {
        Body::TrackEditor(state) => {
            let (track, venue, score) = state.subject()?;
            Some(ThreadScope::track(track, venue, score))
        }
        Body::Graph(editor) => {
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
        Body::Visualizer(_) => None,
    }
}

impl Luma {
    /// Keep the centre pointed at the shell's subject: build the chat on the
    /// first frame (its composer needs a `Window`), and re-point it when the
    /// subject changes — up to and including to nothing.
    ///
    /// Done at draw rather than at every navigation for the reason
    /// [`Luma::take_focus`] is: a navigation is a field assignment, and a
    /// gesture that forgot to ask would leave the centre showing somebody
    /// else's conversation. Comparing the whole scope, not just its presence,
    /// is what stops a chat about one pattern following the eye to the next.
    pub(crate) fn sync_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = scope_for(self);
        if let Some(chat) = &self.chat {
            if chat.read(cx).scope() == wanted.as_ref() {
                return;
            }
        }
        let agent = self.library.agent();
        self.chat = Some(cx.new(|cx| AgentChat::new(agent, wanted, window, cx)));
        cx.notify();
    }
}
