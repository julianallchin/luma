//! The Perform screen's commands: live follow, a venue's controls, and the
//! inputs bound to them.

use std::future::Future;

use luma_lib::controls::Control;
use luma_lib::dispatch::handlers;
use luma_lib::dispatch::handlers::inputs::{Binding, InputState, Mode};
use luma_lib::models::perform::{PerformSource, PerformState};

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

    pub(crate) fn control_list(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<Control>, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("control_list", move |services| async move {
            handlers::controls::control_list(&services, venue_id).await
        })
    }

    pub(crate) fn control_set(
        &self,
        venue_id: &str,
        name: &str,
        value: f64,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, name) = (venue_id.to_owned(), name.to_owned());
        self.call("control_set", move |services| async move {
            handlers::controls::control_set(&services, venue_id, name, value).await
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
