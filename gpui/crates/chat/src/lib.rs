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
//!  ├ scope      ThreadScope  which conversation, derived from the screen
//!  ├ transcript Transcript   luma_lib's type, held — never mirrored
//!  ├ rows       Vec<Row>     render state beside it, one per message
//!  ├ list       ListState    the virtualized transcript
//!  ├ composer   TextareaState
//!  └ turn       TurnState    Idle | Streaming(Task)
//! ```
//!
//! The panel is **orthogonal to the screen**, not a variant of it: chat opens
//! *over* whatever is showing, and its [`ThreadScope`] is derived from that
//! screen by one function on the host's side.
//!
//! # Streaming
//!
//! One [`TurnEvent`] arrives, [`luma_lib::agent::apply`] folds it into the
//! transcript, the row it named re-syncs its parser, and exactly that row is
//! remeasured. Remeasuring the list is the frame budget; remeasuring one row
//! is not. The fade over the new characters is paint only — see `luma-md`.

pub mod chip;
pub mod composer;
pub mod motion;
pub mod theme;
pub mod transcript;

use std::time::Instant;

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

/// A manually driven width tween.
///
/// Not `with_animation`: the container's width is animated while a
/// **fixed-width inner** is clipped by it, so the panel's content never
/// reflows mid-transition. Layout is the enemy — animate geometry that clips,
/// never geometry that measures.
struct WidthTween {
    from: f32,
    to: f32,
    started: Instant,
}

pub struct AgentChat {
    agent: Agent,
    scope: ThreadScope,
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
    open: bool,
    tween: Option<WidthTween>,
    focus: FocusHandle,
    theme: Theme,
}

impl AgentChat {
    /// Open a chat on `scope` and start resolving its thread.
    pub fn new(
        agent: Agent,
        scope: ThreadScope,
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
            open: true,
            tween: Some(WidthTween {
                from: 0.0,
                to: theme::PANEL_WIDTH,
                started: Instant::now(),
            }),
            focus: cx.focus_handle(),
            theme: Theme::dark(),
        };
        chat.load(scope, cx);
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

    /// Which conversation this panel is showing. The host compares it against
    /// what the current screen implies, and retires the panel when they part.
    #[must_use]
    pub fn scope(&self) -> &ThreadScope {
        &self.scope
    }

    /// Whether a turn is running. The composer, the send button and the status
    /// strip all read this one fact.
    pub fn is_streaming(&self) -> bool {
        matches!(self.turn, TurnState::Streaming(_))
    }

    /// Show or hide the panel. The entity survives a close: reopening a
    /// conversation should not re-read it.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        let width = self.width();
        self.tween = Some(WidthTween {
            from: width,
            to: if self.open { theme::PANEL_WIDTH } else { 0.0 },
            started: Instant::now(),
        });
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Escape inside the composer: stop a running turn, or close the panel.
    /// One key, two meanings, ordered by which one the person is more likely
    /// to have meant while something is happening.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if self.is_streaming() {
            self.cancel(cx);
        } else {
            self.toggle(cx);
        }
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

    /// The container's width this frame, and whether it still has to move.
    fn width(&self) -> f32 {
        let target = if self.open { theme::PANEL_WIDTH } else { 0.0 };
        let Some(tween) = &self.tween else {
            return target;
        };
        let raw = tween.started.elapsed().as_secs_f32() * 1000.0
            / (motion::RESIZE.total().as_secs_f32() * 1000.0).max(1.0);
        if raw >= 1.0 {
            return target;
        }
        motion::lerp(tween.from, tween.to, motion::RESIZE.progress(raw))
    }
}

impl Render for AgentChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A tween that has not landed asks for the next frame from the one
        // place that knows the geometry is still moving.
        let width = self.width();
        let target = if self.open { theme::PANEL_WIDTH } else { 0.0 };
        if (width - target).abs() > 0.5 {
            window.request_animation_frame();
        }

        // A fixed-width inner inside a clipping container: the content is laid
        // out at its final width for the whole transition, so nothing reflows
        // while the panel slides.
        div()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .w(px(width))
            .track_focus(&self.focus)
            .child(self.body(window, cx))
    }
}

impl AgentChat {
    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme.clone();
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
                (Some(row), Some(message)) => {
                    transcript::row(row, message, live == Some(ix), &state.theme, window)
                }
                _ => div().into_any_element(),
            }
        })
        .size_full();

        div()
            .w(px(theme::PANEL_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            // The seam against the app: a slice of the brutalist trim, so the
            // two design languages meet on the app's terms.
            .border_l_1()
            .border_color(luma_ui::ladder::trim())
            .bg(theme.bg)
            .text_color(theme.text)
            .child(self.header(&theme))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .px(px(theme::SPACE_LG))
                    // Painted first, so each frame's selection registry holds
                    // exactly that frame's visible text in paint order. Not
                    // optional: the registry is a thread-local that every
                    // painted text element pushes into, and nothing else
                    // empties it.
                    .child(luma_md::render::selection_frame_reset())
                    .when(self.transcript.messages.is_empty(), |el| {
                        el.child(empty_state(self.scope.agent_kind, &this, &theme))
                    })
                    .when(!self.transcript.messages.is_empty(), |el| {
                        el.child(transcript_list).child(fade_band(&theme))
                    }),
            )
            .child(status_strip(streaming, self.error.as_deref(), &theme, cx))
            .child(composer::composer(
                &this,
                &self.composer,
                streaming,
                self.model.as_deref(),
                &theme,
                window,
                cx,
            ))
            .into_any_element()
    }

    fn header(&self, theme: &Theme) -> impl IntoElement {
        let title: SharedString = match self.scope.agent_kind {
            luma_lib::agent::AgentKind::TrackCopilot => "Track agent".into(),
            luma_lib::agent::AgentKind::PatternGraph => "Pattern agent".into(),
        };
        div()
            .h(px(theme::HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .px(px(theme::SPACE_LG))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .child(title.clone())
            .agent_node(NodeRole::Text, title)
    }
}

/// The last band of the transcript, dissolved into the panel's own ground.
///
/// A painted overlay rather than gpui's `EdgeFade`, which this pin does not
/// have: the panel is opaque, so a gradient to [`Theme::bg`] *is* the fade.
/// Without it the last line of a reply butts against the composer's plate and
/// the two read as one control.
fn fade_band(theme: &Theme) -> impl IntoElement {
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(px(theme::TRANSCRIPT_FADE_BAND))
        .bg(linear_gradient(
            0.0,
            linear_color_stop(theme.bg, 0.0),
            linear_color_stop(theme.bg.opacity(0.0), 1.0),
        ))
}

/// What an agent is for, in the words of the thing it is looking at, and three
/// questions worth asking it.
///
/// The copy deliberately does not restate the composer's placeholder: an empty
/// state that paraphrases the field below it is a stub with two voices.
struct Opening {
    blurb: &'static str,
    prompts: [&'static str; 3],
}

impl Opening {
    fn of(kind: luma_lib::agent::AgentKind) -> Self {
        match kind {
            luma_lib::agent::AgentKind::PatternGraph => Self {
                blurb: "It reads the graph, runs Python against it, and says what it finds.",
                prompts: [
                    "Explain what this graph does",
                    "Why is the output flat?",
                    "Suggest a change to the ramp",
                ],
            },
            luma_lib::agent::AgentKind::TrackCopilot => Self {
                blurb:
                    "It reads the track's analysis, runs Python against it, and says what it finds.",
                prompts: [
                    "Summarise this track",
                    "Where are the drops?",
                    "Check the beat grid",
                ],
            },
        }
    }
}

/// A conversation that has not started: a mark, a headline, what the agent can
/// do, and three prompts that fill the composer.
fn empty_state(
    kind: luma_lib::agent::AgentKind,
    chat: &Entity<AgentChat>,
    theme: &Theme,
) -> impl IntoElement {
    const HEADLINE: &str = "Where do you want to start?";
    let opening = Opening::of(kind);
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
                        .bg(theme::ink(0.05))
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
                        .child(SharedString::from(HEADLINE))
                        .agent_node(NodeRole::Text, HEADLINE),
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
                .child(
                    div()
                        .mt(px(theme::SPACE_XS))
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(theme::SPACE_SM))
                        .children(
                            opening
                                .prompts
                                .into_iter()
                                .enumerate()
                                .map(|(ix, prompt)| suggestion(ix, prompt, chat, theme)),
                        ),
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
        .bg(theme::ink(0.035))
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .cursor_pointer()
        .hover(|style| style.bg(theme::ink(0.08)))
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
        // The transcript's inset: the strip is the tail of what is being read,
        // not a label on the composer below it.
        .px(px(theme::SPACE_LG))
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
    strip.into_any_element()
}
