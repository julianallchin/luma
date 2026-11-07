//! This machine's rebound shortcuts: read at launch, written as they change,
//! and put into the keymap live. The table and what a binding means are
//! [`crate::keymap`]'s; Settings → Shortcuts is where a person changes them.

use gpui::Context;

use crate::keymap::{self, Overrides, Shortcut};
use crate::session_store::Stored;
use crate::Luma;

impl Luma {
    /// Read the stored rebinds and bind them. Until they land the defaults
    /// are bound, which is also what a store that cannot be read leaves.
    pub(crate) fn load_shortcuts(&mut self, cx: &mut Context<Self>) {
        let stored = self.library.get_session_item(keymap::OVERRIDES_KEY);
        cx.spawn(async move |this, cx| {
            let stored = stored.await;
            this.update(cx, |this, cx| {
                this.shortcuts =
                    Stored::read(keymap::OVERRIDES_KEY, stored.ok().flatten().as_deref());
                keymap::apply(this.shortcuts.value(), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Give `row` the keys `keys`, or its defaults again for `None`. A row
    /// set to its defaults keeps no entry, so a later change of default
    /// reaches it.
    pub(crate) fn rebind_shortcut(
        &mut self,
        row: &Shortcut,
        keys: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.shortcuts.value().clone();
        match keys {
            Some(keys) if keys.iter().map(String::as_str).ne(row.keys.iter().copied()) => {
                next.insert(row.id.to_string(), keys);
            }
            _ => {
                next.remove(row.id);
            }
        }
        self.set_shortcuts(next, cx);
    }

    /// Every row back to its defaults.
    pub(crate) fn reset_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.set_shortcuts(Overrides::new(), cx);
    }

    fn set_shortcuts(&mut self, next: Overrides, cx: &mut Context<Self>) {
        self.store_session(|this| &mut this.shortcuts, next, cx);
        keymap::apply(self.shortcuts.value(), cx);
        cx.notify();
    }
}
