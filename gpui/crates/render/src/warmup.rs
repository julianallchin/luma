//! Paying for the device before anything is waiting on it.
//!
//! Building [`Gpu`] compiles every pipeline in the renderer. That work has to
//! happen exactly once per process, and the only choice is *when*: inside the
//! first frame somebody asked for, where it reads as the app hanging, or at
//! launch, where it can be named. This module is the second option.
//!
//! # Why the status is process-wide and not a handle
//!
//! There is one device, so there is one warmup, so there is one answer to "is
//! it ready". Handing out a handle would mean every screen that wants to show
//! the answer has to be threaded one from wherever launch happens — and a
//! screen that could not reach it would have to invent a second, weaker answer.
//! [`warm`] is idempotent for the same reason: whoever gets there first starts
//! it, everyone after that just asks.

use std::sync::Mutex;
use std::time::Duration;

use crate::gpu::Gpu;

/// What the process-wide device is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warming {
    /// Nothing has asked for a device yet.
    Cold,
    /// Pipelines are compiling. Nothing can render until this finishes.
    Compiling,
    /// Every pipeline is resident, and how long that took.
    ///
    /// Reported whoever triggered the build, including a renderer that asked
    /// for a device directly without going through [`warm`] — the indicator's
    /// question is whether the GPU is ready, not who woke it.
    Ready {
        /// Wall time spent building the device and its pipelines.
        took: Duration,
    },
    /// This machine has no usable GPU. The message is fit to show a user.
    Unavailable(String),
}

/// Whether [`warm`] has started, and what it found if it finished.
///
/// Only ever holds the two states the device itself cannot report: that a
/// warmup thread is running, and that one failed. Readiness is read from the
/// device, so the two can never disagree.
static STARTED: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// Build the device and every pipeline on a background thread.
///
/// Returns immediately. Call it as early in launch as there is a process —
/// the earlier this starts, the less of it is left when the first stage opens.
/// Calling it more than once is harmless and starts nothing new.
///
/// # Panics
/// Panics if the operating system cannot create the warmup thread.
pub fn warm() {
    let mut started = STARTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if started.is_some() {
        return;
    }
    *started = Some(Ok(()));
    drop(started);
    std::thread::Builder::new()
        .name("luma-warm".into())
        .spawn(|| {
            if let Err(error) = Gpu::shared() {
                *STARTED
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(Err(error.to_string()));
            }
        })
        .expect("warmup thread must start");
}

/// What to tell the user about the GPU right now.
#[must_use]
pub fn warming() -> Warming {
    // The device first, always: it is the fact, and the flag below is only
    // ever a claim about work that may already have finished.
    if let Some(gpu) = Gpu::built() {
        return Warming::Ready {
            took: gpu.built_in(),
        };
    }
    match &*STARTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        None => Warming::Cold,
        Some(Ok(())) => Warming::Compiling,
        Some(Err(error)) => Warming::Unavailable(error.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::{warm, warming, Warming};

    /// Readiness is read from the device rather than from the warmup flag, so
    /// a renderer built without ever calling [`warm`] still reports ready. The
    /// alternative — a screen showing "compiling" over a stage that is already
    /// drawing — is the exact confusion this indirection exists to prevent.
    #[test]
    fn a_device_built_by_anyone_reports_ready() {
        if crate::Renderer::new().is_err() {
            // No GPU on this host; the honest answer is the unavailable one and
            // there is nothing here to assert about readiness.
            return;
        }
        assert!(matches!(warming(), Warming::Ready { .. }));
        warm();
        assert!(matches!(warming(), Warming::Ready { .. }));
    }
}
