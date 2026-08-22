//! The agent chat panel.
//!
//! # The one style exception
//!
//! This crate and `luma-md` paint comet's language — translucency, sliding
//! motion, polished streaming markdown — and the rest of the app keeps the
//! brutalist no-animation contract in `CLAUDE.md`. That is Julian's explicit
//! call (`docs/specs/agent-chat-gpui.md` §0), and the scoping is the crate
//! boundary: [`crate::theme`] is not `luma_ui::ladder`, nothing outside these
//! two crates may import it, and the panel's outer edge against the app is a
//! brutalist trim seam so the two languages meet on the app's terms.
//!
//! # Shape
//!
//! ```text
//! AgentChat                  one thread, one composer
//!  ├ agent      Agent        the loop, on the reactor its database needs
//!  ├ scope      Option<..>   which conversation, derived from the screen
//!  ├ transcript Transcript   luma_lib's type, held — never mirrored
//!  ├ rows       Vec<Row>     render state beside it, one per message
//!  ├ list       ListState    the virtualized transcript
//!  ├ composer   TextareaState
//!  ├ collapsed  HashSet      which tool chips the reader has closed
//!  └ turn       TurnState    Idle | Streaming(Task)
//! ```
//!
//! The panel is **orthogonal to the screen**, not a variant of it: chat opens
//! *over* whatever is showing, and its [`ThreadScope`] is derived from that
//! screen by one function on the host's side. A screen that names no subject
//! yields no scope, and the panel opens *unattached* — an opening that says
//! what it could attach to, with no composer under it, because there is no
//! thread for a send to land in.
//!
//! # Streaming
//!
//! One [`TurnEvent`] arrives, [`luma_lib::agent::apply`] folds it into the
//! transcript, the row it named re-syncs its parser, and exactly that row is
//! remeasured. Remeasuring the list is the frame budget; remeasuring one row
//! is not. The fade over the new characters is paint only — see `luma-md`.

pub mod chip;
pub mod composer;
use luma_ui::motion;
pub mod theme;
pub mod transcript;

use std::collections::HashSet;

use gpui::{
    div, linear_color_stop, linear_gradient, list, prelude::*, px, AnyElement, Context, Entity,
    FocusHandle, Focusable as _, ListAlignment, ListState, SharedString, Task, Window,
};
use gpui_component::input::TextareaState;
use gpui_component::{Icon, IconName};
use luma_lib::agent::{AgentService, ThreadScope, Transcript, TurnEvent, TurnOutcome, UserPrompt};
use luma_lib::models::agent_threads::AgentThreadDetail;
use luma_ui::node::{Instrument, Role as NodeRole};

use crate::theme::Theme;
use crate::transcript::Row;

// -- the loop, on its own reactor --------------------------------------------

/// The chat's door to [`luma_lib::agent`].
///
/// [`AgentService::turn`] hands back a stream whose *work* is polled by
/// whoever reads it, and that work is `sqlx` — which needs the Tokio reactor
/// the host's database runs on and gpui does not have. So every call is made
/// on that runtime, and a turn's events are forwarded here over a channel.
///
/// The runtime's cancellation contract survives the hop: dropping [`Turn`]
/// closes the channel, the forwarder's next send fails, and it drops the
/// `TurnStream` — which is what cancels the turn, Python cell included.
#[derive(Clone)]
pub struct Agent {
    service: AgentService,
    runtime: tokio::runtime::Handle,
}

impl Agent {
    #[must_use]
    pub fn new(service: AgentService, runtime: tokio::runtime::Handle) -> Self {
        Self { service, runtime }
    }

    /// The conversation this scope names, creating one if it is new.
    pub fn resolve_thread(
        &self,
        scope: ThreadScope,
    ) -> impl std::future::Future<Output = Result<AgentThreadDetail, String>> + use<> {
        let service = self.service.clone();
        let task = self.runtime.spawn(async move {
            service
                .resolve_thread(&scope)
                .await
                .map_err(|e| e.to_string())
        });
        async move { task.await.map_err(|error| error.to_string())? }
    }

    /// What the next turn's model is called. `None` once the settings read
    /// fails — the composer's chip is a readout, and a panel that refused to
    /// open because it could not name a model would be the wrong trade.
    pub fn model_label(&self) -> impl std::future::Future<Output = Option<String>> + use<> {
        let service = self.service.clone();
        let task = self
            .runtime
            .spawn(async move { service.model_label().await.ok() });
        async move { task.await.ok().flatten().map(ToString::to_string) }
    }

    /// Start a turn. Nothing happens until [`Turn::next`] is awaited.
    #[must_use]
    pub fn turn(&self, thread_id: &str, prompt: String) -> Turn {
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        let service = self.service.clone();
        let thread = thread_id.to_string();
        self.runtime.spawn(async move {
            use futures::StreamExt as _;
            let mut stream = service.turn(&thread, UserPrompt::from(prompt));
            while let Some(event) = stream.next().await {
                if events.send(event).is_err() {
                    break;
                }
            }
        });
        Turn(rx)
    }
}

/// One turn's events, in order. Dropping it cancels the turn.
pub struct Turn(tokio::sync::mpsc::UnboundedReceiver<TurnEvent>);

impl Turn {
    /// The next event, or `None` once the turn has ended.
    pub async fn next(&mut self) -> Option<TurnEvent> {
        self.0.recv().await
    }
}

// -- the panel ---------------------------------------------------------------

/// Whether a turn is running, and the task driving it. Dropping the task drops
/// the [`Turn`], which cancels — so "cancel" is `self.turn = TurnState::Idle`
/// and there is no second call a caller could forget.
enum TurnState {
    Idle,
    Streaming(#[allow(dead_code)] Task<()>),
}

pub struct AgentChat {
    agent: Agent,
    /// Which conversation, or `None` on a screen that names no subject. An
    /// unattached panel is a real state and not a degenerate one: it opens, it
    /// says what it could attach to, and it never resolves a thread — which is
    /// what keeps "the chat did not open" out of the vocabulary entirely.
    scope: Option<ThreadScope>,
    /// The resolved thread, once it has come back. Until then the composer is
    /// live but a send waits — the alternative is a send that silently starts
    /// a conversation in a thread nobody asked for.
    thread: Option<String>,
    transcript: Transcript,
    rows: Vec<Row>,
    list: ListState,
    composer: Entity<TextareaState>,
    /// What the composer's chip names, once settings have been read.
    model: Option<SharedString>,
    turn: TurnState,
    /// What went wrong, in the panel's own words. Cleared by the next send.
    error: Option<String>,
    /// Tool calls the reader has *closed*, by call id.
    ///
    /// The negative set, because a call's detail is open by default: the work
    /// an agent did is the reason to trust what it says, and a transcript that
    /// hides it behind a chevron nobody presses is a transcript of assertions.
    /// Keyed by the call rather than by row index so a chip keeps its state
    /// while rows arrive above it.
    collapsed: HashSet<SharedString>,
    focus: FocusHandle,
    theme: Theme,
}

impl AgentChat {
    /// Open a chat on `scope`, and start resolving its thread when there is
    /// one. `None` opens the panel unattached — see [`Self::scope`].
    pub fn new(
        agent: Agent,
        scope: Option<ThreadScope>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let chat = Self {
            agent,
            scope: scope.clone(),
            thread: None,
            transcript: Transcript::default(),
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Bottom, px(theme::OVERDRAW_PX)),
            composer: composer::state(window, cx),
            model: None,
            turn: TurnState::Idle,
            error: None,
            collapsed: HashSet::new(),
            focus: cx.focus_handle(),
            theme: Theme::dark(),
        };
        if let Some(scope) = scope {
            chat.load(scope, cx);
        }
        chat.name_model(cx);
        chat
    }

    /// Read the model's name for the composer's chip.
    fn name_model(&self, cx: &mut Context<Self>) {
        let pending = self.agent.model_label();
        cx.spawn(async move |this, cx| {
            let label = pending.await;
            this.update(cx, |this, cx| {
                this.model = label.map(SharedString::from);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read the thread back and seat its history. Restored rows do not fade —
    /// history that dissolved onto the screen every time the panel opened
    /// would read as a reply nobody asked for.
    fn load(&self, scope: ThreadScope, cx: &mut Context<Self>) {
        let pending = self.agent.resolve_thread(scope);
        cx.spawn(async move |this, cx| {
            let resolved = pending.await;
            this.update(cx, |this, cx| {
                match resolved {
                    Ok(detail) => match Transcript::from_rows(&detail.messages) {
                        Ok(transcript) => {
                            this.thread = Some(detail.thread.id);
                            this.seat(transcript);
                        }
                        Err(error) => this.error = Some(error),
                    },
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Replace the whole transcript — the one path that resets the list.
    fn seat(&mut self, transcript: Transcript) {
        self.rows = transcript
            .messages
            .iter()
            .map(|message| {
                let mut row = Row::restored(&message.id);
                row.sync(message);
                row.finish_restoring();
                row
            })
            .collect();
        self.list = ListState::new(
            self.rows.len(),
            ListAlignment::Bottom,
            px(theme::OVERDRAW_PX),
        );
        self.transcript = transcript;
    }

    /// Which conversation this panel is showing, or `None` while it is
    /// unattached. The host compares it against what the current screen
    /// implies, and re-points the panel when they part.
    #[must_use]
    pub fn scope(&self) -> Option<&ThreadScope> {
        self.scope.as_ref()
    }

    /// Whether a turn is running. The composer, the send button and the status
    /// strip all read this one fact.
    pub fn is_streaming(&self) -> bool {
        matches!(self.turn, TurnState::Streaming(_))
    }

    /// Escape inside the composer: stop a running turn. The thread is the
    /// shell's centre and cannot be hidden, so with nothing streaming the key
    /// means nothing here — an escape that dismissed the whole conversation
    /// would be the panel-era reflex pointed at a region.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if self.is_streaming() {
            self.cancel(cx);
        }
    }

    /// Open or close one tool call's detail.
    ///
    /// Remeasures exactly the row the chip is in, for the same reason a
    /// streamed delta does: the height that changed is one row's, and
    /// remeasuring the list is the frame budget.
    pub fn toggle_tool(&mut self, call_id: SharedString, row: usize, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&call_id) {
            self.collapsed.insert(call_id);
        }
        self.list.remeasure_items(row..row + 1);
        cx.notify();
    }

    /// Drop the turn, which cancels it.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.turn = TurnState::Idle;
        cx.notify();
    }

    /// Put a prompt in the composer and leave the caret in it. What the empty
    /// state's prompts do: they *offer* a question, they do not ask it — a
    /// chip that sent on click would spend a turn on a phrasing nobody read.
    pub fn suggest(&mut self, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |state, cx| state.set_value(prompt, window, cx));
        let handle = self.composer.read(cx).focus_handle(cx);
        handle.focus(window, cx);
        cx.notify();
    }

    /// Send what is in the composer.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_streaming() {
            return;
        }
        let prompt = self.composer.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let Some(thread) = self.thread.clone() else {
            self.error = Some("The conversation is still opening.".into());
            cx.notify();
            return;
        };
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.error = None;

        let mut turn = self.agent.turn(&thread, prompt);
        self.turn = TurnState::Streaming(cx.spawn(async move |this, cx| {
            while let Some(event) = turn.next().await {
                if this
                    .update(cx, |this, cx| this.on_event(&event, cx))
                    .is_err()
                {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.turn = TurnState::Idle;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Fold one event and remeasure exactly the row it changed.
    fn on_event(&mut self, event: &TurnEvent, cx: &mut Context<Self>) {
        let applied = luma_lib::agent::apply(&mut self.transcript, event);
        // `apply` is the only thing that appends rows, so growing beside it is
        // the whole of keeping the two in step.
        while self.rows.len() < self.transcript.messages.len() {
            let ix = self.rows.len();
            self.rows
                .push(Row::streaming(&self.transcript.messages[ix].id));
            self.list.splice(ix..ix, 1);
        }
        if let Some(ix) = applied.row {
            if let (Some(row), Some(message)) =
                (self.rows.get_mut(ix), self.transcript.messages.get(ix))
            {
                row.sync(message);
            }
            self.list.remeasure_items(ix..ix + 1);
        }
        if let TurnEvent::TurnEnded {
            outcome: TurnOutcome::Failed { message },
        } = event
        {
            self.error = Some(message.clone());
        }
        cx.notify();
    }
}

impl Render for AgentChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .child(self.body(window, cx))
    }
}

impl AgentChat {
    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme.clone();
        // Unattached: the panel is its own opening and nothing else. No status
        // strip and no composer, because there is no thread for a send to land
        // in — a live field over a conversation that cannot exist would be the
        // silent no-op moved one layer in.
        let Some(kind) = self.scope.as_ref().map(|scope| scope.agent_kind) else {
            return self
                .plate(&theme)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .px(px(theme::CONTENT_GUTTER))
                        .child(opening(&Opening::UNATTACHED, None, &theme)),
                )
                .into_any_element();
        };
        let streaming = self.is_streaming();
        let this = cx.entity();
        let fading = self.rows.iter().any(Row::is_fading);
        if fading {
            window.request_animation_frame();
        }

        let live = transcript::live_row(&self.transcript, streaming);
        let rows = this.clone();
        let transcript_list = list(self.list.clone(), move |ix, window, cx| {
            let state = rows.read(cx);
            match (state.rows.get(ix), state.transcript.messages.get(ix)) {
                (Some(row), Some(message)) => transcript::row(
                    row,
                    message,
                    &transcript::RowCtx {
                        chat: &rows,
                        ix,
                        live: live == Some(ix),
                        collapsed: &state.collapsed,
                        theme: &state.theme,
                    },
                    window,
                ),
                _ => div().into_any_element(),
            }
        })
        .size_full();

        self.plate(&theme)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // The reading column's minimum gutters. The 736 cap lives
                    // on each row (`transcript::row`) because a `list` hands
                    // its items the full width; the gutters live here because
                    // the turn rail lives inside the left one.
                    .px(px(theme::CONTENT_GUTTER))
                    // Painted first, so each frame's selection registry holds
                    // exactly that frame's visible text in paint order. Not
                    // optional: the registry is a thread-local that every
                    // painted text element pushes into, and nothing else
                    // empties it.
                    .child(luma_md::render::selection_frame_reset())
                    .when(self.transcript.messages.is_empty(), |el| {
                        el.child(opening(&Opening::of(kind), Some(&this), &theme))
                    })
                    .when(!self.transcript.messages.is_empty(), |el| {
                        el.child(transcript_list)
                            .child(fade_band())
                            .child(self.rail(&theme))
                    }),
            )
            .child(composer::composer(
                &this,
                &self.composer,
                streaming,
                self.model.as_deref(),
                &theme,
                window,
                cx,
            ))
            .child(status_strip(
                streaming,
                self.error.as_deref(),
                kind,
                &theme,
                cx,
            ))
            .into_any_element()
    }

    /// The turn rail: comet's minimap of the conversation, one tick per user
    /// prompt, living in the reading column's left gutter. Clicking a tick
    /// scrolls its turn into view.
    fn rail(&self, _theme: &Theme) -> gpui::Div {
        let mut ticks = div()
            .absolute()
            .left(px(theme::SPACE_MD))
            .top_0()
            .bottom_0()
            .w(px(theme::SPACE_LG))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(6.));
        let last_user = self
            .transcript
            .messages
            .iter()
            .rposition(|message| matches!(message.role, luma_lib::agent::Role::User));
        for (ix, message) in self.transcript.messages.iter().enumerate() {
            if !matches!(message.role, luma_lib::agent::Role::User) {
                continue;
            }
            let list = self.list.clone();
            let active = Some(ix) == last_user;
            ticks = ticks.child(
                div()
                    .id(SharedString::from(format!("rail-{ix}")))
                    .w(px(14.))
                    .h(px(10.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_click(move |_, _, _| {
                        list.scroll_to_reveal_item(ix);
                    })
                    .child(div().w(px(14.)).h(px(2.)).rounded(px(1.)).bg(if active {
                        theme::ink(0.55)
                    } else {
                        theme::ink(0.18)
                    })),
            );
        }
        ticks
    }

    /// The thread's surface and its header — everything both the attached and
    /// the unattached body sit on, so the two cannot drift apart in the one
    /// place where the chat meets the shell.
    fn plate(&self, theme: &Theme) -> gpui::Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            // No fill of its own: the shell's content card paints the ground
            // (`grey(6)`, comet's `bg`), and a second plane here would put the
            // thread one tone off the pane it *is*.
            .text_color(theme.text)
            .child(self.header(theme))
    }

    fn header(&self, theme: &Theme) -> impl IntoElement {
        let title: SharedString = match self.scope.as_ref().map(|scope| scope.agent_kind) {
            Some(luma_lib::agent::AgentKind::TrackCopilot) => "Track agent".into(),
            Some(luma_lib::agent::AgentKind::PatternGraph) => "Pattern agent".into(),
            None => "Agent".into(),
        };
        let badge: Option<SharedString> = match self.scope.as_ref().map(|scope| scope.agent_kind) {
            Some(luma_lib::agent::AgentKind::TrackCopilot) => Some("Track".into()),
            Some(luma_lib::agent::AgentKind::PatternGraph) => Some("Pattern".into()),
            None => None,
        };
        div()
            .h(px(theme::HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme::SPACE_SM))
            .px(px(theme::SPACE_LG))
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .child(div().child(title.clone()).agent_node(NodeRole::Text, title))
            .children(badge.map(|badge| {
                div()
                    .h(px(18.0))
                    .px(px(theme::SPACE_SM))
                    .flex()
                    .items_center()
                    .rounded(px(theme::CONTROL_RADIUS))
                    .bg(theme::wash(0.06))
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(badge)
            }))
    }
}

/// The last band of the transcript, dissolved into the panel's own ground.
///
/// A painted overlay rather than gpui's `EdgeFade`, which this pin does not
/// have: a gradient to the panel's own ground *is* the fade. Without it the
/// last line of a reply butts against the composer's plate and the two read as
/// one control.
///
/// The stop is the content card's own ground — `grey(6)`, what the shell
/// paints under the thread — because the band has to arrive at exactly the
/// colour the transcript is sitting on: one tone off and the fade reads as a
/// seam stripe instead of a dissolve.
fn fade_band() -> impl IntoElement {
    let ground = theme::grey(6);
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(px(theme::TRANSCRIPT_FADE_BAND))
        .bg(linear_gradient(
            0.0,
            linear_color_stop(ground, 0.0),
            linear_color_stop(ground.opacity(0.0), 1.0),
        ))
}

/// What an agent is for, in the words of the thing it is looking at, and the
/// questions worth asking it.
///
/// The copy deliberately does not restate the composer's placeholder: an empty
/// state that paraphrases the field below it is a stub with two voices.
struct Opening {
    headline: &'static str,
    blurb: &'static str,
    /// A dimmer second line, under the blurb. Only the unattached opening has
    /// one: an attached panel's next move is the composer directly below it,
    /// and a hint pointing at a control the eye is already on is noise.
    hint: Option<&'static str>,
    prompts: &'static [&'static str],
}

impl Opening {
    /// The panel over a screen that names no subject. It offers no prompts
    /// because it has no thread to send one into: what it owes the reader is
    /// the way *out* of this state, which is the blurb.
    const UNATTACHED: Self = Self {
        headline: "Nothing to work on yet",
        blurb: UNATTACHED_BLURB,
        hint: None,
        prompts: &[],
    };

    fn of(kind: luma_lib::agent::AgentKind) -> Self {
        match kind {
            luma_lib::agent::AgentKind::PatternGraph => Self {
                headline: HEADLINE,
                blurb: "It reads the graph, runs Python against it, and says what it finds.",
                hint: None,
                prompts: &[
                    "Explain what this graph does",
                    "Why is the output flat?",
                    "Suggest a change to the ramp",
                ],
            },
            luma_lib::agent::AgentKind::TrackCopilot => Self {
                headline: HEADLINE,
                blurb:
                    "It reads the track's analysis, runs Python against it, and says what it finds.",
                hint: None,
                prompts: &[
                    "Summarise this track",
                    "Where are the drops?",
                    "Check the beat grid",
                ],
            },
        }
    }
}

/// What a conversation that has not started asks the reader.
const HEADLINE: &str = "Where do you want to start?";

/// The way out of an unattached panel, in the panel's own words. Public
/// because it is what the exit gate looks for: a test that spelled the promise
/// itself would pass while the shipped copy said something else.
pub const UNATTACHED_BLURB: &str =
    "Open a pattern's graph or a track's timeline, and the chat attaches to it.";

/// A conversation that has not started: a mark, a headline, what the agent can
/// do, and the prompts that fill the composer.
///
/// `chat` is the panel the prompts send into, and `None` when there is none to
/// send into — which is also when [`Opening::prompts`] is empty, so the two
/// cannot disagree.
fn opening(opening: &Opening, chat: Option<&Entity<AgentChat>>, theme: &Theme) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .pb(px(theme::SPACE_LG))
        .child(
            div()
                .w(px(theme::HERO_WIDTH))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(theme::SPACE_MD))
                .child(
                    div()
                        .size(px(theme::HERO_GLYPH))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme::card_bg())
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            Icon::new(IconName::Bot)
                                .size(px(18.0))
                                .text_color(theme.text_muted),
                        ),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(theme.text)
                        .child(SharedString::from(opening.headline))
                        .agent_node(NodeRole::Text, opening.headline),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_center()
                        .text_color(theme.text_faint)
                        .child(SharedString::from(opening.blurb))
                        .agent_node(NodeRole::Text, opening.blurb),
                )
                .children(opening.hint.map(|hint| {
                    div()
                        .text_size(px(11.0))
                        .text_center()
                        .text_color(theme.text_faint.opacity(0.7))
                        .child(SharedString::from(hint))
                        .agent_node(NodeRole::Text, hint)
                }))
                .child(
                    div()
                        .mt(px(theme::SPACE_XS))
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(theme::SPACE_SM))
                        .children(chat.into_iter().flat_map(|chat| {
                            opening
                                .prompts
                                .iter()
                                .copied()
                                .enumerate()
                                .map(|(ix, prompt)| suggestion(ix, prompt, chat, theme))
                        })),
                ),
        )
}

/// One offered question. Pressing it fills the composer — see
/// [`AgentChat::suggest`] for why it does not also send.
fn suggestion(
    ix: usize,
    prompt: &'static str,
    chat: &Entity<AgentChat>,
    theme: &Theme,
) -> impl IntoElement {
    let pressed = chat.clone();
    div()
        .id(("chat-suggestion", ix))
        .h(px(theme::CHIP_HEIGHT - 6.0))
        .w_full()
        .flex()
        .items_center()
        .px(px(theme::SPACE_MD))
        .rounded(px(theme::CONTROL_RADIUS))
        .bg(theme::card_bg())
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .cursor_pointer()
        .hover(|style| style.bg(theme::glass_hover()))
        .on_click(move |_, window, cx| {
            pressed.update(cx, |this, cx| this.suggest(prompt, window, cx));
        })
        .child(SharedString::from(prompt))
        .agent_node(NodeRole::Button, prompt)
}

/// The reserved strip under the transcript. Always present, so the composer
/// does not shift the moment a turn starts.
fn status_strip(
    streaming: bool,
    error: Option<&str>,
    kind: luma_lib::agent::AgentKind,
    theme: &Theme,
    cx: &mut Context<AgentChat>,
) -> AnyElement {
    let mut strip = div()
        .h(px(theme::STATUS_STRIP_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_SM))
        // Under the composer, as comet keeps it: the strip is the thread's
        // context line, not the transcript's tail. Centered on the same
        // reading column as the plate above it.
        .mx_auto()
        .w_full()
        .max_w(px(theme::MAX_CONTENT_WIDTH + 2.0 * theme::CONTENT_GUTTER))
        .px(px(theme::CONTENT_GUTTER + theme::SPACE_LG))
        .mb(px(theme::SPACE_XS))
        .text_size(px(11.0));
    if let Some(error) = error {
        return strip
            .text_color(theme.danger)
            .child(SharedString::from(error.to_string()))
            .agent_node(NodeRole::Text, error.to_string())
            .into_any_element();
    }
    if streaming {
        // One shared 30fps clock drives every cell, so multi-instance loaders
        // stay phase-locked and an idle window schedules nothing at all.
        let phase = motion::pulse_delta(&motion::PULSE, cx.entity_id(), cx);
        // Cells in the text colour, not `busy`: a saturated hue at the pulse's
        // 0.08 floor paints near-black on this ground and reads as three dead
        // pixels rather than as a loader.
        strip = strip.child(
            div()
                .flex()
                .flex_row()
                .gap(px(theme::SPACE_XS))
                .children((0..3).map(|cell| {
                    let alpha = motion::pulse_opacity(motion::staggered_phase(
                        phase,
                        cell,
                        motion::PULSE_STAGGER,
                    ));
                    div()
                        .size(px(5.0))
                        .rounded_full()
                        .bg(theme.text.opacity(0.25 + 0.75 * alpha))
                })),
        );
        return strip
            .text_color(theme.text_muted)
            .child(SharedString::from("Working"))
            .agent_node(NodeRole::Text, "Working")
            .into_any_element();
    }
    // At rest: what this thread is, faintly — comet's checkout line, in the
    // only vocabulary a light show has.
    let subject = match kind {
        luma_lib::agent::AgentKind::TrackCopilot => "Track thread",
        luma_lib::agent::AgentKind::PatternGraph => "Pattern thread",
    };
    strip
        .text_color(theme.text_faint)
        .child(SharedString::from(subject))
        .child(div().flex_1())
        .child(SharedString::from("⏎ to send"))
        .into_any_element()
}
