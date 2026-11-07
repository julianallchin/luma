//! This device's remembered state: one debounced writer for every session
//! item the shell keeps — each venue's mode and open score
//! ([`crate::saved_venues`]) and each venue's camera
//! ([`crate::saved_cameras`]).
//!
//! Nothing here is synced. A session item is a convenience, not a document:
//! a value that does not parse is nothing saved.

use std::time::Duration;

use gpui::Context;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::Luma;

/// How long a change waits for the next one before it is written. A burst —
/// an orbit drag, a run of mode switches — is written once, after it settles.
const SAVE_WAIT: Duration = Duration::from_millis(500);

/// One session item: what the store holds under `key`, as last read or
/// written, and the correlation for the write in flight.
pub(crate) struct Stored<T> {
    key: &'static str,
    value: T,
    writes: u64,
}

impl<T: Default> Stored<T> {
    pub(crate) fn new(key: &'static str) -> Self {
        Self {
            key,
            value: T::default(),
            writes: 0,
        }
    }
}

impl<T: Default + DeserializeOwned> Stored<T> {
    /// Take in what the store held at launch.
    pub(crate) fn read(key: &'static str, stored: Option<&str>) -> Self {
        Self {
            key,
            value: stored
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default(),
            writes: 0,
        }
    }
}

impl<T> Stored<T> {
    pub(crate) fn value(&self) -> &T {
        &self.value
    }
}

impl Luma {
    /// Store `value` under `item`'s key when it differs from what is stored.
    /// Called every frame by the items' owners; the write lands once the
    /// changes stop.
    ///
    /// `item` names the field rather than borrowing it, because the write
    /// comes back to the shell later to ask whether it is still the newest.
    pub(crate) fn store_session<T: Serialize + PartialEq + 'static>(
        &mut self,
        item: fn(&mut Luma) -> &mut Stored<T>,
        value: T,
        cx: &mut Context<Self>,
    ) {
        let stored = item(self);
        if stored.value == value {
            return;
        }
        let Ok(json) = serde_json::to_string(&value) else {
            return;
        };
        stored.value = value;
        stored.writes += 1;
        let (key, generation) = (stored.key, stored.writes);
        let wait = self.library.debounce(SAVE_WAIT);
        cx.spawn(async move |this, cx| {
            wait.await;
            let Ok(Some(write)) = this.update(cx, |this, _| {
                (item(this).writes == generation).then(|| this.library.set_session_item(key, &json))
            }) else {
                return;
            };
            let _ = write.await;
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_value_is_nothing_saved() {
        let read: Stored<Vec<u32>> = Stored::read("key", Some("not json"));
        assert!(read.value().is_empty());
        let read: Stored<Vec<u32>> = Stored::read("key", Some("[1,2]"));
        assert_eq!(read.value(), &vec![1, 2]);
    }
}
