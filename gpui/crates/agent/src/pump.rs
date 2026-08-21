//! The app thread: the only thread that ever holds a `&mut App`.
//!
//! # Why a thread and a channel
//!
//! gpui's `App` is `!Send` and its whole API wants `&mut`. An interpreter that
//! could reach it would either have to be re-entrant into gpui (it is not) or
//! hold a borrow across a JavaScript call (it cannot). So the app lives alone
//! on one thread, and everything else talks to it in [`Cmd`]s: plain `Send`
//! data in, JSON out. No gpui handle is ever visible to a script.
//!
//! # The settle loop
//!
//! Every command ends the same way:
//!
//! ```text
//! window.update(…)  ->  run_until_parked()  ->  draw()
//! ```
//!
//! `run_until_parked` drains the spawned work an interaction kicked off, and
//! `draw` is what actually produces a frame of nodes — gpui only re-runs
//! `prepaint` when it draws, so without the explicit draw the registry would
//! still describe the frame before the click. Settling *after* rather than
//! *before* each command is what makes the frame stable while a script is
//! deciding what to do next.
//!
//! # Modes
//!
//! Both modes are gpui's `TestPlatform`: no window server, deterministic
//! scheduling from a seed. [`Mode::Headless`] takes its defaults — a noop text
//! system and no renderer — which costs nothing and can measure nothing.
//! [`Mode::Pixel`] plugs in the platform's real text system and a GPU
//! renderer, so glyphs have their true metrics and frames can be read back as
//! images. Same architecture, better parts; see `crate::pixel`, which only
//! exists when the `pixel` feature is on.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    px, size, AnyView, AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement,
    Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    PlatformInput, Point, Render, ScrollDelta, ScrollWheelEvent, Size, TestAppContext,
    TestDispatcher, TouchPhase, Window,
};
use luma_ui::node::{Node, NodeRegistry, Role};
use serde_json::{json, Value};

use crate::error::HarnessError;
use crate::protocol::{Cmd, DragTarget, NodeRef, Restale, ScrollAt};

/// Builds the view under test. `Fn`, not `FnOnce`, because [`Cmd::Reset`]
/// builds it again.
pub type RootFactory = Arc<dyn Fn(&mut Window, &mut App) -> AnyView + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Headless,
    /// Real text metrics and a real renderer behind the same test platform.
    /// The only mode that can screenshot. Needs the `pixel` feature.
    Pixel,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    /// Seeds gpui's `TestDispatcher`, which randomizes task order. Two runs
    /// with the same seed schedule identically, which is what makes a script
    /// byte-reproducible.
    pub seed: u64,
    pub window_size: Size<Pixels>,
    /// How long a single command may take before the pump is declared wedged.
    pub call_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Headless,
            seed: std::env::var("SEED")
                .ok()
                .and_then(|seed| seed.parse().ok())
                .unwrap_or(0),
            window_size: size(px(1200.), px(800.)),
            call_timeout: Duration::from_secs(10),
        }
    }
}

/// The window's root: the view under test, wrapped so that every draw starts a
/// fresh frame of nodes.
///
/// The wrapping lives here rather than in the app so that an app screen has no
/// idea it is being driven — `luma-app` renders the same tree in both worlds.
struct AgentShell {
    inner: AnyView,
}

impl Render for AgentShell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        luma_ui::node::agent_root(self.inner.clone())
    }
}

/// A handle on the pump. `Send` and cheap to clone; holds no gpui state.
#[derive(Clone)]
pub struct PumpClient {
    tx: Sender<Envelope>,
    timeout: Duration,
}

struct Envelope {
    cmd: Cmd,
    reply: Sender<Result<Value, HarnessError>>,
}

impl PumpClient {
    /// Run one command on the app thread and wait for its answer.
    ///
    /// A timeout here means the app thread is stuck inside gpui and is not
    /// coming back. There is no way to interrupt a running thread in Rust, so
    /// this reports the failure and leaves the thread where it is: later calls
    /// keep timing out rather than pretending to work.
    pub fn call(&self, cmd: Cmd) -> Result<Value, HarnessError> {
        let (reply, answer) = mpsc::channel();
        self.tx
            .send(Envelope { cmd, reply })
            .map_err(|_| HarnessError::PumpGone)?;
        match answer.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(HarnessError::Timeout {
                waited: self.timeout,
            }),
            Err(RecvTimeoutError::Disconnected) => Err(HarnessError::PumpGone),
        }
    }
}

/// Run the pump on *this* thread, handing `on_ready` a client first.
///
/// The MCP server calls this from `main` and puts stdio on a thread, rather
/// than the other way round: the app is the long-lived thing this process
/// exists to hold, and giving it the main thread means the process ends when
/// the app does.
pub fn run(config: Config, root: RootFactory, on_ready: impl FnOnce(PumpClient)) {
    let (tx, rx) = mpsc::channel();
    on_ready(PumpClient {
        tx,
        timeout: config.call_timeout,
    });
    let mut backend = Backend::open(&config, &root);
    serve(&mut backend, &config, &root, rx);
}

/// Run the pump on a thread of its own. This is what tests use — a test
/// already owns its thread, and headless mode does not care which one it is.
pub fn spawn(config: Config, root: RootFactory) -> PumpClient {
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gpui-agent-pump".into())
        .spawn(move || {
            run(config, root, |client| {
                let _ = ready_tx.send(client);
            })
        })
        .expect("failed to spawn the pump thread");
    ready_rx.recv().expect("the pump thread died on startup")
}

fn serve(backend: &mut Backend, config: &Config, root: &RootFactory, rx: Receiver<Envelope>) {
    while let Ok(Envelope { cmd, reply }) = rx.recv() {
        let result = if matches!(cmd, Cmd::Reset) {
            *backend = Backend::open(config, root);
            Ok(json!({ "frame": backend.frame() }))
        } else {
            handle(backend, cmd)
        };
        // A dropped receiver means the caller timed out and moved on. The
        // command still ran, so there is nothing to undo — just no one to tell.
        let _ = reply.send(result);
    }
}

// -- commands -----------------------------------------------------------------

fn handle(backend: &mut Backend, cmd: Cmd) -> Result<Value, HarnessError> {
    match cmd {
        Cmd::Reset => unreachable!("handled in `serve`, which owns the backend"),

        Cmd::CurrentFrame => Ok(json!({ "frame": backend.frame() })),

        Cmd::Timings => Ok(backend.timings()),

        Cmd::Snapshot => {
            backend.settle();
            Ok(snapshot(backend))
        }

        Cmd::Click {
            node,
            button,
            count,
            modifiers,
            restale,
        } => {
            let point = center(&resolve(backend, &node, restale)?);
            let modifiers = parse_modifiers(&modifiers)?;
            // Each press-release gets its own settled frame: a handler that
            // opens a menu on the first click and acts on the second only sees
            // the second if the first was allowed to redraw.
            for click in 1..=count.max(1) {
                backend.click(point, button.into(), click, modifiers);
                backend.settle();
            }
            Ok(json!({ "frame": backend.frame() }))
        }

        Cmd::Drag {
            from,
            to,
            steps,
            button,
            modifiers,
            restale,
        } => {
            let start = center(&resolve(backend, &from, restale)?);
            let end = match &to {
                DragTarget::Node(to) => center(&resolve(backend, to, restale)?),
                DragTarget::By { dx, dy } => {
                    let end = Point {
                        x: start.x + px(*dx),
                        y: start.y + px(*dy),
                    };
                    within_window(backend, start, end)?
                }
            };
            let modifiers = parse_modifiers(&modifiers)?;
            drag(backend, start, end, steps.max(1), button.into(), modifiers);
            Ok(json!({ "frame": backend.frame() }))
        }

        Cmd::Type {
            node,
            text,
            modifiers,
            restale,
        } => {
            let point = center(&resolve(backend, &node, restale)?);
            let modifiers = parse_modifiers(&modifiers)?;
            backend.click(point, MouseButton::Left, 1, modifiers);
            backend.settle();
            backend.type_text(&text)?;
            backend.settle();
            Ok(json!({ "frame": backend.frame() }))
        }

        Cmd::Key { keys } => {
            backend.keystrokes(&keys)?;
            backend.settle();
            Ok(json!({ "frame": backend.frame() }))
        }

        Cmd::Action { name, payload } => {
            backend.action(&name, payload)?;
            backend.settle();
            Ok(json!({ "frame": backend.frame() }))
        }

        Cmd::Frames { n, wait_ms } => {
            for _ in 0..n {
                if wait_ms > 0 {
                    std::thread::sleep(Duration::from_millis(wait_ms));
                }
                backend.settle();
            }
            Ok(json!({ "frame": backend.frame() }))
        }

        Cmd::Scroll {
            at,
            dx,
            dy,
            steps,
            modifiers,
            restale,
        } => {
            let point = match &at {
                ScrollAt::Node(node) => center(&resolve(backend, node, restale)?),
                ScrollAt::At { x, y } => Point {
                    x: px(*x),
                    y: px(*y),
                },
            };
            let modifiers = parse_modifiers(&modifiers)?;
            let steps = steps.max(1);
            // A wheel arrives where the pointer already is, and gpui answers
            // `Hitbox::should_handle_scroll` from the last mouse *position*
            // rather than from the event's — so a scroll named at a surface
            // the pointer has never visited would be handed to whatever it
            // last hovered. Put the pointer there first, once for the gesture.
            backend.place_pointer(point, modifiers);
            // Split across steps rather than sent as one big delta: a wheel
            // handler that accumulates (zoom is exponential in the distance)
            // lands somewhere different for one flick than for the same
            // distance in ten, and the ten is what a real wheel does.
            for _ in 0..steps {
                backend.scroll(point, dx / steps as f32, dy / steps as f32, modifiers);
                backend.settle();
            }
            Ok(json!({ "frame": backend.frame() }))
        }

        Cmd::Screenshot { node, restale } => {
            let crop = node
                .map(|node| resolve(backend, &node, restale).map(|node| node.bounds))
                .transpose()?;
            backend.screenshot(crop)
        }
    }
}

/// Check that a delta drag lands inside the window, and report it if it does
/// not.
///
/// gpui drops a pointer move outside the window, so a drag walked past the
/// edge ends with no drop, no error, and a control that moved less than the
/// script asked for — which is worse than a failure, because it passes. The
/// `resolve` path is already guarded (an off-screen node is `NotVisible`);
/// this is the same guard for the target a delta names.
fn within_window(
    backend: &mut Backend,
    start: Point<Pixels>,
    end: Point<Pixels>,
) -> Result<Point<Pixels>, HarnessError> {
    let size = backend.in_window(|window, _| window.viewport_size());
    if end.x < px(0.) || end.y < px(0.) || end.x > size.width || end.y > size.height {
        return Err(HarnessError::BadCall(format!(
            "drag from ({}, {}) by ({}, {}) ends at ({}, {}), outside the {}×{} window",
            f32::from(start.x),
            f32::from(start.y),
            f32::from(end.x - start.x),
            f32::from(end.y - start.y),
            f32::from(end.x),
            f32::from(end.y),
            f32::from(size.width),
            f32::from(size.height),
        )));
    }
    Ok(end)
}

/// A drag is not a move: `active_drag` and every drag preview only materialize
/// on a repaint, and gpui does not treat a press as a drag until the pointer
/// has travelled past its own threshold. So the pointer walks, and the app
/// gets a settled frame between every step to react in.
fn drag(
    backend: &mut Backend,
    start: Point<Pixels>,
    end: Point<Pixels>,
    steps: u32,
    button: MouseButton,
    modifiers: Modifiers,
) {
    backend.mouse_down(start, button, 1, modifiers);
    backend.settle();
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        backend.mouse_move(
            Point {
                x: start.x + (end.x - start.x) * t,
                y: start.y + (end.y - start.y) * t,
            },
            button,
            modifiers,
        );
        backend.settle();
    }
    backend.mouse_up(end, button, 1, modifiers);
    backend.settle();
}

fn snapshot(backend: &Backend) -> Value {
    let (frame, nodes) = backend.registry();
    Value::Object(
        [
            ("frame".to_string(), json!(frame)),
            (
                "nodes".to_string(),
                Value::Array(nodes.iter().map(|node| describe(frame, node)).collect()),
            ),
        ]
        .into_iter()
        .collect(),
    )
}

fn describe(frame: u64, node: &Node) -> Value {
    json!({
        "frame": frame,
        "id": node.id,
        "role": node.role.as_str(),
        "label": node.label.to_string(),
        "bounds": {
            "x": f32::from(node.bounds.origin.x),
            "y": f32::from(node.bounds.origin.y),
            "width": f32::from(node.bounds.size.width),
            "height": f32::from(node.bounds.size.height),
        },
        "enabled": node.enabled,
        "focused": node.focused,
    })
}

fn center(node: &Node) -> Point<Pixels> {
    node.bounds.center()
}

/// Turn a script's node reference back into a node of the current frame.
///
/// The identity check runs even when the frame matches: an id is just an index
/// into a `Vec`, so a hand-written or mutated reference would otherwise land
/// on whatever control happens to sit at that index.
fn resolve(backend: &Backend, node: &NodeRef, restale: Restale) -> Result<Node, HarnessError> {
    let (frame, nodes) = backend.registry();
    let missing = || HarnessError::NoSuchNode {
        role: node.role.clone(),
        label: node.label.clone(),
    };

    let found = if node.frame == frame {
        nodes
            .get(node.id)
            .filter(|found| matches(found, node))
            .cloned()
            .ok_or_else(missing)?
    } else if restale == Restale::Match {
        nodes
            .iter()
            .find(|found| matches(found, node))
            .cloned()
            .ok_or_else(missing)?
    } else {
        return Err(HarnessError::StaleFrame {
            snapshot: node.frame,
            current: frame,
        });
    };

    if found.bounds.is_empty() {
        return Err(HarnessError::NotVisible {
            role: node.role.clone(),
            label: node.label.clone(),
        });
    }
    Ok(found)
}

/// The named modifiers a script may hold during a gesture.
///
/// Spelled out rather than accepting gpui's serde form, so an unknown name is
/// a clear error instead of a silently-default `false` — the same reason the
/// prelude refuses unknown options. `"secondary"` is the platform key under
/// the name gpui's own `Modifiers::secondary` gives it, which is what a
/// handler branching on cmd-or-ctrl actually reads.
fn parse_modifiers(names: &[String]) -> Result<Modifiers, HarnessError> {
    let mut modifiers = Modifiers::none();
    for name in names {
        match name.as_str() {
            "control" | "ctrl" => modifiers.control = true,
            "alt" | "option" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "platform" | "secondary" | "command" | "cmd" | "super" | "win" => {
                modifiers.platform = true
            }
            "function" | "fn" => modifiers.function = true,
            other => {
                return Err(HarnessError::BadCall(format!(
                    "unknown modifier {other:?}; expected control, alt, shift, \
                     platform (aka secondary/command), or function"
                )))
            }
        }
    }
    Ok(modifiers)
}

fn matches(found: &Node, wanted: &NodeRef) -> bool {
    Role::parse(&wanted.role) == Some(found.role) && found.label.as_ref() == wanted.label
}

// -- the backend --------------------------------------------------------------

/// Which mode the app was opened in.
///
/// Only four things differ between the modes — how the app is built, how to
/// borrow it, how to drain its executors, and whether it can produce an image.
/// Everything else, including all input, goes through `Window`'s own public
/// `dispatch_event` / `dispatch_keystroke`, so a click means exactly the same
/// thing in both.
pub(crate) enum Host {
    Headless {
        cx: TestAppContext,
        window: AnyWindowHandle,
    },
    /// A `TestPlatform` with a real text system and a real GPU renderer behind
    /// it: same determinism, same absence of a window server, but glyphs have
    /// their true metrics and frames can be read back as pixels.
    #[cfg(feature = "pixel")]
    Pixel {
        cx: gpui::HeadlessAppContext,
        window: AnyWindowHandle,
        shots: std::path::PathBuf,
    },
}

/// The app, plus what the pump has measured of it.
///
/// The timings live here rather than beside the app because they outlive any
/// one command: a scrub is a `drag`, and the frames worth measuring are the
/// ones it draws between pointer steps. A recorder that only ran during
/// `frames` would miss every interaction anyone wants to profile.
pub(crate) struct Backend {
    host: Host,
    /// The last [`TIMING_HISTORY`] frames. Bounded because a session is
    /// long-lived and nobody reads a million rows.
    timings: VecDeque<FrameTiming>,
}

/// How long one settle took, split by what was doing the work.
///
/// The split is the point. "Scrubbing is slow" has different fixes depending
/// on whether the time is the app recomputing in response to the pointer
/// (`parked`) or the element tree being walked into a scene (`draw`).
///
/// # What is not here
///
/// Rasterization. `Window::draw` builds a scene; handing it to the renderer is
/// `Window::present`, and the only public way in — `present_if_needed` — is
/// gated behind gpui's `bench` feature at the pinned rev, which would drag
/// criterion into this crate. So *neither* mode times the GPU, and pixel mode
/// differs from headless only in having real glyph metrics feeding layout.
/// That is worth knowing before reading these numbers as frame times: they are
/// the CPU half. If `draw` turns out not to be the bottleneck, timing the
/// renderer needs a gpui-side change, not a harness one.
#[derive(Debug, Clone, Copy)]
struct FrameTiming {
    frame: u64,
    parked: Duration,
    draw: Duration,
}

/// Frames kept. At 60Hz this is roughly the last eight seconds of drawing.
const TIMING_HISTORY: usize = 512;

impl Backend {
    fn open(config: &Config, root: &RootFactory) -> Self {
        let root = root.clone();
        let build = move |window: &mut Window, cx: &mut App| AgentShell {
            inner: root(window, cx),
        };
        let host = match config.mode {
            Mode::Headless => {
                let mut cx = TestAppContext::build(TestDispatcher::new(config.seed), None);
                let handle =
                    cx.open_window(config.window_size, move |window, cx| build(window, cx));
                Host::Headless {
                    cx,
                    window: AnyWindowHandle::from(handle),
                }
            }
            #[cfg(feature = "pixel")]
            Mode::Pixel => crate::pixel::open(config.window_size, build),
            #[cfg(not(feature = "pixel"))]
            Mode::Pixel => panic!("pixel mode needs the `pixel` feature"),
        };
        let mut backend = Self {
            host,
            timings: VecDeque::new(),
        };
        backend.settle();
        backend
    }

    /// Drain pending work and produce a frame. The one place a frame is ever
    /// made, which is why the node registry can be trusted afterwards.
    ///
    /// Both phases are timed, always — two `Instant::now()` pairs against a
    /// draw measured in milliseconds is not a cost worth a flag, and a flag
    /// would have to be remembered on `drag` and `click` too, which is exactly
    /// where the interesting frames are.
    fn settle(&mut self) {
        let start = Instant::now();
        self.run_until_parked();
        let parked = start.elapsed();

        let drawn = Instant::now();
        self.in_window(|window, cx| window.draw(cx).clear(cx));
        let draw = drawn.elapsed();

        let frame = self.frame();
        if self.timings.len() == TIMING_HISTORY {
            self.timings.pop_front();
        }
        self.timings.push_back(FrameTiming {
            frame,
            parked,
            draw,
        });
    }

    /// Every frame still in the history, oldest first, with the mode that
    /// produced them. Not cleared by reading: a script filters by frame number
    /// against what a command returned, and a read that mutated would make two
    /// readers of one session interfere.
    fn timings(&self) -> Value {
        json!({
            "mode": match self.host {
                Host::Headless { .. } => "headless",
                #[cfg(feature = "pixel")]
                Host::Pixel { .. } => "pixel",
            },
            "frames": self.timings.iter().map(|timing| json!({
                "frame": timing.frame,
                "parkedMs": timing.parked.as_secs_f64() * 1000.,
                "drawMs": timing.draw.as_secs_f64() * 1000.,
            })).collect::<Vec<_>>(),
        })
    }

    fn run_until_parked(&self) {
        match &self.host {
            Host::Headless { cx, .. } => cx.background_executor.run_until_parked(),
            #[cfg(feature = "pixel")]
            Host::Pixel { cx, .. } => cx.run_until_parked(),
        }
    }

    /// The one door to the app. Every command below goes through here.
    pub(crate) fn in_window<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let window = self.window();
        match &mut self.host {
            Host::Headless { cx, .. } => cx.update_window(window, |_, window, cx| f(window, cx)),
            #[cfg(feature = "pixel")]
            Host::Pixel { cx, .. } => cx.update_window(window, |_, window, cx| f(window, cx)),
        }
        .expect("the window under test was closed")
    }

    /// The mode-specific half, for the one caller that needs to reach past the
    /// uniform surface: taking a screenshot needs the pixel host's shot
    /// directory. That caller is the only one, and it is behind `pixel`.
    #[cfg(feature = "pixel")]
    pub(crate) fn host(&self) -> &Host {
        &self.host
    }

    fn window(&self) -> AnyWindowHandle {
        match &self.host {
            Host::Headless { window, .. } => *window,
            #[cfg(feature = "pixel")]
            Host::Pixel { window, .. } => *window,
        }
    }

    fn frame(&self) -> u64 {
        self.registry().0
    }

    fn registry(&self) -> (u64, Vec<Node>) {
        let read = |app: &App| {
            app.try_global::<NodeRegistry>()
                .map(|registry| (registry.frame(), registry.nodes().to_vec()))
                .unwrap_or_default()
        };
        match &self.host {
            Host::Headless { cx, .. } => cx.read(read),
            #[cfg(feature = "pixel")]
            Host::Pixel { cx, .. } => read(&cx.app.borrow()),
        }
    }

    // -- input ----------------------------------------------------------------

    fn click(
        &mut self,
        at: Point<Pixels>,
        button: MouseButton,
        click_count: u32,
        modifiers: Modifiers,
    ) {
        self.mouse_down(at, button, click_count, modifiers);
        self.mouse_up(at, button, click_count, modifiers);
    }

    fn mouse_down(
        &mut self,
        at: Point<Pixels>,
        button: MouseButton,
        click_count: u32,
        modifiers: Modifiers,
    ) {
        self.input(PlatformInput::MouseDown(MouseDownEvent {
            position: at,
            button,
            modifiers,
            click_count: click_count as usize,
            first_mouse: false,
        }));
    }

    /// Put the pointer somewhere with no button down.
    fn place_pointer(&mut self, to: Point<Pixels>, modifiers: Modifiers) {
        self.input(PlatformInput::MouseMove(MouseMoveEvent {
            position: to,
            modifiers,
            pressed_button: None,
        }));
    }

    fn mouse_move(&mut self, to: Point<Pixels>, button: MouseButton, modifiers: Modifiers) {
        self.input(PlatformInput::MouseMove(MouseMoveEvent {
            position: to,
            modifiers,
            pressed_button: Some(button),
        }));
    }

    fn mouse_up(
        &mut self,
        at: Point<Pixels>,
        button: MouseButton,
        click_count: u32,
        modifiers: Modifiers,
    ) {
        self.input(PlatformInput::MouseUp(MouseUpEvent {
            position: at,
            button,
            modifiers,
            click_count: click_count as usize,
        }));
    }

    fn input(&mut self, event: PlatformInput) {
        self.in_window(|window, cx| window.dispatch_event(event, cx));
    }

    fn scroll(&mut self, at: Point<Pixels>, dx: f32, dy: f32, modifiers: Modifiers) {
        self.input(PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(Point {
                x: px(dx),
                y: px(dy),
            }),
            modifiers,
            // `Moved` and not `Started`/`Ended`: a handler that distinguishes
            // them is looking for momentum, and a scripted wheel has none to
            // honestly report.
            touch_phase: TouchPhase::Moved,
        }));
    }

    fn keystrokes(&mut self, keys: &str) -> Result<(), HarnessError> {
        // Parse the whole sequence first: gpui panics on an unparseable
        // keystroke, and a script typo is not a reason to take the app thread
        // down half way through a chord.
        let keys: Vec<Keystroke> = keys
            .split_whitespace()
            .map(|key| {
                Keystroke::parse(key).map_err(|error| {
                    HarnessError::BadCall(format!("bad keystroke {key:?}: {error}"))
                })
            })
            .collect::<Result<_, _>>()?;
        for key in keys {
            self.in_window(|window, cx| window.dispatch_keystroke(key, cx));
        }
        Ok(())
    }

    /// Typing is a sequence of single-character keystrokes, which is what the
    /// platform delivers and therefore what a focused input handler expects.
    fn type_text(&mut self, text: &str) -> Result<(), HarnessError> {
        for character in text.chars() {
            let key = Keystroke::parse(&character.to_string()).map_err(|error| {
                HarnessError::BadCall(format!("cannot type {character:?}: {error}"))
            })?;
            self.in_window(|window, cx| window.dispatch_keystroke(key, cx));
        }
        Ok(())
    }

    fn action(&mut self, name: &str, payload: Option<Value>) -> Result<(), HarnessError> {
        let action = self
            .in_window(|_, cx| cx.build_action(name, payload))
            .map_err(|error| HarnessError::BadAction(format!("{name}: {error}")))?;
        self.in_window(|window, cx| window.dispatch_action(action, cx));
        Ok(())
    }

    // -- pixels ---------------------------------------------------------------

    #[cfg_attr(not(feature = "pixel"), allow(unused_variables))]
    fn screenshot(&mut self, crop: Option<Bounds<Pixels>>) -> Result<Value, HarnessError> {
        match &self.host {
            Host::Headless { .. } => Err(HarnessError::Unsupported(
                "screenshots need pixel mode (`--pixel`); headless mode has no renderer",
            )),
            #[cfg(feature = "pixel")]
            Host::Pixel { .. } => crate::pixel::screenshot(self, crop),
        }
    }
}
