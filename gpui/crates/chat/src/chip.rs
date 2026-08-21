//! Tool calls, as chips on a rail.
//!
//! A chip is the whole of what the transcript shows for a tool call: a state
//! dot, a phrase, and — when the tool gave one — a clipped line of detail. It
//! is deliberately not expandable yet; the expanded detail views are per-tool
//! renderers and there is no second tool to generalize from.
//!
//! # Its height is declared, never measured
//!
//! [`crate::theme::CHIP_HEIGHT`] is a constant, and the chip is laid out at
//! exactly that height. A fold whose height is *measured* turns every collapse
//! into a relayout of the whole transcript — the same lesson the code block
//! learns by rendering per line.

use gpui::{div, prelude::*, px, Hsla, SharedString};
use luma_lib::agent::{ToolPart, ToolState};

use crate::theme::{self, Theme};

/// How a tool call is narrated, in the two tenses it can be read in.
///
/// Ported from `tool-verbs.ts` + the track agent's `VOCAB`: the vocabulary is
/// small and closed, and a tool the table does not know narrates as "tool"
/// rather than as its wire name — a chip is prose, not a symbol.
struct Verb {
    running: &'static str,
    past: &'static str,
    noun: &'static str,
}

const DEFAULT_VERB: Verb = Verb {
    running: "Running",
    past: "Ran",
    noun: "tool",
};

fn verb(tool: &str) -> Verb {
    match tool {
        "python" => Verb {
            running: "Running",
            past: "Ran",
            noun: "python cell",
        },
        "skill" => Verb {
            running: "Reading",
            past: "Read",
            noun: "skill",
        },
        _ => DEFAULT_VERB,
    }
}

/// Longest inline detail a chip carries before it is clipped, in characters.
/// The chip is one line at a declared height; detail that would wrap has to
/// stop somewhere, and stopping at a constant is what keeps every chip the
/// same shape.
const DETAIL_MAX: usize = 48;

/// The chip's label — also what the automation tree reports, which is why the
/// phrasing lives in one function rather than being assembled at the element.
pub fn label(tool: &ToolPart) -> SharedString {
    let verb = verb(tool.tool_name());
    let tense = match tool.state {
        ToolState::InputStreaming | ToolState::InputAvailable => verb.running,
        _ => verb.past,
    };
    match detail(tool) {
        Some(detail) => format!("{tense} {} · {detail}", verb.noun).into(),
        None => format!("{tense} {}", verb.noun).into(),
    }
}

/// One clipped line of the call's arguments, when they say something a person
/// would want. Deliberately shallow: a chip narrates, and the argument object
/// belongs in a detail view that does not exist yet.
fn detail(tool: &ToolPart) -> Option<String> {
    let input = tool.input.as_ref()?;
    let value = input
        .get("code")
        .or_else(|| input.get("name"))
        .or_else(|| input.get("path"))?
        .as_str()?;
    let line: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    Some(clip(&line, DETAIL_MAX))
}

fn clip(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let head: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The state dot's colour: amber while the call is in flight, emerald once it
/// answered, red when it failed. The one place the chip carries hue.
fn dot(state: &ToolState, theme: &Theme) -> Hsla {
    match state {
        ToolState::InputStreaming | ToolState::InputAvailable => theme.warning,
        ToolState::OutputError => theme.danger,
        _ => theme.success,
    }
}

/// One tool call.
pub fn chip(tool: &ToolPart, theme: &Theme) -> impl IntoElement {
    let text = label(tool);
    div()
        .h(px(theme::CHIP_HEIGHT))
        .flex()
        .flex_none()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_SM))
        .px(px(theme::SPACE_MD))
        .rounded(px(theme::CONTROL_RADIUS))
        .bg(theme::ink(0.035))
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .w(px(6.0))
                .h(px(6.0))
                .flex_none()
                .rounded_full()
                .bg(dot(&tool.state, theme)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(text),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn part(name: &str, state: ToolState, input: Option<serde_json::Value>) -> ToolPart {
        ToolPart {
            name: Some(name.to_string()),
            dynamic: false,
            call_id: "call-1".into(),
            state,
            input,
            output: None,
            error_text: None,
        }
    }

    #[test]
    fn the_verb_carries_the_tense() {
        let running = part("python", ToolState::InputAvailable, None);
        assert_eq!(label(&running), "Running python cell");
        let done = part("python", ToolState::OutputAvailable, None);
        assert_eq!(label(&done), "Ran python cell");
    }

    /// A tool the vocabulary does not know narrates as prose, not as its wire
    /// name — the chip is something a person reads.
    #[test]
    fn an_unknown_tool_narrates_by_the_default_verb() {
        let unknown = part("wobble", ToolState::OutputAvailable, None);
        assert_eq!(label(&unknown), "Ran tool");
    }

    #[test]
    fn detail_is_one_clipped_line() {
        let long = "x".repeat(200);
        let tool = part(
            "python",
            ToolState::OutputAvailable,
            Some(json!({ "code": format!("print(\n  {long}\n)") })),
        );
        let label = label(&tool);
        assert!(label.starts_with("Ran python cell · print( "), "{label}");
        assert!(label.ends_with('…'));
        assert!(!label.contains('\n'));
    }
}
