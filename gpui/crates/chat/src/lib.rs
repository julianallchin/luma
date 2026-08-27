//! The agent chat panel.
//!
//! # Which tier this is
//!
//! The thread is a **chrome** surface (`docs/specs/comet-shell.md` §9): comet's
//! language — translucency, sliding motion, polished streaming markdown —
//! against the instrument tier's square, unanimated `luma_ui::ladder`. That
//! used to be a crate boundary, because the chat was the only comet-language
//! surface in the app; the shell itself is chrome now, so the tier is named
//! (`luma_ui::glass`) and shared, and [`crate::theme`] is the *roles* over it
//! rather than a palette of its own. Both tiers read one grey ladder, so the
//! thread column and the timeline beside it cannot drift apart.
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
//!  └ turn       TurnState    Idle | Streaming { task, since, steer }
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
pub mod theme;
pub mod transcript;
pub mod working;

use std::collections::HashSet;

use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Entity, FocusHandle, ListAlignment, ListState,
    SharedString, Task, Window,
};
use gpui_component::{Icon, IconName};
use luma_lib::agent::{AgentService, ThreadScope, Transcript, TurnEvent, TurnOutcome, UserPrompt};
use luma_lib::models::agent_threads::AgentThreadDetail;
use luma_ui::node::{Instrument, Role as NodeRole};

use crate::composer::Composer;
use crate::theme::Theme;
use crate::transcript::{Entry, RowKey};

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

    /// One specific conversation, by id — what the history picker opens.
    ///
    /// Not [`Self::resolve_thread`]: that one answers "the newest thread for
    /// this subject", so routing a picked row through it would open whichever
    /// conversation about that track is newest rather than the one the reader
    /// chose. An id is a conversation's only unambiguous name.
    pub fn open_thread(
        &self,
        thread_id: String,
    ) -> impl std::future::Future<Output = Result<AgentThreadDetail, String>> + use<> {
        let service = self.service.clone();
        let task = self.runtime.spawn(async move {
            service
                .open_thread(&thread_id)
                .await
                .map_err(|e| e.to_string())
        });
        async move { task.await.map_err(|error| error.to_string())? }
    }

    /// A fresh conversation about `scope`, always created — the + button.
    pub fn new_thread(
        &self,
        scope: ThreadScope,
    ) -> impl std::future::Future<Output = Result<AgentThreadDetail, String>> + use<> {
        let service = self.service.clone();
        let task = self
            .runtime
            .spawn(async move { service.new_thread(&scope).await.map_err(|e| e.to_string()) });
        async move { task.await.map_err(|error| error.to_string())? }
    }

    /// Every conversation about `scope`'s subject, newest first, with its
    /// transcripts read for the picker's summaries and grep.
    pub fn history(
        &self,
        scope: ThreadScope,
    ) -> impl std::future::Future<Output = Result<luma_lib::agent::History, String>> + use<> {
        let service = self.service.clone();
        let task = self
            .runtime
            .spawn(async move { service.history(&scope).await.map_err(|e| e.to_string()) });
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

    /// Start a turn. The stream is built here rather than inside the spawned
    /// task so its steering handle can be handed back with it — a turn a host
    /// could not redirect would force the composer to lock while one ran.
    #[must_use]
    pub fn turn(&self, thread_id: &str, prompt: String) -> Turn {
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = self.service.turn(thread_id, UserPrompt::from(prompt));
        let steer = stream.steering();
        self.runtime.spawn(async move {
            use futures::StreamExt as _;
            while let Some(event) = stream.next().await {
                if events.send(event).is_err() {
                    break;
                }
            }
        });
        Turn { events: rx, steer }
    }
}

/// What the panel asks its host for.
///
/// One variant, and it exists because the chat cannot open a modal: overlays
/// are the shell's to mount, and a chat crate that reached for one would invert
/// the dependency. Starting a *new* conversation is not here — that is entirely
/// the panel's own business, so it just does it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatEvent {
    /// The reader pressed rewind: open the history picker over this subject.
    HistoryRequested,
}

/// One turn's events, in order. Dropping it cancels the turn.
pub struct Turn {
    events: tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    steer: luma_lib::agent::TurnSteer,
}

impl Turn {
    /// The next event, or `None` once the turn has ended.
    pub async fn next(&mut self) -> Option<TurnEvent> {
        self.events.recv().await
    }

    /// A handle that redirects this turn from elsewhere. Sending to a turn
    /// that has already ended is a no-op, not an error — the race is ordinary.
    #[must_use]
    pub fn steering(&self) -> luma_lib::agent::TurnSteer {
        self.steer.clone()
    }
}

// -- the panel ---------------------------------------------------------------

/// Whether a turn is running, and the task driving it. Dropping the task drops
/// the [`Turn`], which cancels — so "cancel" is `self.turn = TurnState::Idle`
/// and there is no second call a caller could forget.
enum TurnState {
    Idle,
    /// The task driving the turn, when it started — the working indicator's
    /// timer origin — and the handle that redirects it. All three begin and
    /// end with the turn; separate optional fields would be three things that
    /// could outlive the one they describe.
    Streaming {
        #[allow(dead_code)]
        task: Task<()>,
        since: std::time::Instant,
        steer: luma_lib::agent::TurnSteer,
    },
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
    /// Render state, one per message — parsers, veil, render cache.
    entries: Vec<Entry>,
    /// What the list is sized by: one entry per *block*. Derived from
    /// `transcript` + `turns` on every fold and reconciled by
    /// [`transcript::diff_rows`], so the list only ever remeasures what moved.
    rows: Vec<RowKey>,
    /// The message a running turn is writing into.
    ///
    /// Recorded by the event that touches it, never derived from "the last
    /// message is an assistant one" — between a send and that turn's first
    /// event the derived answer names the *previous* reply, which would put a
    /// settled turn back under the veil.
    live: Option<usize>,
    list: ListState,
    /// The bottom pin. `pinned` is the state, `spring` is the motion; the two
    /// are separate because a pin can be held while the spring is parked (a
    /// settled transcript resting at the bottom schedules no frames at all).
    pinned: bool,
    spring: transcript::StickSpring,
    /// Last tick's wall clock, for the elapsed-frames conversion, and last
    /// tick's distance, which is what makes the re-stick rule direction-aware.
    spring_tick: Option<std::time::Instant>,
    spring_settled: Option<std::time::Instant>,
    distance: f32,
    /// Reached by `crate::composer` from inside the panel's render, which is
    /// why it is crate-visible rather than private: the plate's layout state
    /// belongs to the composer, and passing it back through the entity would
    /// re-enter a borrow that is already live.
    pub(crate) composer: Composer,
    /// What the composer's chip names, once settings have been read.
    model: Option<SharedString>,
    turn: TurnState,
    /// Which row currently carries the working indicator, so the one row whose
    /// height it changes can be remeasured when it moves. Derived state, kept
    /// only because `ListState` caches heights and cannot be asked what it
    /// last measured.
    trailer_row: Option<usize>,
    /// What went wrong, in the panel's own words. Cleared by the next send.
    error: Option<String>,
    /// Whether the reader chose this conversation by hand, in which case the
    /// panel stops following the screen — see [`Self::open_thread`].
    ///
    /// Distinct from the bottom pin above, which is about the *viewport*: this
    /// one is about which conversation is on screen at all.
    chosen: bool,
    /// Tool calls the reader has *closed*, by call id.
    ///
    /// The negative set, because a call's detail is open by default: the work
    /// an agent did is the reason to trust what it says, and a transcript that
    /// hides it behind a chevron nobody presses is a transcript of assertions.
    /// Keyed by the call rather than by row index so a chip keeps its state
    /// while rows arrive above it.
    collapsed: HashSet<SharedString>,
    /// The one fold in flight: which call was clicked, when, and which row it
    /// is in so the tween can remeasure it. At most one — a fold is started by
    /// a click, and a click lands on one chip.
    fold: Option<(SharedString, std::time::Instant, usize)>,
    focus: FocusHandle,
    theme: Theme,
}

impl AgentChat {
    /// Open a chat on `scope`, and start resolving its thread when there is
    /// one. `None` opens the panel unattached — see [`Self::scope`].
    pub fn new(
        agent: Agent,
        scope: Option<ThreadScope>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let chat = Self {
            agent,
            scope: scope.clone(),
            thread: None,
            transcript: Transcript::default(),
            entries: Vec::new(),
            rows: Vec::new(),
            live: None,
            list: ListState::new(0, ListAlignment::Bottom, px(theme::OVERDRAW_PX)),
            // A fresh thread opens at the bottom, which is where a conversation
            // is read from.
            pinned: true,
            spring: transcript::StickSpring::default(),
            spring_tick: None,
            spring_settled: None,
            distance: 0.0,
            composer: Composer::new(cx),
            model: None,
            turn: TurnState::Idle,
            trailer_row: None,
            error: None,
            chosen: false,
            collapsed: HashSet::new(),
            fold: None,
            focus: cx.focus_handle(),
            theme: Theme::dark(),
        };
        let mut chat = chat;
        chat.watch_scrolling(cx);
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
            this.update(cx, |this, cx| this.receive(resolved, cx)).ok();
        })
        .detach();
    }

    /// Replace the whole transcript — the one path that resets the list.
    fn seat(&mut self, transcript: Transcript, cx: &mut Context<Self>) {
        self.entries = transcript
            .messages
            .iter()
            .map(|message| {
                let mut turn = Entry::restored(&message.id);
                turn.sync(message, false);
                turn.finish_restoring();
                turn
            })
            .collect();
        self.transcript = transcript;
        self.rows = transcript::rows_for(&self.transcript, &self.entries, None);
        self.list = ListState::new(
            self.rows.len(),
            ListAlignment::Bottom,
            px(theme::OVERDRAW_PX),
        );
        self.trailer_row = None;
        // A fresh list is a fresh scroll handler: the old one watched a
        // `ListState` this panel no longer shows.
        self.watch_scrolling(cx);
        self.pinned = true;
        self.spring.reset();
        self.spring_tick = None;
        self.distance = 0.0;
    }

    /// Rebuild the row list and tell the list exactly what moved.
    ///
    /// The remeasure-vs-splice choice is the whole reason this is one function
    /// rather than a `splice` at each call site. `splice` resets its items to
    /// hint-less `Unmeasured` and, when the viewport's top item falls inside
    /// the range, re-anchors the scroll to the range start. On an equal-count
    /// edit — the live→settled flip, where every version moves and every
    /// identity stays — that is a visible jump at the end of every turn.
    /// `remeasure_items` keeps the old heights as hints and holds the anchor.
    fn reconcile_rows(&mut self) {
        let next = transcript::rows_for(&self.transcript, &self.entries, self.live);
        let Some((old_range, count)) = transcript::diff_rows(&self.rows, &next) else {
            return;
        };
        self.rows = next;
        if old_range.len() == count {
            self.list.remeasure_items(old_range);
        } else {
            self.list.splice(old_range, count);
        }
    }

    // -- the bottom pin ------------------------------------------------------

    /// How far the view sits above the end, in px.
    fn distance_from_bottom(&self) -> f32 {
        let max = f32::from(self.list.max_offset_for_scrollbar().y);
        let current = f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        (max + current).max(0.0)
    }

    /// Watch the reader's own scrolling, and only the reader's.
    ///
    /// The list calls this from its wheel and touch path exclusively —
    /// programmatic scrolls never re-enter it — which is what makes it a safe
    /// place to decide the pin. Without that guarantee the spring's own
    /// `scroll_by` would unpin the view on the frame it started chasing.
    fn watch_scrolling(&mut self, cx: &mut Context<Self>) {
        let list = self.list.clone();
        let chat = cx.weak_entity();
        list.set_scroll_handler(move |_, _, cx| {
            // The list holds its own `RefCell` borrow while dispatching, so
            // reading the state back synchronously panics. Defer to after it
            // has let go.
            let chat = chat.clone();
            cx.defer(move |cx| {
                chat.update(cx, |this, cx| {
                    let distance = this.distance_from_bottom();
                    let previous = std::mem::replace(&mut this.distance, distance);
                    if this.pinned && transcript::should_unpin(distance, previous) {
                        this.pinned = false;
                        this.spring.reset();
                        this.spring_tick = None;
                    } else if !this.pinned && transcript::should_restick(distance, previous) {
                        this.pinned = true;
                    }
                    cx.notify();
                })
                .ok();
            });
        });
    }

    /// One spring frame: observe the target, step, apply the delta, and park
    /// once it has landed and stayed landed.
    ///
    /// Runs after layout, so the measurements it reads are this frame's rather
    /// than the previous frame's — a spring fed stale geometry chases a target
    /// that has already moved and never converges.
    fn step_spring(&mut self) {
        if !self.pinned {
            self.spring_tick = None;
            return;
        }
        let now = std::time::Instant::now();
        let frames = match self.spring_tick {
            Some(last) => (now.duration_since(last).as_secs_f32() * 1000.0
                / theme::SPRING_FRAME_MS)
                .min(theme::SPRING_MAX_CATCHUP_FRAMES),
            None => 1.0,
        };
        self.spring_tick = Some(now);

        let target = f32::from(self.list.max_offset_for_scrollbar().y);
        let mut distance = self.distance_from_bottom();
        // A long jump — opening a thread mid-history, a huge paste — teleports
        // most of the way first. Gliding it would be a slow ride through
        // content nobody asked to see.
        let viewport = f32::from(self.list.viewport_bounds().size.height);
        let glide_max = theme::GLIDE_MAX_VIEWPORTS * viewport;
        if viewport > 0.0 && distance > glide_max {
            self.list.scroll_by(px(distance - glide_max));
            distance = glide_max;
        }
        let pos = target - distance;
        let next = self.spring.step(pos, target, frames);
        if next > pos {
            self.list.scroll_by(px(next - pos));
        }
        self.distance = (target - next).max(0.0);

        if target - next <= 0.5 {
            let settled = *self.spring_settled.get_or_insert(now);
            let grace = std::time::Duration::from_millis(theme::SPRING_SETTLE_GRACE_MS);
            if now.duration_since(settled) >= grace && self.spring.is_idle() {
                // Park. A transcript resting at the bottom must schedule no
                // frames at all — this is the difference between an idle panel
                // costing nothing and pinning a display at its refresh rate.
                self.spring.reset();
                self.spring_tick = None;
            }
        } else {
            self.spring_settled = None;
        }
    }

    /// Re-engage the pin and ride back down — the jump button, and what a send
    /// does so the reply lands in view.
    pub fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.pinned = true;
        self.spring_tick = None;
        self.spring_settled = None;
        cx.notify();
    }

    /// Bring one turn's parse up to its message, at its current liveness.
    fn resync(&mut self, ix: usize) {
        let live = self.live == Some(ix);
        if let (Some(turn), Some(message)) =
            (self.entries.get_mut(ix), self.transcript.messages.get(ix))
        {
            turn.sync(message, live);
        }
    }

    /// Show one specific conversation, by id.
    ///
    /// **Only the thread changes.** The screen underneath — which track is
    /// open, which tab is focused, what the score says — is untouched, because
    /// reading an old conversation is not the same act as going back to what it
    /// was about. The agent re-orients itself from the transcript.
    ///
    /// [`Self::is_chosen`] is what keeps it open: without it the next scope change
    /// would re-resolve the screen's subject and silently swap the reader onto
    /// a different conversation about the same track.
    pub fn open_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        // Whatever is running belongs to the conversation being left.
        self.cancel(cx);
        self.chosen = true;
        self.thread = None;
        self.error = None;
        self.seat(Transcript::default(), cx);
        let pending = self.agent.open_thread(thread_id.to_string());
        cx.spawn(async move |this, cx| {
            let opened = pending.await;
            this.update(cx, |this, cx| {
                this.receive(opened, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Start a fresh conversation about the screen's own subject.
    ///
    /// Always creates: "new chat" is a statement, and resolving would hand back
    /// the existing conversation whenever there was one — the button appearing
    /// to do nothing. Unpinned, because a new chat about *this* screen is
    /// exactly what the screen implies.
    pub fn new_thread(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope.clone() else {
            return;
        };
        self.cancel(cx);
        self.chosen = false;
        self.thread = None;
        self.error = None;
        self.seat(Transcript::default(), cx);
        let pending = self.agent.new_thread(scope);
        cx.spawn(async move |this, cx| {
            let created = pending.await;
            this.update(cx, |this, cx| {
                this.receive(created, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Seat whatever a thread read came back with — the one place a resolve, an
    /// open and a create all land, so the three cannot drift about what
    /// "showing a thread" means.
    fn receive(&mut self, detail: Result<AgentThreadDetail, String>, cx: &mut Context<Self>) {
        match detail {
            Ok(detail) => match Transcript::from_rows(&detail.messages) {
                Ok(transcript) => {
                    self.thread = Some(detail.thread.id);
                    self.seat(transcript, cx);
                }
                Err(error) => self.error = Some(error),
            },
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// Whether this panel is showing a conversation the reader chose by hand.
    ///
    /// Such a panel does not follow the screen: see [`Self::open_thread`].
    #[must_use]
    pub fn is_chosen(&self) -> bool {
        self.chosen
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
        matches!(self.turn, TurnState::Streaming { .. })
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
            self.collapsed.insert(call_id.clone());
        }
        // The fold is timed from the click, and only from a click: a chip that
        // scrolls back into view has no fold in flight and renders at rest.
        self.fold = Some((call_id, std::time::Instant::now(), row));
        self.list.remeasure_items(row..row + 1);
        cx.notify();
    }

    /// The fold in flight this frame, and whether it still needs frames.
    ///
    /// Advanced here rather than inside the chip because the *row* is what has
    /// to be remeasured on every step: the card's height is changing, and the
    /// list caches heights.
    fn tick_fold(&mut self, reduced_motion: bool) -> Option<(SharedString, f32)> {
        let (call, at, row) = self.fold.clone()?;
        if reduced_motion || at.elapsed() >= luma_ui::motion::span(&luma_ui::motion::RESIZE) {
            self.fold = None;
            self.list.remeasure_items(row..row + 1);
            return None;
        }
        let progress = luma_ui::motion::exit_progress(&luma_ui::motion::RESIZE, at);
        self.list.remeasure_items(row..row + 1);
        Some((call, progress))
    }

    /// Drop the turn, which cancels it.
    ///
    /// Also the *end*-of-turn path, not only the stop button: a turn that ran
    /// to completion and one the reader stopped leave the panel in exactly the
    /// same state, and a second function for it would be a second place that
    /// has to remember to settle the live turn's parse.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.turn = TurnState::Idle;
        // The live turn stops fading and switches from the display parse to
        // the canonical one, which moves every one of its row versions.
        if let Some(ix) = self.live.take() {
            self.resync(ix);
        }
        self.reconcile_rows();
        self.settle_trailer();
        cx.notify();
    }

    /// Put a prompt in the composer and leave the caret in it. What the empty
    /// state's prompts do: they *offer* a question, they do not ask it — a
    /// chip that sent on click would spend a turn on a phrasing nobody read.
    pub fn suggest(&mut self, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.suggest(prompt, window, cx);
        cx.notify();
    }

    /// Send what is in the composer — starting a turn, or steering the one
    /// already running.
    ///
    /// Steering rather than queueing is the runtime's own shape: a redirect is
    /// applied at the next assistant-row boundary, which is where the turn
    /// keeps its durability invariant anyway. Queueing here would be a second
    /// place that decides when a prompt takes effect.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        let prompt = self.composer.prompt(cx);
        if prompt.is_empty() {
            return;
        }
        if let TurnState::Streaming { steer, .. } = &self.turn {
            steer.send(prompt);
            self.composer.clear(cx);
            cx.notify();
            return;
        }
        let Some(thread) = self.thread.clone() else {
            self.error = Some("The conversation is still opening.".into());
            cx.notify();
            return;
        };
        self.composer.clear(cx);
        self.error = None;
        // Your own send always brings you back down: a reply you asked for
        // arriving off-screen is the one case where following is not a guess.
        self.jump_to_bottom(cx);

        let turn = self.agent.turn(&thread, prompt);
        let steer = turn.steering();
        let mut turn = turn;
        let task = cx.spawn(async move |this, cx| {
            while let Some(event) = turn.next().await {
                if this
                    .update(cx, |this, cx| this.on_event(&event, cx))
                    .is_err()
                {
                    return;
                }
            }
            this.update(cx, |this, cx| this.cancel(cx)).ok();
        });
        self.turn = TurnState::Streaming {
            task,
            since: std::time::Instant::now(),
            steer,
        };
        self.settle_trailer();
        cx.notify();
    }

    /// The working indicator's state, or `None` when no turn is running.
    ///
    /// [`working::Working::Sending`] until the model's own row exists: before
    /// that there is nothing being written and a timer would be counting the
    /// round trip out rather than the thinking.
    fn trailer(&self) -> Option<working::Trailer> {
        let TurnState::Streaming { since, .. } = &self.turn else {
            return None;
        };
        let answering = matches!(
            self.transcript.messages.last().map(|message| message.role),
            Some(luma_lib::agent::Role::Assistant)
        );
        Some(working::Trailer {
            state: if answering {
                working::Working::Thinking
            } else {
                working::Working::Sending
            },
            since: *since,
            seed: working::flavour_seed(self.thread.as_deref().unwrap_or_default()),
        })
    }

    /// Remeasure the rows the indicator just moved between.
    ///
    /// Called after anything that can start, end or advance a turn. The
    /// indicator is one line inside a row, so only its *arrival and departure*
    /// change a height — the word and the timer rewrite within it. It is also
    /// the one height a row's content hash cannot see, which is why it gets its
    /// own remeasure rather than riding [`Self::reconcile_rows`].
    fn settle_trailer(&mut self) {
        let next = self.trailer().and(self.rows.len().checked_sub(1));
        if next == self.trailer_row {
            return;
        }
        for row in self.trailer_row.into_iter().chain(next) {
            if row < self.rows.len() {
                self.list.remeasure_items(row..row + 1);
            }
        }
        self.trailer_row = next;
    }

    /// Fold one event, reparse what it touched, and tell the list what moved.
    fn on_event(&mut self, event: &TurnEvent, cx: &mut Context<Self>) {
        let applied = luma_lib::agent::apply(&mut self.transcript, event);
        // `apply` is the only thing that appends messages, so growing beside it
        // is the whole of keeping the two in step.
        while self.entries.len() < self.transcript.messages.len() {
            let ix = self.entries.len();
            self.entries
                .push(Entry::streaming(&self.transcript.messages[ix].id));
        }
        if let TurnEvent::TurnEnded {
            outcome: TurnOutcome::Failed { message },
        } = event
        {
            self.error = Some(message.clone());
        }
        // Liveness is recorded here, from the event that actually writes into
        // a turn — see `Self::live`.
        let was_live = self.live;
        if let Some(ix) = applied.row {
            if matches!(
                self.transcript.messages.get(ix).map(|m| m.role),
                Some(luma_lib::agent::Role::Assistant)
            ) && self.is_streaming()
            {
                self.live = Some(ix);
            }
        }
        // The turn the event landed in, and whichever turn just stopped being
        // live — a liveness flip rebuilds a display parse even when no text
        // moved.
        for ix in applied
            .row
            .into_iter()
            .chain(was_live.filter(|ix| Some(*ix) != self.live))
        {
            self.resync(ix);
        }
        self.reconcile_rows();
        self.settle_trailer();
        cx.notify();
    }
}

impl gpui::EventEmitter<ChatEvent> for AgentChat {}

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
            let unattached = cx.entity();
            return self
                .plate(&unattached, &theme)
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
        // Read out before the composer takes `&mut self.composer` below —
        // the plate is painted in one expression, and a live `&self` inside it
        // would collide with that borrow.
        let model = self.model.clone();
        let error = self.error.clone();
        let this = cx.entity();
        // Asked with a clock: a fade that finished while its block was off
        // screen — or while the turn was settling — must stop asking for
        // frames on its own, or the panel never idles again.
        let now = std::time::Instant::now();
        let fading = self.entries.iter().any(|entry| entry.is_fading(now));
        if fading {
            window.request_animation_frame();
        }
        // The spring ticks *after* layout, so it reads this frame's geometry.
        // The condition self-terminates: once a settled transcript has landed
        // the spring parks, `spring_tick` clears, and an idle panel schedules
        // nothing at all.
        if self.pinned && (streaming || self.spring_tick.is_some()) {
            let chat = cx.entity();
            window.on_next_frame(move |_, cx| {
                chat.update(cx, |this, cx| {
                    this.step_spring();
                    cx.notify();
                });
            });
        }
        // Far enough up that the way back is worth offering. Read before the
        // list is built so it reflects the same frame the rows do.
        let adrift = !self.pinned && self.distance > theme::SCROLL_BUTTON_THRESHOLD_PX;
        let fold = self.tick_fold(luma_ui::motion::reduced_motion(cx));
        if fold.is_some() {
            window.request_animation_frame();
        }

        let live = self.live;
        // The indicator trails the *last* row whichever role it has: between
        // the send and the model's first row that is still the user's prompt,
        // and comet's "Sending" bridge is exactly that gap named.
        let trailer = self.trailer();
        let last_row = self.rows.len().checked_sub(1);
        let opening_trailer = self.rows.is_empty().then_some(trailer).flatten();
        let view = cx.entity_id();
        let rows = this.clone();
        let transcript_list = list(self.list.clone(), move |ix, window, cx| {
            let held = rows.clone();
            held.update(cx, |state, cx| {
                let Some(key) = state.rows.get(ix).copied() else {
                    return div().into_any_element();
                };
                let (Some(turn), Some(message)) = (
                    state.entries.get(key.turn),
                    state.transcript.messages.get(key.turn),
                ) else {
                    return div().into_any_element();
                };
                // A turn's last row is the one that carries the trailer and the
                // timestamp lane — read off the row list, which is the only
                // place that knows where a turn ends.
                let last_of_turn = state
                    .rows
                    .get(ix + 1)
                    .is_none_or(|next| next.turn != key.turn);
                transcript::row(
                    &key,
                    turn,
                    message,
                    &transcript::RowCtx {
                        chat: &rows,
                        ix,
                        live: live == Some(key.turn),
                        top_gap: transcript::top_gap_for(
                            ix.checked_sub(1).and_then(|prev| state.rows.get(prev)),
                            &key,
                        ),
                        last_of_turn,
                        trailer: (last_row == Some(ix)).then_some(trailer).flatten(),
                        collapsed: &state.collapsed,
                        fold: fold.as_ref().map(|(call, progress)| (call, *progress)),
                        theme: &state.theme,
                    },
                    window,
                    cx,
                )
            })
        })
        .size_full();

        self.plate(&this, &theme)
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
                        // A turn sent from the empty state has no row to trail
                        // yet — the user's own row arrives with the turn's
                        // first event, one hop later. Without this the panel
                        // would go silent for that hop, which is exactly the
                        // moment a person is looking for a sign it heard them.
                        let opening_trailer = opening_trailer.map(|state| {
                            div()
                                .absolute()
                                .bottom(px(theme::SPACE_LG))
                                .left_0()
                                .child(working::trailer(&state, &theme, view, cx))
                        });
                        el.child(opening(&Opening::of(kind), Some(&this), &theme))
                            .children(opening_trailer)
                    })
                    .when(!self.transcript.messages.is_empty(), |el| {
                        el.child(transcript_list)
                            .children(fade_bands())
                            .child(self.rail(&theme))
                            .children(adrift.then(|| jump_to_bottom(&this, &theme)))
                    }),
            )
            .child(composer::composer(
                &mut self.composer,
                &this,
                streaming,
                model.as_deref(),
                &theme,
                window,
                cx,
            ))
            .child(status_strip(streaming, error.as_deref(), kind, &theme))
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
        // One tick per prompt, and the tick scrolls to that prompt's *row* —
        // the rail indexes the list, not the transcript, and with block rows
        // the two no longer coincide.
        let prompts: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, key)| matches!(key.kind, transcript::RowKind::Prompt))
            .map(|(row, _)| row)
            .collect();
        let last_user = prompts.last().copied();
        for ix in prompts {
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
                    .child(div().w(px(14.)).h(px(2.)).rounded_full().bg(if active {
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
    fn plate(&self, chat: &Entity<AgentChat>, theme: &Theme) -> gpui::Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            // No fill of its own: the shell's content card paints the ground
            // (`theme::panel`), and a second plane here would stack a second
            // coverage over the blur the card is there to let through.
            .text_color(theme.text)
            .child(self.header(chat, theme))
    }

    fn header(&self, chat: &Entity<AgentChat>, theme: &Theme) -> impl IntoElement {
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
                    .rounded(px(luma_ui::radius::CONTROL))
                    .bg(theme::wash(0.06))
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(badge)
            }))
            .child(div().flex_1())
            // The two ways out of the conversation you are in: back to an
            // older one, or on to a new one. Only shown on an attached panel —
            // an unattached one has no venue to search and no subject to start
            // a chat about, so both would be buttons that cannot work.
            .children(self.scope.is_some().then(|| {
                let rewind = chat.clone();
                let fresh = chat.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_XS))
                    .child(header_button(
                        "chat-history",
                        IconName::Undo2,
                        "Chat history",
                        theme,
                        move |cx| {
                            rewind.update(cx, |_, cx| cx.emit(ChatEvent::HistoryRequested));
                        },
                    ))
                    .child(header_button(
                        "chat-new",
                        IconName::Plus,
                        "New chat",
                        theme,
                        move |cx| {
                            fresh.update(cx, |this, cx| this.new_thread(cx));
                        },
                    ))
            }))
    }
}

/// One icon button in the panel's header — the chat's own chrome control.
///
/// Square-cornered and unlabelled, sized to the header's own rhythm rather than
/// to a dialog's: these sit *in* the thread's frame, not on a card floating over
/// it, so they take the header's recessive tone and gain their fill only on
/// hover.
fn header_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    theme: &Theme,
    pressed: impl Fn(&mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(theme::HEADER_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(luma_ui::radius::CONTROL))
        .cursor_pointer()
        .hover(|style| style.bg(theme::wash(0.06)))
        .on_click(move |_, _, cx| pressed(cx))
        .child(Icon::new(icon).size(px(14.0)).text_color(theme.text_faint))
        .agent_node(NodeRole::Button, label)
}

/// The bands at both ends of the transcript, dissolved into the panel's own
/// ground.
///
/// Painted overlays rather than gpui's `EdgeFade`, which this pin does not
/// have: a gradient to the panel's own ground *is* the fade. Without them a
/// reply butts against the composer's plate below and the header's label above,
/// and each pair reads as one control.
///
/// The stop is [`theme::panel_opaque`] — the ground's colour at **full**
/// coverage, not [`theme::panel`].
///
/// The difference is the whole bug this had: `panel()` carries the coverage the
/// *plane* spends on the blur behind the window, but a band is painted on top
/// of that plane, not instead of it. Fading to `panel()` therefore laid a
/// second half-coverage of the ground's tone over a surface that already had
/// one — darker than the ground it was meant to disappear into, and still only
/// half-covering the text it was meant to dissolve. It read as a grey haze with
/// the prose showing through.
///
/// The top band is inset by the header's height so text dissolves *before* it
/// can reach the header's own label, rather than crossing under it.
fn fade_bands() -> [gpui::Div; 2] {
    let ground = theme::panel_opaque();
    [
        luma_ui::pane::edge_fade(theme::TRANSCRIPT_FADE_BAND, ground, true).top(px(0.0)),
        luma_ui::pane::edge_fade(theme::TRANSCRIPT_FADE_BAND, ground, false),
    ]
}

/// The way back down, offered only once the bottom is far enough away to be
/// worth a control.
///
/// It re-engages the *pin* rather than jumping the scroll offset: the way back
/// is the same glide the transcript uses to follow a reply, so there is one
/// motion toward the bottom and not two that could disagree about where it is.
fn jump_to_bottom(chat: &Entity<AgentChat>, theme: &Theme) -> impl IntoElement {
    let pressed = chat.clone();
    div()
        .absolute()
        .bottom(px(theme::TRANSCRIPT_FADE_BAND + theme::SPACE_SM))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .id("chat-jump-to-bottom")
                .size(px(theme::JUMP_DIAMETER))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme::card_bg())
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|style| style.bg(theme::glass_hover()))
                .on_click(move |_, _, cx| {
                    pressed.update(cx, |this, cx| this.jump_to_bottom(cx));
                })
                .child(
                    Icon::new(IconName::ArrowDown)
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                )
                .agent_node(NodeRole::Button, "Jump to bottom"),
        )
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
        .rounded(px(luma_ui::radius::CONTROL))
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
) -> AnyElement {
    let strip = div()
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
    // No loader here: the working indicator trails the last row (see
    // [`crate::working`]). A strip that also spun would be a second answer to
    // "is it running?", and two answers can disagree.
    //
    // What the strip does drop while a turn runs is the send hint — a key
    // legend for a field that is busy is an instruction that will not work.
    let subject = match kind {
        luma_lib::agent::AgentKind::TrackCopilot => "Track thread",
        luma_lib::agent::AgentKind::PatternGraph => "Pattern thread",
    };
    strip
        .text_color(theme.text_faint)
        .child(SharedString::from(subject))
        .child(div().flex_1())
        .when(!streaming, |el| el.child(SharedString::from("⏎ to send")))
        .into_any_element()
}
