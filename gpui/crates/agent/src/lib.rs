//! An in-process automation harness for the GPUI app, driven by interpreted
//! code.
//!
//! # Why in-process
//!
//! GPUI paints its own widgets. There is no OS control behind a button, so
//! there is no accessibility tree to query and no external driver — AX, UIA,
//! SkyLight — that can see anything but a rectangle of pixels. The only place
//! that knows a button is a button is the element tree, so that is where the
//! instrumentation goes: `luma_ui::node` publishes a frame of nodes during
//! `prepaint`, and this crate turns "the button labelled Back" into a click at
//! a point.
//!
//! # Shape
//!
//! ```text
//!  MCP client ──stdio──▶ mcp ──▶ interp ──Cmd/JSON──▶ pump ──▶ gpui App
//!                       (thread B)                 (main thread)
//! ```
//!
//! Three layers, and the split between them is not negotiable:
//!
//! - [`pump`] owns the whole gpui side and is the only code that touches
//!   `&mut App`. It is a loop over [`protocol::Cmd`]s.
//! - [`interp`] runs QuickJS on a different thread. It holds a channel, never
//!   a gpui handle, so there is no way for a script to re-enter gpui.
//! - [`mcp`] is the stdio transport, and exposes exactly two tools.
//!
//! Only plain `Send` data crosses between them. That is what makes the
//! interpreter safe to run scripts of unknown quality: the worst a script can
//! do is send a command that fails.
//!
//! # The input vocabulary
//!
//! There is one pointer gesture — press, optionally walk, release — and its
//! variations are *parameters*, not separate commands: which
//! [`protocol::Button`], how many clicks, which modifiers are held. So
//! right-click is `click` with `button: "right"`, double-click is `count: 2`,
//! and shift-click is `modifiers: ["shift"]`. Adding a `double_click` command
//! beside `click` would mean two places that know how a press is synthesized,
//! and the second would drift.
//!
//! Modifiers are held for the *whole* gesture, intermediate moves included,
//! because that is what the app reads: an alt-drag that let go of alt before
//! the drop is a move, not a duplicate.
//!
//! # The snapshot invariant
//!
//! Node ids are indices into one frame's registration order, so they mean
//! nothing in any other frame. Every snapshot carries its `frame`, every
//! mutating call carries it back, and a mismatch is a hard error — clicking at
//! coordinates from a frame that has since been redrawn is exactly the failure
//! that makes UI automation untrustworthy. `{restale: "match"}` opts into
//! re-finding the node by `(role, label)`; it is never the default.

pub mod error;
#[cfg(feature = "app")]
pub mod fixture;
pub mod interp;
pub mod mcp;
#[cfg(feature = "pixel")]
mod pixel;
pub mod protocol;
pub mod pump;
#[cfg(feature = "pixel")]
mod shots;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub use error::HarnessError;
pub use interp::{ExecResult, Interpreter, API_DTS};
pub use pump::{Config, Mode, PumpClient, RootFactory, GPU_LIVENESS_TIMEOUT};

/// How many threads may be driving an app at once in one process.
///
/// A *deadline* limit, not a CPU limit: a script polls for a rendered frame
/// with a timeout, and fixtures hold responses for fixed milliseconds, so a
/// harness that gets a sliver of a core fails an assertion that has nothing
/// to do with what it tested.
///
/// The default suits `cargo test`, which runs one binary's tests on as many
/// threads as there are cores. `luma-test` runs its own pool and sets this to
/// the pool's size, so the pool is the one knob.
static HARNESS_CONCURRENCY: AtomicUsize = AtomicUsize::new(6);

/// Set how many threads may drive an app at once. See [`HARNESS_CONCURRENCY`].
pub fn set_harness_concurrency(permits: usize) {
    HARNESS_CONCURRENCY.store(permits.max(1), Ordering::Relaxed);
}

/// Permits, and somewhere to wait for one.
static DRIVING_THREADS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

std::thread_local! {
    /// How many harnesses this thread is holding.
    ///
    /// The permit is per *thread*, not per harness, because a test may keep
    /// two apps open at once — `add_tracks_pixels` shoots an empty library and
    /// a populated one in one test. Counting harnesses would let a full house
    /// of tests each block waiting for a permit none of them can release,
    /// since each is already holding one. Counting threads cannot deadlock: a
    /// thread that is already driving never queues again.
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// This thread's turn at driving an app, given back when its last harness
/// drops.
struct Slot;

impl Slot {
    fn acquire() -> Self {
        if DEPTH.with(|depth| depth.replace(depth.get() + 1)) == 0 {
            let (count, free) = &DRIVING_THREADS;
            let mut driving = count
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while *driving >= HARNESS_CONCURRENCY.load(Ordering::Relaxed) {
                driving = free
                    .wait(driving)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            *driving += 1;
        }
        Self
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if DEPTH.with(|depth| depth.replace(depth.get() - 1)) == 1 {
            let (count, free) = &DRIVING_THREADS;
            let mut driving = count
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *driving -= 1;
            free.notify_one();
        }
    }
}

/// The two tools, over one interpreter and one app.
///
/// Construct this on the interpreter's thread; the pump is already running
/// somewhere else by the time you have a [`PumpClient`].
pub struct Harness {
    interpreter: Interpreter,
    /// Held for the harness's life by [`Harness::headless`]. `None` when the
    /// pump runs on the caller's thread (the MCP binary), which is one app in
    /// a process that exists to hold it and has nothing to contend with.
    _slot: Option<Slot>,
    /// Last, so it drops after the app: see [`Harness::remove_on_drop`].
    library: Option<Library>,
}

/// A seeded library that goes when its harness does, unless the thread is
/// panicking — a failed test's library is the evidence for it, so it stays.
struct Library(std::path::PathBuf);

impl Drop for Library {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("kept the failed test's library: {}", self.0.display());
        } else {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
}

impl Harness {
    pub fn new(client: PumpClient) -> Result<Self, HarnessError> {
        Ok(Self {
            interpreter: Interpreter::new(client)?,
            _slot: None,
            library: None,
        })
    }

    /// Spin up a headless pump on its own thread and attach a harness to it.
    /// This is the shape a test wants: it already owns a thread, and headless
    /// mode does not care which one gpui runs on.
    pub fn headless(mut config: Config, root: RootFactory) -> Result<Self, HarnessError> {
        // The mode already says whether this run may touch a GPU, so it is the
        // one place that can answer for every test at once — including the
        // fixtures that build their own root and would otherwise each have to
        // remember. A headless `cargo test` must not create a device, and the
        // shell's stage pane acquires one wherever it mounts.
        //
        // A caller that set `stage_gpu` itself is left alone, so a suite can be
        // measured or debugged either way without editing fixtures.
        config.runtime.stage_gpu.get_or_insert(match config.mode {
            #[cfg(feature = "pixel")]
            Mode::Pixel => true,
            Mode::Headless => false,
        });
        // Taken before the app opens and held until this harness drops, so a
        // suite cannot start more windows than it can feed. See
        // [`HARNESS_CONCURRENCY`].
        let slot = Slot::acquire();
        Ok(Self {
            interpreter: Interpreter::new(pump::spawn(config, root))?,
            _slot: Some(slot),
            library: None,
        })
    }

    /// Delete `dir` when this harness drops, unless the thread is panicking
    /// (a failed test keeps its library, and says where). Removing files
    /// SQLite still has open is harmless on Unix.
    #[must_use]
    pub fn remove_on_drop(mut self, dir: std::path::PathBuf) -> Self {
        self.library = Some(Library(dir));
        self
    }

    pub fn exec(&mut self, code: &str, timeout: Duration) -> ExecResult {
        self.interpreter.exec(code, timeout)
    }

    /// [`Self::exec`], with `filename` in the stack traces — so an error in a
    /// test file points at that file's own lines.
    pub fn exec_named(&mut self, code: &str, filename: &str, timeout: Duration) -> ExecResult {
        self.interpreter.exec_named(code, filename, timeout)
    }

    pub fn reset(&mut self) -> Result<(), HarnessError> {
        self.interpreter.reset()
    }

    /// A global function `name` for scripts, backed by `function`. See
    /// [`interp::Interpreter::bind`].
    pub fn bind(
        &mut self,
        name: &str,
        function: impl Fn(String) -> Result<String, String> + 'static,
    ) -> Result<(), HarnessError> {
        self.interpreter.bind(name, std::rc::Rc::new(function))
    }
}
