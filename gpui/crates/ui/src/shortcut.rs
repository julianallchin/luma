//! The one way a key is drawn: an action's current binding, as caps.
//!
//! A hint never spells its key. It names the *action* and asks gpui's keymap
//! which keys say it now, so a rebind in Settings → Shortcuts reaches every
//! legend, menu row and chip at once, and a hint cannot promise a key the
//! keymap does not hold. With no binding there is no cap: a legend that
//! names no key is better than one that names the wrong one.
//!
//! Glyphs are the platform's: `⌘⇧P` on macOS, `Ctrl+Shift+P` elsewhere —
//! the order and the symbols each platform's own menus use.

use gpui::{Action, AnyElement, App, Div, IntoElement, KeyBinding, Modifiers, ParentElement};

use crate::float;
use crate::node::{Instrument, Role};

/// The keys `action` answers to now, first-registered first: the table's
/// own key before its alternates (the same one a macOS menu shows).
pub fn bindings(action: &dyn Action, cx: &App) -> Vec<KeyBinding> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap.bindings_for_action(action).cloned().collect()
}

/// How `action`'s first binding is written, or `None` while it has none.
pub fn text(action: &dyn Action, cx: &App) -> Option<String> {
    bindings(action, cx).first().map(binding_text)
}

/// One binding as its keys show: a sequence's strokes space-separated.
pub fn binding_text(binding: &KeyBinding) -> String {
    binding
        .keystrokes()
        .iter()
        .map(|stroke| keystroke_text(stroke.modifiers(), stroke.key()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One keystroke as its keys show on this platform.
pub fn keystroke_text(modifiers: &Modifiers, key: &str) -> String {
    let mac = cfg!(target_os = "macos");
    let mut out = String::new();
    // Apple's order (⌃⌥⇧⌘), and the words-joined-by-plus everyone else's
    // menus use.
    let parts: [(bool, &str, &str); 5] = [
        (modifiers.function, "fn", "Fn"),
        (modifiers.control, "⌃", "Ctrl"),
        (modifiers.alt, "⌥", "Alt"),
        (modifiers.shift, "⇧", "Shift"),
        (modifiers.platform, "⌘", "Super"),
    ];
    for (held, glyph, word) in parts {
        if held {
            if mac {
                out.push_str(glyph);
            } else {
                out.push_str(word);
                out.push('+');
            }
        }
    }
    out.push_str(&key_text(key, mac));
    out
}

fn key_text(key: &str, mac: bool) -> String {
    let named = match key {
        "enter" => Some(if mac { "↵" } else { "Enter" }),
        "space" => Some("Space"),
        "backspace" => Some(if mac { "⌫" } else { "Backspace" }),
        "delete" => Some(if mac { "⌦" } else { "Delete" }),
        "escape" => Some("Esc"),
        "tab" => Some(if mac { "⇥" } else { "Tab" }),
        "up" => Some("↑"),
        "down" => Some("↓"),
        "left" => Some("←"),
        "right" => Some("→"),
        "home" => Some("Home"),
        "end" => Some("End"),
        "pageup" => Some("PgUp"),
        "pagedown" => Some("PgDn"),
        _ => None,
    };
    match named {
        Some(name) => name.to_string(),
        None if key.chars().count() == 1 => key.to_uppercase(),
        // F-keys and anything else gpui names: as named, capitalised.
        None => {
            let mut chars = key.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// A [`float::key_cap`] holding `keys`, named to the automation tree by what
/// it says.
pub fn cap_of(keys: impl Into<String>) -> AnyElement {
    let keys = keys.into();
    float::key_cap()
        .child(keys.clone())
        .agent_node(Role::Text, keys)
        .into_any_element()
}

/// `action`'s first binding as a cap, or nothing while it has none.
pub fn cap(action: &dyn Action, cx: &App) -> Option<AnyElement> {
    text(action, cx).map(cap_of)
}

/// A footer legend entry: `action`'s cap, then the verb it performs —
/// [`float::key_hint_text`] for a key the keymap owns.
pub fn hint(action: &dyn Action, label: &'static str, cx: &App) -> Option<Div> {
    let cap = cap(action, cx)?;
    Some(
        float::hint_row()
            .child(cap)
            .child(float::key_hint_label(label)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(source: &str) -> String {
        let stroke = gpui::Keystroke::parse(source).unwrap();
        keystroke_text(&stroke.modifiers, &stroke.key)
    }

    #[test]
    fn the_platform_key_reads_as_the_platform_writes_it() {
        if cfg!(target_os = "macos") {
            assert_eq!(shown("secondary-shift-p"), "⇧⌘P");
            assert_eq!(shown("cmd-enter"), "⌘↵");
            assert_eq!(shown("ctrl-z"), "⌃Z");
        } else {
            assert_eq!(shown("secondary-shift-p"), "Ctrl+Shift+P");
            assert_eq!(shown("ctrl-enter"), "Ctrl+Enter");
        }
        assert_eq!(shown("space"), "Space");
        assert_eq!(shown("f"), "F");
        assert_eq!(shown("f5"), "F5");
    }
}
