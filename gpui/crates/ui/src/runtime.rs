//! Per-app runtime knobs: the library it opens, and the dev switches that
//! change what it costs to draw.
//!
//! # Why these are not environment variables
//!
//! Every field here configures *one* app. A test binary runs many apps — the
//! harness gives each its own pump thread — so process-wide `set_var` let two
//! of them contradict each other: whichever wrote last won for *both*, and the
//! loser silently ran against the other's library directory, motion timescale
//! or GPU policy. Nothing failed; the assertions just measured the wrong app.
//!
//! Scoping the knobs to the thread that owns the app makes that
//! unrepresentable. One app, one thread, one [`Runtime`].
//!
//! A real binary installs nothing and has one app anyway, so [`Runtime::with`]
//! falls back to reading the environment once and production behaviour is
//! unchanged. The environment stays the interface for a human running a
//! one-off; it is no longer the interface for a test.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// The knobs an app resolves once at startup.
#[derive(Debug, Clone)]
pub struct Runtime {
    /// App config directory, overriding `$APPCONFIG`. `None` means "wherever
    /// the platform puts it", which is what a real install wants.
    pub config_dir: Option<PathBuf>,
    /// Root of the bundled fixture definitions, overriding the repo's copy.
    pub fixtures_root: Option<PathBuf>,
    /// Snap every animation to its end state. The harness sets this so a test
    /// reads final geometry the frame after it acts instead of racing a slide.
    pub reduced_motion: bool,
    /// Stretches every catalog timeline by this factor, so a screenshot burst
    /// can sample a 200ms tween per frame. Clamped to `0.01..=100`.
    pub motion_scale: f32,
    /// Whether the stage pane may build a wgpu device. Off leaves the pane,
    /// its chrome and its node tree exactly as they are and skips only the
    /// device — which is what a headless run wants, since it pays a full
    /// shader compilation for a viewport it never looks at.
    ///
    /// `None` is "nobody asked", which lets the harness answer from its mode
    /// while an explicit request still wins. Unasked reads as on — see
    /// [`Runtime::stage_gpu_enabled`].
    pub stage_gpu: Option<bool>,
}

impl Default for Runtime {
    /// The environment's answer, for a binary that was launched rather than
    /// driven. Every variable here is a dev escape hatch: unset is production.
    fn default() -> Self {
        Self {
            config_dir: std::env::var_os("LUMA_CONFIG_DIR").map(PathBuf::from),
            fixtures_root: std::env::var_os("LUMA_FIXTURES_ROOT").map(PathBuf::from),
            reduced_motion: std::env::var("LUMA_MOTION").as_deref() == Ok("off"),
            motion_scale: std::env::var("LUMA_MOTION_SCALE")
                .ok()
                .and_then(|value| value.parse::<f32>().ok())
                .filter(|scale| scale.is_finite())
                .map_or(1.0, |scale| scale.clamp(0.01, 100.0)),
            stage_gpu: std::env::var("LUMA_STAGE_GPU")
                .ok()
                .map(|value| !matches!(value.as_str(), "off" | "0")),
        }
    }
}

thread_local! {
    /// Set by [`Runtime::install`] on the thread that owns an app. Empty means
    /// nobody installed one, and [`Runtime::with`] reads the environment
    /// instead.
    static CURRENT: RefCell<Option<Rc<Runtime>>> = const { RefCell::new(None) };
}

impl Runtime {
    /// Make `self` the runtime for every app on this thread.
    ///
    /// Call before the window opens. The harness calls it on the pump thread
    /// it just created, which is why two harnesses in one test binary cannot
    /// see each other's knobs.
    pub fn install(self) {
        CURRENT.with(|current| *current.borrow_mut() = Some(Rc::new(self)));
    }

    /// Whether the stage may build a device, resolved.
    ///
    /// Unasked is **on**: a run that forgets to decide renders normally, which
    /// is the safe direction for a switch whose wrong value is invisible in a
    /// screenshot.
    pub fn stage_gpu_enabled(&self) -> bool {
        self.stage_gpu.unwrap_or(true)
    }

    /// Read the current thread's runtime, falling back to the environment.
    ///
    /// The fallback is deliberately **not** memoized. Caching it would make
    /// the first reader on a thread decide for every later one, which is the
    /// same "whoever got there first wins" bug this module exists to remove —
    /// and the tests that still configure themselves through `set_var` before
    /// opening a library depend on the read being fresh. Threads that draw
    /// install a [`Runtime`] up front and never reach this path.
    pub fn with<R>(read: impl FnOnce(&Runtime) -> R) -> R {
        match CURRENT.with(|current| current.borrow().clone()) {
            Some(runtime) => read(&runtime),
            None => read(&Runtime::default()),
        }
    }
}
