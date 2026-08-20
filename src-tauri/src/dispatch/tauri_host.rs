//! The two host capabilities the desktop app supplies, backed by `AppHandle`.
//!
//! Everything else about `AppHandle` stays in `lib.rs` where it belongs —
//! window lifecycle, menus, plugins, the render and sync loops. `AppServices`
//! is assembled there as a struct literal; a constructor taking every field
//! would have exactly one caller and read no better than the literal.

use serde_json::Value;
use tauri::{AppHandle, Emitter};

use super::{EventSink, Events, Host, HostControl};

/// Tauri's event bus. Delivery failure is dropped — see [`EventSink`].
struct TauriEvents(AppHandle);

impl EventSink for TauriEvents {
    fn emit(&self, event: &str, payload: Value) {
        let _ = self.0.emit(event, payload);
    }
}

struct TauriHost(AppHandle);

impl Host for TauriHost {
    fn exit(&self, code: i32) {
        self.0.exit(code);
    }
}

pub(crate) fn tauri_events(app: &AppHandle) -> Events {
    Events::new(TauriEvents(app.clone()))
}

pub(crate) fn tauri_host(app: &AppHandle) -> HostControl {
    HostControl::new(TauriHost(app.clone()))
}
