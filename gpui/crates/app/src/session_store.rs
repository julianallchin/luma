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
    /// The item is the signed-in account's rather than the device's — see
    /// [`Luma::account_key`].
    account: bool,
    value: T,
    writes: u64,
}

impl<T: Default> Stored<T> {
    pub(crate) fn new(key: &'static str) -> Self {
        Self {
            key,
            account: false,
            value: T::default(),
            writes: 0,
        }
    }
}

impl<T> Stored<T> {
    /// Keep the item per account: it names things only that account can
    /// reach, such as its conversations.
    pub(crate) fn per_account(mut self) -> Self {
        self.account = true;
        self
    }
}

impl<T: Default + DeserializeOwned> Stored<T> {
    /// Take in what the store held at launch.
    pub(crate) fn read(key: &'static str, stored: Option<&str>) -> Self {
        Self {
            key,
            account: false,
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
    /// `key` for the signed-in account: where an item that is one person's,
    /// not the device's, is kept. Resolved when the item is read or stored,
    /// so a write still waiting at a sign-out lands under whose it was.
    pub(crate) fn account_key(&self, key: &str) -> String {
        format!("{key}:{}", self.library.user_id().unwrap_or_default())
    }

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
        let (name, account, generation) = (stored.key, stored.account, stored.writes);
        let key = if account {
            self.account_key(name)
        } else {
            name.to_owned()
        };
        self.unsaved_session.insert(key.clone(), json);
        let wait = self.library.debounce(SAVE_WAIT);
        cx.spawn(async move |this, cx| {
            wait.await;
            let Ok(Some(write)) = this.update(cx, |this, _| {
                if item(this).writes != generation {
                    return None;
                }
                let json = this.unsaved_session.remove(&key)?;
                Some(this.library.set_session_item(&key, &json))
            }) else {
                return;
            };
            let _ = write.await;
        })
        .detach();
    }

    /// Write every item still waiting out its debounce, now: the shell is
    /// going away, and the timer that would have written them goes with it.
    pub(crate) fn flush_session(&mut self) -> Vec<impl std::future::Future<Output = ()> + use<>> {
        std::mem::take(&mut self.unsaved_session)
            .into_iter()
            .map(|(key, json)| {
                let write = self.library.set_session_item(&key, &json);
                async move {
                    let _ = write.await;
                }
            })
            .collect()
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
