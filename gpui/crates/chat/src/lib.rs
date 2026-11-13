//! The agent chat panel.
//!
//! # Which tier this is
//!
//! The thread is a **chrome** surface (`docs/specs/comet-shell.md` §9):
//! translucency, sliding motion and streaming markdown. The chrome tier is
//! `luma_ui::glass`, shared with the shell, and [`crate::theme`] names the
//! *roles* over it rather than a palette of its own. Glass and
//! `luma_ui::ladder` read one grey ladder, so the thread column and the
//! timeline beside it cannot drift apart.
//!
//! # Shape
//!
//! ```text
//! AgentChat                  one thread, one composer
//!  ├ agent      Agent        the loop, on the reactor its database needs
//!  ├ subject    Option<..>   the score whose chats this panel shows
//!  ├ scope      Option<..>   the open conversation’s subject
//!  ├ transcript Transcript   luma_lib's type, held — never mirrored
//!  ├ rows       Vec<Row>     render state beside it, one per message
//!  ├ list       ListState    the virtualized transcript
//!  ├ composer   TextareaState
//!  ├ expanded  HashSet      which tool chips the reader has opened
//!  ├ turn       Option<..>   when the shown thread's turn started, if one runs
//!  └ running    Entity ──────┐
//!                            ▼
//! RunningTurns               one per app, shared by every panel
//!  └ turns      HashMap      thread id → { _task, since, steer, transcript }
//! ```
//!
//! A chat belongs to one score. The host names the open score, and the panel
//! opens that score's most recently updated chat. Other editor tabs do not
//! change the thread: they only supply a context for each turn.
//!
//! A turn belongs to its thread, not to the panel. [`RunningTurns`] drives it;
//! the panel attaches to whichever thread it shows, and switching away only
//! detaches — see [`running`].
//!
//! # Streaming
//!
//! One [`TurnEvent`] arrives, [`luma_lib::agent::apply`] folds it into the
//! transcript, the row it named re-syncs its parser, and exactly that row is
//! remeasured. Remeasuring the list is the frame budget; remeasuring one row
//! is not. The fade over the new characters is paint only — see `luma-md`.

pub mod chip;
pub mod composer;
mod model_picker;
pub use model_picker::ModelPicker;
pub mod python_cell;
pub mod running;
mod send_motion;
pub mod subagents;
pub mod theme;
pub mod transcript;
pub mod usage;
pub mod working;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    div, list, prelude::*, px, AnyElement, App, Context, Entity, FocusHandle, ListAlignment,
    ListState, SharedString, Window,
};
use gpui_component::Icon;
use luma_lib::agent::{
    engine::catalog::{ModelChoice, Selection, Service},
    AgentService, ThreadScope, Transcript, TurnContext, TurnEvent, TurnOutcome, UserPrompt,
};
use luma_lib::database::local::feedback::{Feedback, Target, Verdict};
use luma_lib::models::agent_threads::{AgentThread, AgentThreadDetail};
use luma_ui::float::MenuVisibility;
use luma_ui::icons::IconName;
use luma_ui::node::{Instrument, Role as NodeRole};

use crate::composer::Composer;
pub use crate::running::{RunningEvent, RunningTurns};
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

    /// One score's conversations, most recently updated first, with their
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

    pub fn set_selection(
        &self,
        thread_id: String,
        selection: Selection,
    ) -> impl std::future::Future<Output = Result<AgentThread, String>> + use<> {
        let service = self.service.clone();
        let task = self.runtime.spawn(async move {
            service
                .set_thread_selection(&thread_id, selection)
                .await
                .map_err(|error| error.to_string())
        });
        async move { task.await.map_err(|error| error.to_string())? }
    }

    pub fn preferred_selection(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<Selection>, String>> + use<> {
        let service = self.service.clone();
        let task = self.runtime.spawn(async move {
            service
                .preferred_selection()
                .await
                .map_err(|error| error.to_string())
        });
        async move { task.await.map_err(|error| error.to_string())? }
    }

    pub fn models(
        &self,
        service: Service,
    ) -> impl std::future::Future<Output = Result<Vec<ModelChoice>, String>> + use<> {
        let agent = self.service.clone();
        let task = self
            .runtime
            .spawn(async move { agent.models(service).await.map_err(|e| e.to_string()) });
        async move { task.await.map_err(|e| e.to_string())? }
    }

    /// The gateway list saved by the last [`Self::models`], for rows to show
    /// while the next read is on the wire.
    pub fn cached_models(
        &self,
        service: Service,
    ) -> impl std::future::Future<Output = Option<Vec<ModelChoice>>> + use<> {
        let agent = self.service.clone();
        let task = self
            .runtime
            .spawn(async move { agent.cached_models(service).await });
        async move { task.await.ok().flatten() }
    }

    /// A python figure's PNG, by its stored path — see
    /// [`luma_lib::agent::AgentService::figure`].
    pub fn figure(
        &self,
        path: String,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, String>> + Send + use<> {
        let service = self.service.clone();
        let task = self
            .runtime
            .spawn(async move { service.figure(&path).await.map_err(|e| e.to_string()) });
        async move { task.await.map_err(|error| error.to_string())? }
    }

    /// Start a turn. The stream is built here rather than inside the spawned
    /// task so its steering handle can be handed back with it — a turn a host
    /// could not redirect would force the composer to lock while one ran.
    #[must_use]
    pub fn turn(&self, thread_id: &str, prompt: UserPrompt) -> Turn {
        self.drive(self.service.turn(thread_id, prompt))
    }

    /// Continue the thread's unfinished turn. Only on the reader's request —
    /// see [`luma_lib::agent::AgentService::resume`].
    #[must_use]
    pub fn resume(&self, thread_id: &str) -> Turn {
        self.drive(self.service.resume(thread_id))
    }

    /// Record a stop the reader asked for, once the stopped turn has let go,
    /// and answer the thread as it now stands.
    pub fn record_stop(
        &self,
        thread_id: String,
    ) -> impl std::future::Future<Output = Result<Transcript, String>> + use<> {
        let agent = self.service.clone();
        let task = self.runtime.spawn(async move {
            agent
                .record_stop(&thread_id)
                .await
                .map_err(|e| e.to_string())
        });
        async move { task.await.map_err(|e| e.to_string())? }
    }

    fn drive(&self, mut stream: luma_lib::agent::TurnStream) -> Turn {
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        let steer = stream.steering();
        // Drop the stream — which cancels the turn and releases its run lease —
        // as soon as the panel lets go, not at the next event: a long tool
        // call emits nothing, and a send in the meantime would find the
        // thread still locked.
        self.runtime.spawn(async move {
            use futures::{future::Either, StreamExt as _};
            loop {
                let next = std::pin::pin!(stream.next());
                let closed = std::pin::pin!(events.closed());
                match futures::future::select(next, closed).await {
                    Either::Left((Some(event), _)) => {
                        if events.send(event).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });
        Turn { events: rx, steer }
    }
}

/// Reads the editor for one message, at the moment it is sent. The host's
/// own state stays the host's: the chat only calls this.
pub type ContextSource = Box<dyn Fn(&App) -> TurnContext>;

/// What the panel asks its host for.
///
/// Shell requests exist because the chat cannot open a modal: overlays
/// are the shell's to mount, and a chat crate that reached for one would invert
/// the dependency. Starting a *new* conversation is not here — that is entirely
/// the panel's own business, so it just does it.
///
/// Documents an agent commit changed are not here: a turn can run while the
/// panel shows another thread, so the app hears them from [`RunningTurns`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatEvent {
    /// Open this account’s conversation history.
    HistoryRequested,
    /// Show this thread's subagents. `Some` names the child to open straight
    /// into — a transcript chip knows which delegation was clicked, the
    /// floating pill only knows that some are running.
    ///
    /// A request rather than an action for the same reason rewind is: the
    /// panel is a region and cannot mount an overlay.
    SubagentsRequested(Option<SharedString>),
    /// This thread is now on screen: read the reader's verdicts on its turns
    /// and hand them to [`AgentChat::set_verdicts`].
    ///
    /// Requests rather than calls because verdicts are the signed-in
    /// account's, and the account is the host's to know.
    FeedbackRequested(String),
    /// The reader judged a turn, or with `None` took the verdict back. The
    /// panel already shows it; the host stores it.
    FeedbackGiven {
        target: Target,
        verdict: Option<Verdict>,
    },
}

/// One turn's events, in order. Dropping it cancels the turn.
///
/// Driven by [`RunningTurns`], never by a panel — see [`running`].
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

/// Which conversation is on screen, as a *state* rather than an absence.
///
/// An empty transcript means two different things — "this conversation has
/// nothing in it" and "the read has not landed yet" — and the panel paints
/// the opening over the first. Splitting them here is what stops the second
/// being paintable at all: the opening is drawn from [`Self::Open`] only, so
/// a conversation with history cannot flash "where do you want to start?" on
/// its way in. An explicit conversation change shows a loading plate until
/// the selected transcript arrives.
enum Conversation {
    /// Nothing was asked for yet — a reader before its open starts.
    Idle,
    /// A read is in flight, carrying the token it was started with. Answers
    /// are matched against the token so a slow resolve landing after a newer
    /// open is dropped rather than painted over it.
    Loading(u64),
    /// The read landed. This is the thread, and the transcript beside it is
    /// its whole history — empty included, and that empty is a fact.
    Open(String),
}

pub struct AgentChat {
    agent: Agent,
    /// The score or venue whose chats this panel shows: what "New chat"
    /// creates under, and what a send without a context source is about.
    /// `None` only for a reader, which never sends.
    subject: Option<ThreadScope>,
    /// Reads the editor when a message is sent; never changes conversation
    /// identity. `None` sends [`Self::scope`] alone.
    context_source: Option<ContextSource>,
    /// Which conversation, and whether its read has landed. Until it has, the
    /// composer is live but a send waits — the alternative is a send that
    /// silently starts a conversation in a thread nobody asked for.
    conversation: Conversation,
    /// The next read's token. Monotonic, so no two reads of this panel ever
    /// share one — see [`Conversation::Loading`].
    reads: u64,
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
    /// Wheel input headed for the top of a scrollable list broke the pin as it
    /// arrived ([`Self::on_wheel`]); the deferred decision in
    /// [`Self::watch_scrolling`] must not re-engage it.
    input_escaped: bool,
    /// The list took the wheel event in flight: its handler runs first, so a
    /// wheel an overlay above the list let bubble (and the list never saw)
    /// cannot break the pin.
    list_took_input: bool,
    spring: transcript::StickSpring,
    send_motion: send_motion::SendMotion,
    composer_bounds: Rc<Cell<gpui::Bounds<gpui::Pixels>>>,
    plate_bounds: Rc<Cell<gpui::Bounds<gpui::Pixels>>>,
    /// Last tick's wall clock, for the elapsed-frames conversion, and last
    /// tick's distance, which is what makes the re-stick rule direction-aware.
    spring_tick: Option<std::time::Instant>,
    spring_settled: Option<std::time::Instant>,
    distance: f32,
    /// The geometry the pin last held against: the transcript's size (set at
    /// prepaint, ahead of the list's own layout) and the footer's height. A
    /// pinned view that sees either change snaps to the end in the same
    /// frame, so a resize or a taller composer never leaves it short of the
    /// bottom. The spring only runs while a turn streams, so it cannot cover
    /// these.
    pin_size: Rc<Cell<gpui::Size<gpui::Pixels>>>,
    pin_footer: f32,
    /// Reached by `crate::composer` from inside the panel's render, which is
    /// why it is crate-visible rather than private: the plate's layout state
    /// belongs to the composer, and passing it back through the entity would
    /// re-enter a borrow that is already live.
    pub(crate) composer: Composer,
    selection: Option<Selection>,
    model_picker: Entity<model_picker::ModelPicker>,
    selection_saving: bool,
    /// A choice made while the last was still saving, saved after it.
    queued_selection: Option<Selection>,
    /// How many times the panel has rendered, for `Chat renders = N`.
    renders: u32,
    /// The context gauge's click-open card: open, playing its exit, or closed.
    usage: MenuVisibility,
    /// Live delegation state, newest child last, one entry per subagent this
    /// panel has seen a snapshot for.
    ///
    /// **Not durable, and deliberately not derived from the transcript.** What
    /// is durable about a child is the child's own thread; this is the answer
    /// to "what is running right now", which no row can be. Kept beside the
    /// transcript rather than inside it because [`luma_lib::agent::apply`]
    /// drops the event that carries it — see [`TurnEvent::Subagent`].
    subagents: Vec<luma_lib::agent::subagent::SubagentSnapshot>,
    /// A panel that shows a conversation and cannot join it: no header, no
    /// composer, no status strip. What the subagents dialog mounts over a
    /// child thread, so inspecting one costs no second transcript renderer.
    read_only: bool,
    /// Where turns run. The panel starts, steers and stops them there and
    /// never owns one, so showing another thread cannot cancel anything.
    running: Entity<RunningTurns>,
    _running: gpui::Subscription,
    /// When the shown thread's turn started, while one runs — the working
    /// indicator's timer origin. A mirror of [`Self::running`], kept so
    /// [`Self::is_streaming`] needs no `cx`: set by a send and by attaching,
    /// cleared by [`Self::seat`] and by the registry's
    /// [`RunningEvent::Ended`].
    turn: Option<std::time::Instant>,
    /// Which row currently carries the working indicator, so the one row whose
    /// height it changes can be remeasured when it moves. Derived state, kept
    /// only because `ListState` caches heights and cannot be asked what it
    /// last measured.
    trailer_row: Option<usize>,
    /// What went wrong, in the panel's own words. Cleared by the next send.
    error: Option<String>,
    /// A stop the reader asked for is being written. Until it lands, the
    /// stopped turn reads as unfinished, and the panel must not offer to
    /// resume what the reader just stopped.
    stopping: bool,
    /// Tool calls the reader has opened, by call id. Details start closed.
    /// Keyed by the call rather than by row index so a chip keeps its state
    /// while rows arrive above it.
    expanded: HashSet<SharedString>,
    /// Compact turns the reader has opened, by message id.
    open_work: HashSet<SharedString>,
    /// The reader's verdicts on the shown thread's turns, by the id of the
    /// user message that started each one.
    verdicts: HashMap<SharedString, Verdict>,
    /// Whether finished turns fold their work behind a header, as of the last
    /// frame; a change re-splits the rows. See [`set_compact_turns`].
    compact: bool,
    /// Python calls, read once each — see [`python_cell`]. Lives on the panel
    /// rather than on a turn because a call is named by its id, and a `RefCell`
    /// because a row reads its call while the panel is mid-render.
    cells: RefCell<python_cell::Cells>,
    /// Markdown rows the list laid out since the last frame's trim; see
    /// [`transcript::Viewed`].
    viewed: transcript::Viewed,
    /// The one fold in flight: which call was clicked, when, and which row it
    /// is in so the tween can remeasure it. At most one — a fold is started by
    /// a click, and a click lands on one chip.
    fold: Option<(SharedString, std::time::Instant, usize)>,
    /// The folding row, and where its top sat on screen when it was clicked.
    /// Outlives [`Self::fold`] by one layout, the one that settles it.
    fold_hold: Option<(usize, f32)>,
    /// The folding tool card's height, as the chip last rendered it. Written
    /// by the chip during the list's layout, and read later in the same
    /// layout by the send room.
    fold_px: Cell<f32>,
    focus: FocusHandle,
    theme: Theme,
}

impl AgentChat {
    /// Open a chat on `subject`'s conversations: `thread` when one is named,
    /// else the subject's most recently updated one.
    ///
    /// `running` is the app's one [`RunningTurns`]: every panel that can send
    /// shares it, so a turn outlives whichever panel started it.
    pub fn new(
        agent: Agent,
        running: Entity<RunningTurns>,
        subject: ThreadScope,
        thread: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut chat = Self::blank(agent, running, Some(subject.clone()), cx);
        match thread {
            Some(thread) => chat.open_thread(thread, cx),
            None => chat.load(subject, cx),
        }
        chat
    }

    fn blank(
        agent: Agent,
        running: Entity<RunningTurns>,
        subject: Option<ThreadScope>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&running, |this, _, event, cx| this.on_running(event, cx));
        let model_picker = model_picker::ModelPicker::new(agent.clone(), cx);
        // The picker is a view of its own, so it hears of what it shows of
        // the panel — the selection, whether a change is taken — from here.
        cx.observe_self(|chat: &mut Self, cx| chat.sync_model_picker(cx))
            .detach();
        let chat = Self {
            agent,
            subject,
            context_source: None,
            conversation: Conversation::Idle,
            reads: 0,
            transcript: Transcript::default(),
            entries: Vec::new(),
            rows: Vec::new(),
            live: None,
            list: ListState::new(1, ListAlignment::Top, px(theme::OVERDRAW_PX)),
            // A fresh thread opens at the bottom, which is where a conversation
            // is read from.
            pinned: true,
            input_escaped: false,
            list_took_input: false,
            spring: transcript::StickSpring::default(),
            send_motion: send_motion::SendMotion::default(),
            composer_bounds: Rc::default(),
            plate_bounds: Rc::default(),
            spring_tick: None,
            spring_settled: None,
            distance: 0.0,
            pin_size: Rc::default(),
            pin_footer: 0.0,
            composer: Composer::new(cx),
            selection: None,
            model_picker,
            selection_saving: false,
            queued_selection: None,
            renders: 0,
            usage: MenuVisibility::Closed,
            subagents: Vec::new(),
            read_only: false,
            running,
            _running: subscription,
            stopping: false,
            turn: None,
            trailer_row: None,
            error: None,
            expanded: HashSet::new(),
            open_work: HashSet::new(),
            verdicts: HashMap::new(),
            compact: false,
            cells: RefCell::default(),
            viewed: transcript::Viewed::default(),
            fold: None,
            fold_hold: None,
            fold_px: Cell::new(0.0),
            focus: cx.focus_handle(),
            theme: Theme::dark(),
        };
        let mut chat = chat;
        chat.watch_scrolling(cx);
        chat
    }

    /// A panel that only *reads* one conversation, by id.
    ///
    /// The same entity the shell's centre is, with the two surfaces that make
    /// a conversation joinable — the header and the footer — left unbuilt. It
    /// is not a second renderer and must never become one: the whole reason a
    /// subagent's thread is a real `agent_threads` row is that reading it
    /// needs no code of its own.
    ///
    /// A reader never sends, but it reads the app's [`RunningTurns`]: a
    /// running child's turn is folded there from its parent's, so the reader
    /// attaches to it the way the main panel attaches to its own turn.
    pub fn reader(
        agent: Agent,
        running: Entity<RunningTurns>,
        thread_id: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut chat = Self::blank(agent, running, None, cx);
        chat.read_only = true;
        chat.open_thread(thread_id, cx);
        chat
    }

    /// The app's running turns — what the history list marks as running.
    #[must_use]
    pub fn running(&self) -> &Entity<RunningTurns> {
        &self.running
    }

    /// Every subagent this panel has heard from this session, running and
    /// finished. The dialog lists them; the pill counts the unfinished ones.
    #[must_use]
    pub fn subagents(&self) -> &[luma_lib::agent::subagent::SubagentSnapshot] {
        &self.subagents
    }

    /// Ask the host to show this thread's subagents, optionally opening
    /// straight into one child's transcript.
    pub(crate) fn request_subagents(
        &mut self,
        child: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(ChatEvent::SubagentsRequested(child));
    }

    /// Open or close the context gauge's card. Crate-visible because the
    /// gauge is painted from a free function in [`usage`], which has no
    /// `&mut self` to reach.
    pub(crate) fn set_usage_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.usage = MenuVisibility::Open;
        } else {
            self.usage.close();
        }
        cx.notify();
    }

    /// Tell the picker what it shows of the panel, when that has changed.
    fn sync_model_picker(&mut self, cx: &mut Context<Self>) {
        let seen = model_picker::Seen {
            selection: self.selection.clone(),
            streaming: self.is_streaming(),
            saving: self.selection_saving,
        };
        if self.model_picker.read(cx).seen != seen {
            self.model_picker.update(cx, |picker, cx| {
                picker.seen = seen;
                cx.notify();
            });
        }
    }

    /// The model picker, its own view so that scrolling or typing in it
    /// redraws only it. The shell mounts it beside the panel; the panel draws
    /// only its chip.
    #[must_use]
    pub fn model_picker(&self) -> &Entity<model_picker::ModelPicker> {
        &self.model_picker
    }

    fn select_model(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.is_streaming() {
            return;
        }
        if self.selection_saving {
            self.queued_selection = Some(selection);
            return;
        }
        let Some(thread) = self.thread_id().map(str::to_owned) else {
            return;
        };
        self.selection_saving = true;
        let read = self.reads;
        let pending = self.agent.set_selection(thread, selection);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if this.reads != read {
                    return;
                }
                this.selection_saving = false;
                let queued = this.queued_selection.take();
                if queued.is_none() {
                    this.model_picker
                        .update(cx, |picker, cx| picker.settle_effort(cx));
                }
                match result {
                    Ok(thread) => {
                        this.selection = Selection::from_thread(&thread).ok();
                        if let Some(selection) = &this.selection {
                            let service = selection.service;
                            this.model_picker
                                .update(cx, |picker, cx| picker.browse(service, cx));
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                if let Some(selection) = queued {
                    this.select_model(selection, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read the thread back and seat its history. Restored rows do not fade —
    /// history that dissolved onto the screen every time the panel opened
    /// would read as a reply nobody asked for.
    fn load(&mut self, scope: ThreadScope, cx: &mut Context<Self>) {
        let read = self.begin_read(cx);
        let pending = self.agent.resolve_thread(scope);
        cx.spawn(async move |this, cx| {
            let resolved = pending.await;
            this.update(cx, |this, cx| this.receive(read, resolved, cx))
                .ok();
        })
        .detach();
    }

    /// Enter [`Conversation::Loading`] and hand back the token the answer must
    /// carry. The one place a read starts, so "the column is blank while a
    /// conversation is arriving" is stated once rather than at each of the
    /// three call sites.
    fn begin_read(&mut self, cx: &mut Context<Self>) -> u64 {
        self.reads += 1;
        self.conversation = Conversation::Loading(self.reads);
        self.selection = None;
        self.model_picker.update(cx, |picker, cx| picker.close(cx));
        self.selection_saving = false;
        self.queued_selection = None;
        self.error = None;
        self.verdicts.clear();
        self.seat(Transcript::default(), cx);
        self.reads
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
        // Live state belongs to the conversation being left: a snapshot names a
        // child of *this* thread, and carrying one across would put another
        // conversation's delegation on the pill. So does the attachment to its
        // turn — detaching, which leaves the turn running.
        self.subagents.clear();
        self.turn = None;
        self.live = None;
        self.rows = self.rows_for(None);
        self.list = ListState::new(
            self.rows.len() + 1,
            ListAlignment::Top,
            px(theme::OVERDRAW_PX),
        );
        self.send_motion = send_motion::SendMotion::default();
        self.list.scroll_to_end();
        self.trailer_row = None;
        // A fold names a row of the list being replaced.
        self.fold = None;
        self.fold_hold = None;
        // A fresh list is a fresh scroll handler: the old one watched a
        // `ListState` this panel no longer shows.
        self.watch_scrolling(cx);
        self.pinned = true;
        self.spring.reset();
        self.spring_tick = None;
        self.distance = 0.0;
    }

    /// The rows the transcript renders as, under the current layout.
    fn rows_for(&self, live: Option<usize>) -> Vec<transcript::RowKey> {
        let compact = transcript::Compact {
            on: self.compact,
            open: &self.open_work,
        };
        transcript::rows_for(&self.transcript, &self.entries, live, compact)
    }

    /// Open or close one compact turn's work.
    ///
    /// The header's top is held where the reader clicked it, as for a chip's
    /// fold: the rows that appear or vanish are below it.
    pub fn toggle_work(&mut self, message_id: SharedString, row: usize, cx: &mut Context<Self>) {
        if !self.open_work.remove(&message_id) {
            self.open_work.insert(message_id);
        }
        self.fold_hold = self
            .list
            .bounds_for_item(row)
            .map(|bounds| (row, f32::from(bounds.top())));
        self.reconcile_rows();
        cx.notify();
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
        let next = self.rows_for(self.live);
        let pending = self.send_motion.pending.len();
        let changed = transcript::diff_rows(&self.rows, &next);
        if changed.is_none() && pending == self.send_motion.rendered_pending {
            return;
        }
        let old_pending = self.send_motion.rendered_pending;
        self.send_motion.rendered_pending = pending;
        if pending == old_pending {
            if let Some((range, count)) = changed {
                self.rows = next;
                if range.len() == count {
                    self.list.remeasure_items(range);
                } else {
                    self.list.splice(range, count);
                }
            }
        } else {
            let start = changed.map_or(self.rows.len(), |(range, _)| range.start);
            let old_end = self.rows.len() + old_pending;
            let new_end = next.len() + pending;
            self.rows = next;
            if old_end == new_end {
                self.list.remeasure_items(start..new_end + 1);
            } else {
                self.list.splice(start..old_end, new_end - start);
            }
        }
    }

    fn start_send_motion(&mut self, text: String, cx: &mut Context<Self>) {
        self.send_motion.start(
            text,
            f32::from(self.composer_bounds.get().top()),
            luma_ui::motion::reduced_motion(cx),
        );
        self.reconcile_rows();
        // Bring an offscreen append into the measured range before the spring
        // lifts it. Its visible copy still starts at the composer.
        self.list
            .scroll_to_reveal_item(self.rows.len() + self.send_motion.pending.len() - 1);
        self.pinned = true;
        self.spring.reset();
        self.spring_tick = Some(std::time::Instant::now());
        self.spring_settled = None;
    }

    fn anchor_row(&self) -> Option<usize> {
        let id = self.send_motion.anchor.as_ref()?;
        self.rows
            .iter()
            .position(|row| {
                matches!(row.kind, transcript::RowKind::Prompt)
                    && self.transcript.messages[row.turn].id == id.as_ref()
            })
            .or_else(|| {
                self.send_motion
                    .pending
                    .iter()
                    .position(|prompt| &prompt.id == id)
                    .map(|ix| self.rows.len() + ix)
            })
    }

    /// Whether the tool card folding now is in the exchange the send room sits
    /// under — the one exchange whose height the room depends on.
    fn fold_in_turn(&self) -> bool {
        match (&self.fold, self.anchor_row()) {
            (Some((_, _, row)), Some(anchor)) => *row > anchor,
            _ => false,
        }
    }

    /// How far the transcript's head sits under the floating header: the
    /// header and its blur ([`theme::HEADER_FADE`]). The list's first row
    /// rests below it, and what scrolls up passes under it. A reader has no
    /// header.
    fn header_clearance(&self) -> f32 {
        if self.read_only {
            0.0
        } else {
            theme::HEADER_FADE
        }
    }

    /// Read this frame's layout, consume reserved room, then advance the
    /// message toward its moving on-screen destination with the shared spring.
    fn step_send_motion(&mut self, reduced: bool) {
        let viewport = self.list.viewport_bounds();
        let scroll = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        let anchor = self.anchor_row();
        if let Some(bounds) = anchor.and_then(|ix| self.list.bounds_for_item(ix)) {
            self.send_motion.anchor_offset =
                Some(f32::from(bounds.top() - viewport.top()) + scroll);
        }
        let old_room = self.send_motion.room;
        if let Some(offset) = self.send_motion.anchor_offset {
            let end = self.rows.len() + self.send_motion.pending.len();
            if let Some(bounds) = end
                .checked_sub(1)
                .and_then(|ix| self.list.bounds_for_item(ix))
            {
                let content_end = f32::from(bounds.bottom() - viewport.top()) + scroll;
                // The room fills what the reader sees, which stops at the
                // composer below and the header above: the list's padding at
                // either end is scroll travel of its own, so counting it here
                // too lifts the prompt out of sight by that much.
                let visible =
                    f32::from(viewport.size.height) - self.pin_footer - self.header_clearance();
                let fold = self.fold_in_turn().then(|| self.fold_px.get());
                let room = self
                    .send_motion
                    .measure(visible, content_end - offset, fold);
                if (room - self.send_motion.room).abs() > 0.5 {
                    self.send_motion.room = room;
                    self.list.remeasure_items(end..end + 1);
                }
            }
        }

        if let Some(flight) = &mut self.send_motion.flight {
            if let Some(top) = anchor
                .and_then(|ix| self.list.bounds_for_item(ix))
                .map(|bounds| f32::from(bounds.top()))
                .or_else(|| {
                    self.send_motion
                        .anchor_offset
                        .map(|offset| f32::from(viewport.top()) + offset - scroll)
                })
            {
                let gap =
                    anchor
                        .filter(|ix| *ix < self.rows.len())
                        .map_or(theme::GAP_BLOCK, |ix| {
                            transcript::top_gap_for(
                                ix.checked_sub(1).and_then(|prev| self.rows.get(prev)),
                                &self.rows[ix],
                            )
                        });
                if flight.step(top + gap, reduced)
                    && old_room == self.send_motion.room
                    && self.spring.is_idle()
                {
                    self.send_motion.flight = None;
                }
            }
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
            // Synchronously: only the chat's own flag is touched, not the
            // list, whose `RefCell` borrow is still held here.
            chat.update(cx, |this, _| this.list_took_input = true).ok();
            // Reading the list back is another matter: defer that to after it
            // has let go.
            let chat = chat.clone();
            cx.defer(move |cx| {
                chat.update(cx, |this, cx| {
                    this.list_took_input = false;
                    let escaped = std::mem::take(&mut this.input_escaped);
                    // A deliberate reader scroll ends the send flight.
                    this.send_motion.flight = None;
                    let distance = this.distance_from_bottom();
                    let previous = std::mem::replace(&mut this.distance, distance);
                    let pinned =
                        transcript::pin_after_scroll(this.pinned, escaped, distance, previous);
                    if this.pinned && !pinned {
                        this.spring.reset();
                        this.spring_tick = None;
                    }
                    this.pinned = pinned;
                    cx.notify();
                })
                .ok();
            });
        });
    }

    /// Wheel input headed for the top breaks the pin as it arrives, however
    /// little it moved, as use-stick-to-bottom's wheel handler does. The
    /// distance test in [`Self::watch_scrolling`] cannot see that alone: a
    /// small notch up while the spring chases a growing tail is pulled back
    /// down by the spring before the deferred check measures it. Bubbles after
    /// the list, which has applied the input by now, or never saw it.
    fn on_wheel(&mut self, event: &gpui::ScrollWheelEvent) {
        let can_scroll = self.list.max_offset_for_scrollbar().y > px(0.0);
        self.input_escaped = std::mem::take(&mut self.list_took_input)
            && transcript::wheel_breaks_pin(event.delta, can_scroll);
        if self.input_escaped {
            self.pinned = false;
            self.spring.reset();
            self.spring_tick = None;
        }
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
        let rate = if self.send_motion.flight.is_some() {
            send_motion::SCROLL_RATE
        } else {
            1.0
        };
        let next = self.spring.step(pos, target, frames * rate);
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

    /// Re-engage following and ride back to the latest reply.
    pub fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.pinned = true;
        self.spring_tick = Some(std::time::Instant::now());
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
    /// A turn running in the conversation being left keeps running; opening a
    /// running one attaches to its turn — see [`Self::receive`].
    pub fn open_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let read = self.begin_read(cx);
        let pending = self.agent.open_thread(thread_id.to_string());
        cx.spawn(async move |this, cx| {
            let opened = pending.await;
            this.update(cx, |this, cx| {
                this.receive(read, opened, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Start a fresh conversation on the same score.
    ///
    /// Always creates: "new chat" is a statement, and resolving would hand back
    /// the existing conversation whenever there was one.
    pub fn new_thread(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.subject.clone() else {
            return;
        };
        let read = self.begin_read(cx);
        let pending = self.agent.new_thread(scope);
        cx.spawn(async move |this, cx| {
            let created = pending.await;
            this.update(cx, |this, cx| {
                this.receive(read, created, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Seat whatever a thread read came back with — the one place a resolve, an
    /// open and a create all land, so the three cannot drift about what
    /// "showing a thread" means.
    ///
    /// Answers are dropped unless they belong to the read still in flight: two
    /// reads can be outstanding at once — a fast open issued after a slow
    /// resolve — and the later *answer* is not necessarily the later *ask*.
    fn receive(
        &mut self,
        read: u64,
        detail: Result<AgentThreadDetail, String>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.conversation, Conversation::Loading(current) if current == read) {
            return;
        }
        match detail {
            Ok(detail) => match ThreadScope::try_from(&detail.thread)
                .map_err(|error| error.to_string())
                .and_then(|_| Transcript::from_rows(&detail.messages))
            {
                Ok(transcript) => {
                    self.selection = Selection::from_thread(&detail.thread).ok();
                    // A running thread is attached to, not read: its rows are
                    // written at step boundaries, so the read is missing the
                    // reply in flight, and the registry's transcript is not.
                    let live = self
                        .running
                        .read(cx)
                        .live(&detail.thread.id)
                        .map(|live| (live.since, live.transcript.clone()));
                    if !self.read_only {
                        cx.emit(ChatEvent::FeedbackRequested(detail.thread.id.clone()));
                    }
                    self.conversation = Conversation::Open(detail.thread.id);
                    match live {
                        Some((since, transcript)) => {
                            self.seat(transcript, cx);
                            self.turn = Some(since);
                            self.settle_trailer();
                        }
                        None => {
                            // A turn a quit cut short: a call that never
                            // returned shows as interrupted, as a resume
                            // will record it.
                            let mut transcript = transcript;
                            transcript.interrupt_open_calls();
                            self.seat(transcript, cx);
                        }
                    }
                    if !self.read_only {
                        self.selection_saving = true;
                        let pending = self.agent.preferred_selection();
                        cx.spawn(async move |this, cx| {
                            let result = pending.await;
                            this.update(cx, |this, cx| {
                                if this.reads != read {
                                    return;
                                }
                                this.selection_saving = false;
                                cx.notify();
                                match result {
                                    Ok(Some(selection))
                                        if this.selection.as_ref() != Some(&selection) =>
                                    {
                                        this.select_model(selection, cx)
                                    }
                                    Err(error) => {
                                        this.error = Some(error);
                                        cx.notify();
                                    }
                                    _ => {}
                                }
                            })
                            .ok();
                        })
                        .detach();
                    }

                    if let Some(selection) = &self.selection {
                        let service = selection.service;
                        self.model_picker
                            .update(cx, |picker, cx| picker.browse(service, cx));
                    }
                }
                Err(error) => self.error = Some(error),
            },
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// The conversation on screen, once its read has landed: where a send
    /// lands, and which running turn this panel follows.
    pub fn thread_id(&self) -> Option<&str> {
        match &self.conversation {
            Conversation::Open(id) => Some(id.as_str()),
            Conversation::Idle | Conversation::Loading(_) => None,
        }
    }

    /// Seat the reader's stored verdicts on `thread_id`'s turns: the host's
    /// answer to [`ChatEvent::FeedbackRequested`]. Dropped when the panel has
    /// moved on to another thread.
    pub fn set_verdicts(
        &mut self,
        thread_id: &str,
        feedback: Vec<Feedback>,
        cx: &mut Context<Self>,
    ) {
        if self.thread_id() != Some(thread_id) {
            return;
        }
        self.verdicts = feedback
            .into_iter()
            .filter_map(|feedback| match feedback.target {
                Target::Turn { message_id, .. } => Some((message_id.into(), feedback.verdict)),
                Target::Clip { .. } => None,
            })
            .collect();
        cx.notify();
    }

    /// A press on one of a turn's thumbs: give `verdict` on the turn `asked`
    /// started, or take it back when it is the one already given.
    pub(crate) fn judge(&mut self, asked: SharedString, verdict: Verdict, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id().map(str::to_owned) else {
            return;
        };
        let next = (self.verdicts.get(&asked) != Some(&verdict)).then_some(verdict);
        match next {
            Some(verdict) => self.verdicts.insert(asked.clone(), verdict),
            None => self.verdicts.remove(&asked),
        };
        cx.emit(ChatEvent::FeedbackGiven {
            target: Target::Turn {
                thread_id,
                message_id: asked.to_string(),
            },
            verdict: next,
        });
        cx.notify();
    }

    /// The thumbs under message `ix`: the user message that started its turn,
    /// and the verdict on it. `None` unless `ix` is the last assistant
    /// message of a turn that has ended.
    ///
    /// A verdict names the user message because that is what the turn's
    /// writes are recorded against; a steer starts a turn of its own.
    fn turn_verdict(&self, ix: usize) -> Option<transcript::TurnVerdict> {
        use luma_lib::agent::Role;
        let messages = &self.transcript.messages;
        if self.read_only || !matches!(messages.get(ix)?.role, Role::Assistant) {
            return None;
        }
        let running = match messages.get(ix + 1) {
            Some(next) => matches!(next.role, Role::Assistant),
            // A prompt still on its way in belongs to the next turn.
            None => self.is_streaming() && self.send_motion.pending.is_empty(),
        };
        if running || self.live == Some(ix) {
            return None;
        }
        let asked = messages[..ix]
            .iter()
            .rev()
            .find(|message| !matches!(message.role, Role::Assistant))
            .filter(|message| matches!(message.role, Role::User))?;
        let asked = SharedString::from(asked.id.clone());
        Some(transcript::TurnVerdict {
            verdict: self.verdicts.get(&asked).copied(),
            asked,
        })
    }

    /// Whether the panel is showing a conversation rather than waiting for
    /// one. The empty state hangs off this: an opening painted while a read is
    /// in flight is a claim the panel cannot yet make.
    fn is_open(&self) -> bool {
        matches!(self.conversation, Conversation::Open(_))
    }

    /// Set what reads the editor for each message sent. Read at send time
    /// rather than pushed, because the playhead moves every frame.
    pub fn set_context_source(&mut self, source: ContextSource) {
        self.context_source = Some(source);
    }

    /// The editor as it stands now, for the message being sent.
    fn editor_context(&self, cx: &App) -> TurnContext {
        match &self.context_source {
            Some(source) => source(cx),
            None => TurnContext {
                scope: self.subject.clone(),
                editor: None,
                perform: None,
            },
        }
    }

    /// Whether a turn is running. The composer, the send button and the status
    /// strip all read this one fact.
    pub fn is_streaming(&self) -> bool {
        self.turn.is_some()
    }

    /// Dismiss the model picker first; otherwise stop a running turn.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if self.model_picker.read(cx).open {
            self.model_picker.update(cx, |picker, cx| picker.close(cx));
            return;
        }
        if self.is_streaming() {
            self.cancel(cx);
        }
    }

    /// Open or close one tool call's detail.
    ///
    /// Remeasures exactly the row the chip is in, for the same reason a
    /// streamed delta does: the height that changed is one row's, and
    /// remeasuring the list is the frame budget.
    ///
    /// Nothing above the chip may move while it folds. The press that clicked
    /// it already released the bottom pin (see the transcript's mouse-down);
    /// the send room takes the card's change in the same layout (see
    /// [`send_motion::SendMotion::room_with_fold`]); and the row's top is held
    /// where the reader clicked it until the fold settles — see
    /// [`Self::hold_fold`].
    pub fn toggle_tool(&mut self, call_id: SharedString, row: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&call_id) {
            self.expanded.insert(call_id.clone());
        }
        // Read before the remeasure: an unmeasured row reports no bounds.
        self.fold_hold = self
            .list
            .bounds_for_item(row)
            .map(|bounds| (row, f32::from(bounds.top())));
        // The fold is timed from the click, and only from a click: a chip that
        // scrolls back into view has no fold in flight and renders at rest.
        self.fold = Some((call_id, std::time::Instant::now(), row));
        self.list.remeasure_items(row..row + 1);
        cx.notify();
    }

    /// Keep the folding row's top where it was when the reader clicked.
    ///
    /// The list holds its scroll anchor through a remeasure, so a fold alone
    /// moves nothing above the row. What can still move it is the list
    /// clamping its scroll when a collapse shortens the content under the
    /// view, and the pixel between the card's tweened and natural heights.
    /// This scrolls the difference back after each layout, and stops on the
    /// first layout after the fold has settled.
    fn hold_fold(&mut self) {
        let Some((row, top)) = self.fold_hold else {
            return;
        };
        if self.fold.is_none() {
            self.fold_hold = None;
        }
        if let Some(bounds) = self.list.bounds_for_item(row) {
            // `scroll_by(+d)` moves the content up by `d`.
            let drift = f32::from(bounds.top()) - top;
            if drift.abs() > 0.1 {
                self.list.scroll_by(px(drift));
            }
        }
    }

    /// The fold in flight this frame, and whether it still needs frames.
    ///
    /// Advanced here rather than inside the chip because the *row* is what has
    /// to be remeasured on every step: the card's height is changing, and the
    /// list caches heights.
    fn tick_fold(&mut self, reduced_motion: bool) -> Option<(SharedString, f32)> {
        let (call, at, row) = self.fold.clone()?;
        if reduced_motion || at.elapsed() >= luma_ui::motion::span(&luma_ui::motion::SURFACE) {
            self.fold = None;
            self.list.remeasure_items(row..row + 1);
            return None;
        }
        let progress = luma_ui::motion::exit_progress(&luma_ui::motion::SURFACE, at);
        self.list.remeasure_items(row..row + 1);
        Some((call, progress))
    }

    /// Stop the shown thread's turn. Turns of other threads keep running.
    ///
    /// The stop is written to the thread, so the chat does not offer to
    /// resume a turn the reader ended on purpose — that offer is for turns a
    /// quit or a failure cut short.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(thread) = self.thread_id().filter(|_| self.is_streaming()) {
            let thread = thread.to_owned();
            self.running
                .update(cx, |running, cx| running.cancel(&thread, cx));
            self.stopping = true;
            let pending = self.agent.record_stop(thread.clone());
            cx.spawn(async move |this, cx| {
                let recorded = pending.await;
                this.update(cx, |this, cx| {
                    this.stopping = false;
                    match recorded {
                        // Seated only over the same thread, at rest: a
                        // reader who moved on or sent again has newer rows.
                        Ok(transcript)
                            if this.thread_id() == Some(thread.as_str())
                                && !this.is_streaming() =>
                        {
                            this.seat(transcript, cx);
                        }
                        Ok(_) => {}
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        self.settle(cx);
    }

    /// Whether to offer [`Self::resume`]: the shown thread's last turn
    /// stopped part way — a quit or a failure, not the reader's stop — and
    /// nothing is running in it.
    fn resumable(&self) -> bool {
        !self.read_only
            && !self.is_streaming()
            && !self.stopping
            && self.is_open()
            && self.transcript.unfinished()
    }

    /// Continue the shown thread's unfinished turn from what it saved.
    pub fn resume(&mut self, cx: &mut Context<Self>) {
        if !self.resumable() {
            return;
        }
        let Some(thread) = self.thread_id().map(str::to_owned) else {
            return;
        };
        let agent = self.agent.clone();
        let transcript = self.transcript.clone();
        let started = self.running.update(cx, |running, cx| {
            running.start(&thread, transcript, || agent.resume(&thread), cx)
        });
        match started {
            Ok(since) => {
                self.error = None;
                self.turn = Some(since);
                self.settle_trailer();
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// Detach from an ended turn.
    ///
    /// The stop button's path and the *end*-of-turn path both land here: a
    /// turn that ran to completion and one the reader stopped leave the panel
    /// in exactly the same state, and a second function for it would be a
    /// second place that has to remember to settle the live turn's parse.
    fn settle(&mut self, cx: &mut Context<Self>) {
        self.turn = None;
        if !self.send_motion.pending.is_empty() {
            let draft = self
                .send_motion
                .pending
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            self.composer.restore(&draft, cx);
            self.send_motion.pending.clear();
            self.send_motion.flight = None;
            self.send_motion.anchor = None;
            self.send_motion.anchor_offset = None;
            self.send_motion.room = 0.0;
        }
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
    /// Steering rather than queueing is the runtime's own shape: a redirect
    /// reaches the model at the engine's next step, and the turn places it in
    /// the transcript there ([`TurnEvent::Steered`]). Queueing here would be a
    /// second place that decides when a prompt takes effect.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        if self.selection_saving {
            return;
        }
        self.model_picker.update(cx, |picker, cx| picker.close(cx));
        let prompt = self.composer.prompt(cx);
        if prompt.is_empty() {
            return;
        }
        let message = UserPrompt {
            text: prompt.clone(),
            context: Some(self.editor_context(cx)),
        };
        if let Some(thread) = self.thread_id().filter(|_| self.is_streaming()) {
            self.running.read(cx).steer(thread, message);
            self.start_send_motion(prompt, cx);
            self.settle_trailer();
            self.composer.clear(cx);
            cx.notify();
            return;
        }
        let Some(thread) = self.thread_id().map(str::to_owned) else {
            self.error = Some("The conversation is still opening.".into());
            cx.notify();
            return;
        };
        let agent = self.agent.clone();
        let transcript = self.transcript.clone();
        let started = self.running.update(cx, |running, cx| {
            running.start(&thread, transcript, || agent.turn(&thread, message), cx)
        });
        let since = match started {
            Ok(since) => since,
            // Refused: the prompt stays in the composer for after a stop.
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.composer.clear(cx);
        self.error = None;
        self.start_send_motion(prompt, cx);
        self.turn = Some(since);
        self.settle_trailer();
        cx.notify();
    }

    /// The working indicator's state, or `None` when no turn is running.
    ///
    /// [`working::Working::Sending`] until the model's own row exists: before
    /// that there is nothing being written and a timer would be counting the
    /// round trip out rather than the thinking.
    fn trailer(&self) -> Option<working::Trailer> {
        let since = self.turn?;
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
            since,
            seed: working::flavour_seed(self.thread_id().unwrap_or_default()),
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
        let next = self.trailer().and(self.trailer_item());
        if next == self.trailer_row {
            return;
        }
        let items = self.rows.len() + self.send_motion.pending.len();
        for row in self.trailer_row.into_iter().chain(next) {
            if row < items {
                self.list.remeasure_items(row..row + 1);
            }
        }
        self.trailer_row = next;
    }

    /// The list item the indicator trails: the last row, or the last prompt
    /// still waiting for the turn to accept it.
    fn trailer_item(&self) -> Option<usize> {
        (self.rows.len() + self.send_motion.pending.len()).checked_sub(1)
    }

    /// Take one live subagent snapshot, replacing the child's previous one.
    ///
    /// Keyed by the *call* rather than by the child thread, because the call
    /// exists from the delegation's first frame and the thread does not — a
    /// child keyed by thread id would appear as a second row the moment it was
    /// created. Order is first-seen, which is the order the transcript's chips
    /// are in.
    ///
    /// A snapshot this build cannot read is dropped rather than shown as a
    /// blank row: it is live state, so the next one is milliseconds away.
    fn fold_subagent(&mut self, snapshot: &serde_json::Value) {
        let Ok(snapshot) =
            serde_json::from_value::<luma_lib::agent::subagent::SubagentSnapshot>(snapshot.clone())
        else {
            return;
        };
        match self
            .subagents
            .iter_mut()
            .find(|held| held.call_id == snapshot.call_id)
        {
            Some(held) => *held = snapshot,
            None => self.subagents.push(snapshot),
        }
    }

    /// Follow the shown thread's turn; every other thread's is not this
    /// panel's to paint.
    fn on_running(&mut self, event: &RunningEvent, cx: &mut Context<Self>) {
        match event {
            RunningEvent::Event { thread, event } if self.thread_id() == Some(thread) => {
                self.on_event(event, cx);
            }
            RunningEvent::Ended { thread } if self.thread_id() == Some(thread) => {
                self.settle(cx);
                // A reader follows a turn it did not start, so it may have
                // attached late or to a delegation its parent dropped. The
                // rows are the thread's record; end on them.
                if self.read_only {
                    self.reload(cx);
                }
            }
            _ => {}
        }
    }

    /// Seat the shown thread's persisted rows in place of the live transcript,
    /// without the blank a fresh read shows first. Skipped when the rows say
    /// what is already shown, and when a turn attached meanwhile.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id().map(str::to_owned) else {
            return;
        };
        let pending = self.agent.open_thread(thread.clone());
        cx.spawn(async move |this, cx| {
            let Ok(detail) = pending.await else {
                return;
            };
            let Ok(transcript) = Transcript::from_rows(&detail.messages) else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.thread_id() != Some(thread.as_str())
                    || this.turn.is_some()
                    || this.transcript == transcript
                {
                    return;
                }
                this.seat(transcript, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fold one event, reparse what it touched, and tell the list what moved.
    fn on_event(&mut self, event: &TurnEvent, cx: &mut Context<Self>) {
        match event {
            TurnEvent::MessageStarted {
                id,
                role: luma_lib::agent::Role::User,
            }
            | TurnEvent::Steered { user_id: id, .. } => self.send_motion.accept(id),
            _ => {}
        }
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
        if let TurnEvent::Subagent { snapshot } = event {
            self.fold_subagent(snapshot);
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
        self.renders += 1;
        div()
            .size_full()
            .track_focus(&self.focus)
            // A press in the transcript focuses this root, not the composer,
            // so the transcript's copy lives here too (as comet's pane does).
            .on_key_down(|event, _, cx| {
                let keys = &event.keystroke;
                let copy = keys.key == "c"
                    && (keys.modifiers.control || keys.modifiers.platform)
                    && !keys.modifiers.shift
                    && !keys.modifiers.alt;
                if !copy {
                    return;
                }
                if let Some(text) = luma_md::selection::selected_text() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    cx.stop_propagation();
                }
            })
            .child(self.body(window, cx))
            .child(luma_ui::node::render_count("Chat", self.renders))
    }
}

/// The `compact_turns` setting, shared by every chat panel in the app. Unset
/// reads as on, the setting's default.
struct CompactTurns(bool);

impl gpui::Global for CompactTurns {}

/// Whether finished turns fold their work behind one header.
#[must_use]
pub fn compact_turns(cx: &App) -> bool {
    cx.try_global::<CompactTurns>().is_none_or(|flag| flag.0)
}

/// Adopt the setting. Panels re-split their rows on their next frame.
pub fn set_compact_turns(on: bool, cx: &mut App) {
    if compact_turns(cx) != on {
        cx.set_global(CompactTurns(on));
        cx.refresh_windows();
    }
}

impl AgentChat {
    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        transcript::trim_caches(&self.entries, &self.viewed);
        let compact = compact_turns(cx);
        if compact != self.compact {
            self.compact = compact;
            self.reconcile_rows();
        }
        if self.usage.tick_close(luma_ui::motion::reduced_motion(cx)) {
            window.request_animation_frame();
        }
        let theme = self.theme.clone();
        let streaming = self.is_streaming();
        // Read out before the composer takes `&mut self.composer` below —
        // the plate is painted in one expression, and a live `&self` inside it
        // would collide with that borrow.
        let picker = self.selection.as_ref().map(|selection| {
            model_picker::chip(
                &self.model_picker,
                selection,
                self.is_streaming() || self.selection_saving,
                cx,
            )
        });
        let error = self.error.clone();
        // Whether the composer's row is complete: the thread's read has landed,
        // and with it the model picker, whose label is the one it keeps — or
        // the read failed and nothing more is coming.
        let ready = error.is_some()
            || (!matches!(self.conversation, Conversation::Loading(_))
                && self
                    .selection
                    .as_ref()
                    .is_none_or(|selection| self.model_picker.read(cx).label_settled(selection)));
        let resumable = self.resumable();
        let this = cx.entity();
        let gauge = self
            .transcript
            .last_request()
            .map(|request| usage::gauge(&request, &this, self.usage, &theme).into_any_element());
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
        if (self.pinned && (streaming || self.spring_tick.is_some()))
            || self.send_motion.flight.is_some()
            || self.send_motion.anchor.is_some()
            || self.fold_hold.is_some()
        {
            let chat = cx.entity();
            let reduced = luma_ui::motion::reduced_motion(cx);
            window.on_next_frame(move |_, cx| {
                chat.update(cx, |this, cx| {
                    let old_room = this.send_motion.room;
                    let had_flight = this.send_motion.flight.is_some();
                    let old_scroll = this.list.scroll_px_offset_for_scrollbar();
                    this.step_send_motion(reduced);
                    if reduced && this.pinned {
                        this.list.scroll_to_end();
                        this.spring_tick = None;
                    } else {
                        this.step_spring();
                    }
                    this.hold_fold();
                    if old_room != this.send_motion.room
                        || had_flight
                        || this.spring_tick.is_some()
                        || old_scroll != this.list.scroll_px_offset_for_scrollbar()
                    {
                        cx.notify();
                    }
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
        // The indicator trails the *last* item whichever role it has: between
        // the send and the model's first row that is still the user's prompt —
        // the pending copy of it until the turn's first event lands — and
        // comet's "Sending" bridge is exactly that gap named.
        let trailer = self.trailer();
        let last_item = self.trailer_item();
        let opening_trailer = self.rows.is_empty().then_some(trailer).flatten();
        let view = cx.entity_id();
        let rows = this.clone();
        // Whether the footer below is the composer's own: a composer is not
        // painted until it is placed (see `composer::Layout::settled`), and
        // its height reaches `composer_bounds` a paint after that. Until
        // then the transcript, which clears the footer, is laid out but not
        // painted either — a pinned tail drawn over a guessed footer would
        // jump when the real one landed.
        let placed = self.read_only || self.composer.shown();
        if !placed {
            window.request_animation_frame();
        }
        // The footer's height as last painted. It overlays the transcript, so
        // the list and everything pinned to the transcript's bottom clear it
        // by this much. A change repaints (see the footer's canvas), so a lag
        // of one frame never settles.
        let footer = if !self.read_only {
            f32::from(self.composer_bounds.get().size.height)
        } else {
            0.0
        };
        let header = self.header_clearance();
        // The composer grew or shrank: the list's bottom padding moves with it,
        // and a pinned view has to move too or the tail slides under the
        // composer.
        if footer != self.pin_footer {
            self.pin_footer = footer;
            if self.pinned {
                self.list.scroll_to_end();
                self.distance = 0.0;
            }
        }
        let pin_watch = {
            let list = self.list.clone();
            let pinned = self.pinned;
            let seen = self.pin_size.clone();
            // Prepainted before the list, so a snap lands in the list's own
            // layout this frame rather than one frame late.
            gpui::canvas(
                move |bounds, _, _| {
                    if seen.replace(bounds.size) != bounds.size && pinned {
                        list.scroll_to_end();
                    }
                },
                |_, _, _, _| (),
            )
            .absolute()
            .size_full()
        };
        let transcript_list = list(self.list.clone(), move |ix, window, cx| {
            let held = rows.clone();
            held.update(cx, |state, cx| {
                let Some(key) = state.rows.get(ix).copied() else {
                    if let Some(prompt) = state.send_motion.pending.get(ix - state.rows.len()) {
                        let flying = state
                            .send_motion
                            .flight
                            .as_ref()
                            .is_some_and(|flight| flight.id == prompt.id);
                        // Shown from the send's first frame, under the slot
                        // the message is flying to: the reader sees at once
                        // that the panel heard them.
                        let trailer = (last_item == Some(ix))
                            .then_some(trailer)
                            .flatten()
                            .map(|trailer| working::trailer(&trailer, &state.theme, view, cx));
                        // The same centred reading column a row has (see
                        // `transcript::row`), so the message does not move
                        // sideways when the turn's own row replaces it.
                        return div()
                            .w_full()
                            .pt(px(theme::GAP_BLOCK))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(theme::MAX_CONTENT_WIDTH))
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().opacity(if flying { 0.0 } else { 1.0 }).child(
                                        transcript::user_bubble(
                                            &prompt.text,
                                            &prompt.id,
                                            &state.theme,
                                        ),
                                    ))
                                    .children(trailer),
                            )
                            .agent_node(NodeRole::Text, prompt.text.clone())
                            .into_any_element();
                    }
                    let room = if state.fold_in_turn() {
                        state.send_motion.room_with_fold(state.fold_px.get())
                    } else {
                        state.send_motion.room
                    };
                    return div().h(px(room)).into_any_element();
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
                // Live state of the shown thread's turn, which outlives this
                // panel's attachment to it.
                let awaiting_setup = state
                    .thread_id()
                    .and_then(|thread| state.running.read(cx).live(thread))
                    .map(|live| live.awaiting_setup.clone())
                    .unwrap_or_default();
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
                        flying: state
                            .send_motion
                            .flight
                            .as_ref()
                            .is_some_and(|flight| flight.id.as_ref() == message.id),
                        trailer: (last_item == Some(ix)).then_some(trailer).flatten(),
                        expanded: &state.expanded,
                        open_work: &state.open_work,
                        cells: &state.cells,
                        viewed: &state.viewed,
                        agent: &state.agent,
                        fold: fold.as_ref().map(|(call, progress)| (call, *progress)),
                        fold_px: &state.fold_px,
                        awaiting_setup: &awaiting_setup,
                        verdict: last_of_turn.then(|| state.turn_verdict(key.turn)).flatten(),
                        theme: &state.theme,
                    },
                    window,
                    cx,
                )
            })
        })
        .size_full()
        // The footer floats over the list's tail and the header over its
        // head, so the first and last rows can scroll clear of them and what
        // passes under either blurs rather than stops.
        .pt(px(header))
        .pb(px(footer));

        let plate_bounds = self.plate_bounds.clone();
        let composer_bounds = self.composer_bounds.clone();
        let flight = self.send_motion.flight.as_ref().map(|flight| {
            div()
                .absolute()
                .top(px(flight.y - f32::from(self.plate_bounds.get().top())))
                .left(px(theme::CONTENT_GUTTER))
                .right(px(theme::CONTENT_GUTTER))
                .flex()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .max_w(px(theme::MAX_CONTENT_WIDTH))
                        .child(transcript::user_bubble(&flight.text, &flight.id, &theme)),
                )
                .agent_node(NodeRole::Card, "Sending message")
        });
        self.plate(&theme)
            .font(luma_ui::fonts::font(theme.font_sans.clone()))
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |bounds, _, _, _| plate_bounds.set(bounds),
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .on_scroll_wheel(cx.listener(|this, event, _, _| this.on_wheel(event)))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.pinned = false;
                            this.spring.reset();
                            this.spring_tick = None;
                            this.send_motion.flight = None;
                            cx.notify();
                        }),
                    )
                    // The reading column's minimum gutters. The 736 cap lives
                    // on each row (`transcript::row`) because a `list` hands
                    // its items the full width.
                    .px(px(theme::CONTENT_GUTTER))
                    // Painted first, so each frame's selection registry holds
                    // exactly that frame's visible text in paint order. Not
                    // optional: the registry is a thread-local that every
                    // painted text element pushes into, and nothing else
                    // empties it.
                    .child(luma_md::render::selection_frame_reset_for(
                        cx.entity_id().as_u64(),
                    ))
                    // Only over a conversation that has *landed* empty. A read
                    // in flight has an empty transcript too, and the opening
                    // painted over that is a conversation with history being
                    // told it has none — see [`Conversation`].
                    .when(
                        self.transcript.messages.is_empty()
                            && self.send_motion.pending.is_empty()
                            && self.is_open(),
                        |el| {
                            // A turn sent from the empty state has no row to
                            // trail yet — the user's own row arrives with the
                            // turn's first event, one hop later. Without this
                            // the panel would go silent for that hop, which is
                            // exactly the moment a person is looking for a sign
                            // it heard them.
                            let opening_trailer = opening_trailer.map(|state| {
                                div()
                                    .absolute()
                                    .bottom(px(footer + theme::SPACE_LG))
                                    .left_0()
                                    .child(working::trailer(&state, &theme, view, cx))
                            });
                            el.pt(px(header))
                                .pb(px(footer))
                                .child(opening(Opening::of(self.subject.as_ref()), &this, &theme))
                                .children(opening_trailer)
                        },
                    )
                    .when(
                        !self.transcript.messages.is_empty()
                            || !self.send_motion.pending.is_empty(),
                        |el| {
                            el.child(pin_watch)
                                .child(transcript_list)
                                .when(!self.read_only, |el| el.child(tail_cover(footer)))
                                .children(adrift.then(|| jump_to_bottom(&this, footer, &theme)))
                        },
                    )
                    .when(!placed, |el| {
                        el.map(|mut el| {
                            el.style().visibility = Some(gpui::Visibility::Hidden);
                            el
                        })
                    })
                    .agent_node(NodeRole::Card, "Conversation"),
            )
            // Keep the composer on the transcript's reading column. The footer
            // floats over the transcript's tail rather than below it, so the
            // conversation blurs under it instead of stopping at a seam. It
            // takes the pointer, except the wheel: a click on the composer must
            // not reach the transcript's unpin, but the list still scrolls.
            .when(!self.read_only, |el| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        // One gutter on three sides: the pill sits as far
                        // from the pane's bottom as from its edges.
                        .px(px(theme::CONTENT_GUTTER))
                        .pb(px(theme::CONTENT_GUTTER))
                        .block_mouse_except_scroll()
                        .child(
                            div()
                                .relative()
                                .flex()
                                .flex_col()
                                .w_full()
                                .max_w(px(theme::MAX_CONTENT_WIDTH))
                                .child(
                                    gpui::canvas(
                                        |_, _, _| (),
                                        move |bounds, _, window, _| {
                                            if composer_bounds.get().size.height
                                                != bounds.size.height
                                            {
                                                window.refresh();
                                            }
                                            composer_bounds.set(bounds)
                                        },
                                    )
                                    .absolute()
                                    .size_full(),
                                )
                                .child(composer::composer(
                                    &mut self.composer,
                                    &this,
                                    ready,
                                    streaming,
                                    picker,
                                    gauge,
                                    &theme,
                                    window,
                                    cx,
                                ))
                                .children(resume_strip(resumable, &this, &theme))
                                .children(error_strip(error.as_deref(), &theme))
                                // The subagent pill clears the full composer.
                                .children(subagents::pill(&self.subagents, &this, &theme)),
                        ),
                )
            })
            // A reader is mounted inside a dialog that already carries a header
            // band; a second one would be two titles over one conversation.
            .when(!self.read_only, |el| el.child(self.header(&this)))
            .children(flight)
            .into_any_element()
    }

    /// The thread's surface: the one place where the chat meets the shell.
    fn plate(&self, theme: &Theme) -> gpui::Div {
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            // No fill of its own: the shell's content card paints the ground
            // (`theme::panel`), and a second plane here would stack a second
            // coverage over the blur the card is there to let through.
            .text_color(theme.text)
    }

    /// The panel's header, floating over the transcript's head: the
    /// transcript [`dissolve`]s under it, and the icons sit crisp on top.
    fn header(&self, chat: &Entity<AgentChat>) -> impl IntoElement {
        dissolve(theme::HEADER_FADE, true)
            .top_0()
            // A press here is the header's, not the transcript's unpin; the
            // wheel still scrolls the list under it.
            .block_mouse_except_scroll()
            // A second fade over the icons' row only, so the ground closes in
            // faster toward the edge than one linear band would.
            .child(luma_ui::pane::edge_fade(
                theme::HEADER_HEIGHT,
                theme::panel_opaque(),
                true,
            ))
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(theme::HEADER_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(theme::SPACE_LG))
                    // The two ways out of the conversation you are in: back
                    // to an older one, or on to a new one.
                    .child({
                        let rewind = chat.clone();
                        let fresh = chat.clone();
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACE_XS))
                            .child(header_button(
                                "chat-history",
                                IconName::Undo,
                                "Chat history",
                                false,
                                move |cx| {
                                    rewind.update(cx, |_, cx| cx.emit(ChatEvent::HistoryRequested));
                                },
                            ))
                            .child(header_button(
                                "chat-new",
                                IconName::Plus,
                                "New chat",
                                !self.is_open(),
                                move |cx| {
                                    fresh.update(cx, |this, cx| this.new_thread(cx));
                                },
                            ))
                    }),
            )
            .agent_node(NodeRole::Card, "Chat header")
    }
}

/// One icon button in the panel's header — the chat's own chrome control.
///
/// Unlabelled, sized to the header's own rhythm rather than
/// to a dialog's: these sit *in* the thread's frame, not on a card floating over
/// it, so they take the header's recessive tone and gain their fill only on
/// hover.
fn header_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    disabled: bool,
    pressed: impl Fn(&mut gpui::App) + 'static,
) -> impl IntoElement {
    luma_ui::icon_button(icon, luma_ui::Enabled::from(!disabled))
        .id(id)
        .on_click(move |_, _, cx| pressed(cx))
        .agent_node(NodeRole::Button, label)
}

/// One end of the transcript dissolving into the panel's own ground: a
/// progressive blur under a fade, strongest at the edge (the top edge when
/// `toward_top`). `height` tall, full width; the caller places it.
///
/// The renderer's backdrop blur is one radius over a rectangle; it has no
/// mask. So the blur is stepped: each of [`theme::DISSOLVE_STEPS`] covers the
/// band from the edge to a line that moves toward the edge with each step,
/// and since every step blurs what the steps before it left, the blur
/// compounds toward the edge. The fade over the steps finishes the dissolve
/// and hides their seams. Where there is no backdrop blur, the fade alone
/// does it.
///
/// The fade's stop is [`theme::panel_opaque`] — the ground's colour at
/// **full** coverage, not [`theme::panel`]. `panel()` carries the coverage
/// the *plane* spends on the blur behind the window, and a band painted on
/// top of that plane at `panel()` would be darker than the ground and only
/// half-cover the text: a grey haze.
///
/// Nothing here moves on its own: it paints when the panel does, so an idle
/// panel costs nothing and a scroll pays a few small blurs a frame.
fn dissolve(height: f32, toward_top: bool) -> gpui::Div {
    let steps = theme::DISSOLVE_STEPS.len();
    let blur = luma_ui::dialog::BACKDROP_BLUR_SUPPORTED.then(|| {
        theme::DISSOLVE_STEPS
            .iter()
            .enumerate()
            .map(move |(step, radius)| {
                let reach = height * (steps - step) as f32 / steps as f32;
                let band = div().absolute().left_0().right_0().h(px(reach));
                let band = if toward_top {
                    band.top_0()
                } else {
                    band.bottom_0()
                };
                luma_ui::dialog::frosted(0.0, *radius, band)
            })
    });
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(px(height))
        .children(blur.into_iter().flatten())
        .child(luma_ui::pane::edge_fade(
            height,
            theme::panel_opaque(),
            toward_top,
        ))
}

/// The transcript's tail under the composer: it [`dissolve`]s just above the
/// footer, and the footer's own room — the pill and the gutters around it —
/// is the panel's ground, so no text shows beside or below the pill.
fn tail_cover(footer: f32) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(footer + theme::TRANSCRIPT_FADE_BAND))
        .child(dissolve(theme::TRANSCRIPT_FADE_BAND, false).top_0())
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(footer))
                .bg(theme::panel_opaque()),
        )
        .agent_node(NodeRole::Card, "Transcript tail")
}

/// The way back down, offered only once the bottom is far enough away to be
/// worth a control.
///
/// It re-engages the *pin* rather than jumping the scroll offset: the way back
/// is the same glide the transcript uses to follow a reply, so there is one
/// motion toward the bottom and not two that could disagree about where it is.
fn jump_to_bottom(chat: &Entity<AgentChat>, footer: f32, theme: &Theme) -> impl IntoElement {
    let pressed = chat.clone();
    div()
        .absolute()
        .bottom(px(footer + theme::TRANSCRIPT_FADE_BAND + theme::SPACE_SM))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        // Frosted and filled like the composer pill: it floats over the
        // transcript, and a bare chip's faint wash let the text show straight
        // through it.
        .child(luma_ui::dialog::frosted(
            theme::JUMP_DIAMETER / 2.0,
            theme::PILL_BLUR,
            luma_ui::float::chip()
                .id("chat-jump-to-bottom")
                .size(px(theme::JUMP_DIAMETER))
                .px(px(0.0))
                .rounded_full()
                .bg(theme.input_bg)
                .border_1()
                .border_color(theme.border)
                .on_click(move |_, _, cx| {
                    pressed.update(cx, |this, cx| this.jump_to_bottom(cx));
                })
                .child(
                    Icon::new(IconName::ArrowDown)
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                )
                .agent_node(NodeRole::Button, "Jump to bottom"),
        ))
}

/// What a conversation that has not started offers: a question, and things
/// worth asking of what the conversation is about.
///
/// The copy deliberately does not restate the composer's placeholder: an empty
/// state that paraphrases the field below it is a stub with two voices.
struct Opening {
    prompts: &'static [&'static str],
}

impl Opening {
    /// Over a score: its song is what there is to ask about.
    const SCORE: Self = Self {
        prompts: &[
            "Light this whole song",
            "Make the drop hit harder",
            "What is on the timeline?",
        ],
    };
    /// Over a venue: its room and its rig.
    const VENUE: Self = Self {
        prompts: &[
            "Describe this room",
            "Group these fixtures for me",
            "What can you do here?",
        ],
    };

    fn of(subject: Option<&ThreadScope>) -> &'static Self {
        match subject.map(|scope| scope.subject_kind) {
            Some(luma_lib::agent::SubjectKind::Track) => &Self::SCORE,
            Some(luma_lib::agent::SubjectKind::Venue) | None => &Self::VENUE,
        }
    }
}

/// What a conversation that has not started asks the reader. Public because
/// it is what a test asserting the empty state never flashes over a loading
/// thread looks for, and a test spelling the copy itself would pass while the
/// shipped words said something else.
pub const OPENING_HEADLINE: &str = "What should it look like?";

/// A conversation that has not started: a headline over the prompts that fill
/// the composer.
///
/// `chat` is the panel the prompts send into.
fn opening(opening: &Opening, chat: &Entity<AgentChat>, theme: &Theme) -> impl IntoElement {
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
                        .text_size(px(20.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from(OPENING_HEADLINE))
                        .agent_node(NodeRole::Text, OPENING_HEADLINE),
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
                                .iter()
                                .copied()
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
    luma_ui::float::chip()
        .id(("chat-suggestion", ix))
        .w_full()
        .justify_start()
        .px(px(theme::SPACE_MD))
        .text_color(theme.text_muted)
        .on_click(move |_, window, cx| {
            pressed.update(cx, |this, cx| this.suggest(prompt, window, cx));
        })
        .child(SharedString::from(prompt))
        .agent_node(NodeRole::Button, prompt)
}

/// The error line under the composer, or nothing. Only a failure earns the
/// space: the send hint went with the language label, and the context gauge
/// lives in the composer's cluster.
/// The offer to continue a turn that stopped part way. Never automatic: a
/// resume spends money, so it waits for the reader.
fn resume_strip(show: bool, chat: &Entity<AgentChat>, theme: &Theme) -> Option<AnyElement> {
    if !show {
        return None;
    }
    let chat = chat.clone();
    Some(
        div()
            .h(px(theme::STATUS_STRIP_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(theme::SPACE_MD))
            .w_full()
            .px(px(theme::SPACE_LG))
            .mt(px(theme::SPACE_XS))
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .child("The last turn stopped before it finished.")
            .child(
                luma_ui::button("Resume", luma_ui::Enabled::Yes)
                    .id("resume-turn")
                    .on_click(move |_, _, cx| chat.update(cx, |this, cx| this.resume(cx)))
                    .agent_node(NodeRole::Button, "Resume"),
            )
            .into_any_element(),
    )
}

fn error_strip(error: Option<&str>, theme: &Theme) -> Option<AnyElement> {
    let error = error?;
    Some(
        div()
            .h(px(theme::STATUS_STRIP_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .px(px(theme::SPACE_LG))
            .mt(px(theme::SPACE_XS))
            .text_size(px(11.0))
            .text_color(theme.danger)
            .child(SharedString::from(error.to_string()))
            .agent_node(NodeRole::Text, error.to_string())
            .into_any_element(),
    )
}
