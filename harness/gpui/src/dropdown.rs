//! GPUI port of `<Dropdown>` (src/shared/components/ui/dropdown.tsx), closed.
//!
//! Only the trigger is ported: the harness captures the closed state, so the
//! portalled menu is out of scope. The trigger is a `<Button>` whose width
//! comes from the same ghost stack `<Selector>` uses — sized to the widest of
//! (label, ...items) — the one difference being that its chevron is not
//! dimmed: it inherits the trigger's `text-foreground/90`.

use gpui::*;

use crate::{ladder, select};

pub fn luma_dropdown(label: &str, items: &[&str]) -> Div {
    let mut rows = vec![label];
    rows.extend_from_slice(items);
    select::ghost_trigger(label, &rows, ladder::foreground_90().into())
}
