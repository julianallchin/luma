//! The Perform screen's commands: live follow, the venue's Perform set and
//! its layout, the venue's controls, and the inputs bound to them.

use std::future::Future;

use luma_lib::controls::Control;
use luma_lib::dispatch::handlers;
use luma_lib::dispatch::handlers::inputs::{Binding, InputState, Mode};
use luma_lib::models::perform::{PerformLayout, PerformPattern, PerformSource, PerformState};

use super::{Library, LibraryError};

impl Library {
    pub(crate) fn perform_state(
        &self,
    ) -> impl Future<Output = Result<PerformState, LibraryError>> + use<> {
        self.call("perform_state", |services| async move {
            handlers::perform::perform_state(&services).await
        })
    }

    pub(crate) fn perform_start(
        &self,
        venue_id: &str,
        source: PerformSource,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("perform_start", move |services| async move {
            handlers::perform::perform_start(&services, venue_id, source).await
        })
    }

    pub(crate) fn perform_stop(&self) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("perform_stop", |services| async move {
            handlers::perform::perform_stop(&services).await
        })
    }

    pub(crate) fn perform_set_latency(
        &self,
        latency_ms: f32,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("perform_set_latency", move |services| async move {
            handlers::perform::perform_set_latency(&services, latency_ms).await
        })
    }

    /// A venue's Perform set: its patterns, the top of the set first.
    pub(crate) fn perform_patterns(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<PerformPattern>, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("list_perform_patterns", |services| async move {
            handlers::perform::list_perform_patterns(&services, venue_id).await
        })
    }

    /// Store a Perform pattern, new or changed, under its own id.
    pub(crate) fn save_perform_pattern(
        &self,
        venue_id: &str,
        pattern: PerformPattern,
    ) -> impl Future<Output = Result<PerformPattern, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("save_perform_pattern", |services| async move {
            handlers::perform::save_perform_pattern(&services, venue_id, pattern).await
        })
    }

    /// Delete a Perform pattern, its cell, and every fader cell's hold on it.
    pub(crate) fn delete_perform_pattern(
        &self,
        venue_id: &str,
        pattern_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, pattern_id) = (venue_id.to_owned(), pattern_id.to_owned());
        self.call("delete_perform_pattern", |services| async move {
            handlers::perform::delete_perform_pattern(&services, venue_id, pattern_id).await
        })
    }

    /// A venue's Perform layout, `None` before one is saved.
    pub(crate) fn perform_layout(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Option<PerformLayout>, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("get_perform_layout", |services| async move {
            handlers::perform::get_perform_layout(&services, venue_id).await
        })
    }

    pub(crate) fn save_perform_layout(
        &self,
        venue_id: &str,
        layout: PerformLayout,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("save_perform_layout", |services| async move {
            handlers::perform::save_perform_layout(&services, venue_id, layout).await
        })
    }

    /// Add a busk page made from a venue's rig, beside its set. Answers the
    /// page's id.
    pub(crate) fn perform_add_busk_page(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<String, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("perform_add_busk_page", |services| async move {
            handlers::perform::perform_add_busk_page(&services, venue_id).await
        })
    }

    /// Compile a venue's Perform set, to play as its controls say.
    pub(crate) fn perform_open_set(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("perform_open_set", |services| async move {
            handlers::perform::perform_open_set(&services, venue_id).await
        })
    }

    /// Set a control of the open Perform set's venue: a pad or a fader.
    pub(crate) fn perform_control(
        &self,
        name: &str,
        value: f64,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let name = name.to_owned();
        self.call("perform_control", move |services| async move {
            handlers::perform::perform_control(&services, name, value).await
        })
    }

    /// Press (`true`) or let go of buttons of the open Perform set, one
    /// after another: a button node of a pattern's graph, its pad among
    /// them, or a target. Each plays as the button says: a toggle flips on
    /// the press, a flash holds until the release.
    pub(crate) fn perform_press(
        &self,
        presses: Vec<(String, bool)>,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("perform_press", move |services| async move {
            for (name, down) in presses {
                handlers::perform::perform_press(&services, name, down).await?;
            }
            Ok(())
        })
    }

    pub(crate) fn perform_set_bpm(
        &self,
        venue_id: &str,
        bpm: f64,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("perform_set_bpm", move |services| async move {
            handlers::perform::perform_set_bpm(&services, venue_id, bpm).await
        })
    }

    pub(crate) fn control_list(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<Control>, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("control_list", move |services| async move {
            handlers::controls::control_list(&services, venue_id).await
        })
    }

    pub(crate) fn input_open(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<InputState, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("input_open", move |services| async move {
            handlers::inputs::input_open(&services, venue_id).await
        })
    }

    pub(crate) fn input_state(
        &self,
    ) -> impl Future<Output = Result<InputState, LibraryError>> + use<> {
        self.call("input_state", |services| async move {
            handlers::inputs::input_state(&services).await
        })
    }

    pub(crate) fn input_learn(
        &self,
        venue_id: &str,
        control: &str,
        mode: Mode,
    ) -> impl Future<Output = Result<Binding, LibraryError>> + use<> {
        let (venue_id, control) = (venue_id.to_owned(), control.to_owned());
        self.call("input_learn", move |services| async move {
            handlers::inputs::input_learn(&services, venue_id, control, mode).await
        })
    }

    pub(crate) fn input_cancel_learn(
        &self,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("input_cancel_learn", |services| async move {
            handlers::inputs::input_cancel_learn(&services).await
        })
    }

    pub(crate) fn input_update_binding(
        &self,
        venue_id: &str,
        binding_id: &str,
        mode: Mode,
        invert: bool,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, binding_id) = (venue_id.to_owned(), binding_id.to_owned());
        self.call("input_update_binding", move |services| async move {
            handlers::inputs::input_update_binding(&services, venue_id, binding_id, mode, invert)
                .await
        })
    }

    pub(crate) fn input_unbind(
        &self,
        venue_id: &str,
        binding_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, binding_id) = (venue_id.to_owned(), binding_id.to_owned());
        self.call("input_unbind", move |services| async move {
            handlers::inputs::input_unbind(&services, venue_id, binding_id).await
        })
    }
}
