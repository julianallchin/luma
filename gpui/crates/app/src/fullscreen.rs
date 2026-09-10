//! Temporary presentation of the existing visualizer in the current window.
use std::{cell::Cell, rc::Rc, time::Instant};

use gpui::{prelude::*, *};
use luma_ui::{
    ladder, motion,
    node::{Instrument, Role},
};

use crate::{keymap, shell, track_editor, visualizer, Body, Luma};

pub(crate) struct State {
    open: bool,
    from: f32,
    since: Instant,
    reduced: bool,
    /// One sample per render keeps the shell and the overlay in agreement.
    frame_progress: f32,
    /// The embedded slot keeps measuring while the view moves above the shell.
    pub(crate) slot: Rc<Cell<Bounds<Pixels>>>,
    native: Native,
}

/// Fullscreen requests are asynchronous on desktop platforms. A reversal waits
/// for the outstanding request before issuing its opposite.
struct Native {
    original: bool,
    observed: bool,
    pending: Option<bool>,
}

impl Native {
    fn new(fullscreen: bool) -> Self {
        Self {
            original: fullscreen,
            observed: fullscreen,
            pending: None,
        }
    }

    fn observe(&mut self, actual: bool) -> bool {
        let external_exit = self.observed && !actual && self.pending != Some(false);
        self.observed = actual;
        if self.pending == Some(actual) {
            self.pending = None;
        }
        if external_exit {
            self.original = false;
        }
        external_exit
    }

    fn request(&mut self, open: bool) -> bool {
        let desired = open || self.original;
        if self.pending.is_none() && self.observed != desired {
            self.pending = Some(desired);
            true
        } else {
            false
        }
    }

    fn restored(&self) -> bool {
        self.pending.is_none() && self.observed == self.original
    }
}

impl State {
    fn progress(&self) -> f32 {
        let target = if self.open { 1. } else { 0. };
        if self.reduced {
            target
        } else {
            motion::lerp(
                self.from,
                target,
                motion::exit_progress(&motion::SURFACE, self.since),
            )
        }
    }

    fn retarget(&mut self, open: bool) {
        self.from = self.progress();
        self.open = open;
        self.since = Instant::now();
    }

    fn visible(&self) -> bool {
        self.open || self.frame_progress > 0.
    }
}

impl Luma {
    pub(crate) fn fullscreen_presented(&self) -> bool {
        self.fullscreen.as_ref().is_some_and(State::visible)
    }

    pub(crate) fn toggle_visualizer_fullscreen(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &mut self.fullscreen {
            state.retarget(!state.open);
        } else {
            if self.overlay.get().is_some() {
                return;
            }
            let Some(visualizer) = &self.visualizer else {
                return;
            };
            self.fullscreen = Some(State {
                open: true,
                from: 0.,
                since: Instant::now(),
                reduced: motion::reduced_motion(cx),
                frame_progress: 0.,
                slot: Rc::new(Cell::new(visualizer.stage_pane())),
                native: Native::new(window.is_fullscreen()),
            });
        }
        if let Some(visualizer) = &mut self.visualizer {
            visualizer.prepare_presentation();
        }
        window.focus(&self.visualizer_focus, cx);
        cx.notify();
    }

    pub(crate) fn dismiss_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(visualizer) = &mut self.visualizer {
            if visualizer.settings_open {
                visualizer.settings_open = false;
                window.focus(&self.visualizer_focus, cx);
                cx.notify();
                return;
            }
        }
        if let Some(state) = &mut self.fullscreen {
            if state.open {
                state.retarget(false);
                if let Some(visualizer) = &mut self.visualizer {
                    visualizer.prepare_presentation();
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn sync_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = &mut self.fullscreen else {
            return;
        };
        if (state.native.observe(window.is_fullscreen()) || self.visualizer.is_none()) && state.open
        {
            state.retarget(false);
            if let Some(visualizer) = &mut self.visualizer {
                visualizer.prepare_presentation();
            }
        }
        state.reduced = motion::reduced_motion(cx);
        state.frame_progress = state.progress();
        if state.native.request(state.open) {
            window.toggle_fullscreen();
        }
        if !state.visible() && state.native.restored() {
            self.fullscreen = None;
        } else if state.native.pending.is_some()
            || state.frame_progress != if state.open { 1. } else { 0. }
        {
            window.request_animation_frame();
        }
    }
}

/// Measure the normal slot without mounting a second live renderer.
pub(crate) fn placeholder(slot: Rc<Cell<Bounds<Pixels>>>) -> AnyElement {
    canvas(move |bounds, _, _| slot.set(bounds), |_, (), _, _| {})
        .size_full()
        .into_any_element()
}

pub(crate) fn content(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) -> AnyElement {
    if !app.fullscreen_presented() {
        return shell::regions(app, window, cx).into_any_element();
    }
    let progress = app.fullscreen.as_ref().unwrap().frame_progress;
    let shell = (progress < 1.).then(|| shell::regions(app, window, cx));
    let bounds = app.fullscreen.as_ref().unwrap().slot.get();
    let viewport = window.viewport_size();
    let interpolate = |from: Pixels, to: Pixels| px(motion::lerp(from.into(), to.into(), progress));
    let transport = match app.workspace.active_body_mut() {
        Some(Body::TrackEditor(editor)) => {
            Some(track_editor::fullscreen_transport(editor, &cx.entity(), cx))
        }
        _ => None,
    };
    let focus = app.visualizer_focus.clone();
    let Luma {
        visualizer,
        library,
        ..
    } = app;
    let stage = visualizer.as_mut().map(|state| {
        visualizer::visualizer(
            state,
            &cx.entity(),
            library,
            window,
            visualizer::Chrome::Fullscreen { transport },
            &focus,
        )
        .into_any_element()
    });
    div()
        .size_full()
        .relative()
        .children(shell)
        .child(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .bg(ladder::background().opacity(progress)),
        )
        .child(
            div()
                .absolute()
                .left(interpolate(bounds.origin.x, px(0.)))
                .top(interpolate(bounds.origin.y, px(0.)))
                .w(interpolate(bounds.size.width, viewport.width))
                .h(interpolate(bounds.size.height, viewport.height))
                .overflow_hidden()
                .key_context(keymap::context::VISUALIZER_FULLSCREEN)
                .children(stage)
                .agent_node(Role::Card, "Fullscreen visualizer"),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::Native;

    #[test]
    fn rapid_exit_waits_for_native_entry_before_restoring() {
        let mut native = Native::new(false);
        assert!(native.request(true));
        assert!(!native.request(false));
        assert!(!native.observe(true));
        assert!(native.request(false));
        assert!(!native.observe(false));
        assert!(native.restored());
    }

    #[test]
    fn preserves_existing_fullscreen_and_recognizes_os_exit() {
        let mut native = Native::new(true);
        assert!(!native.request(true));
        assert!(!native.request(false));
        assert!(native.restored());
        assert!(native.observe(false));
        assert!(!native.request(false));
        assert!(native.restored());
    }
}
