//! Tool calls, as chips on a rail.
//!
//! A chip is the whole of what the transcript shows for a tool call: a state
//! dot, a phrase, a clipped line of detail, and — under a chevron, open by
//! default — the call's input and what came back.
//!
//! # The detail card is generic, not per-tool
//!
//! What a call *is* — over the wire and in storage — is a JSON input and a JSON
//! output, so that is what the card shows, unwrapped one level into `key value`
//! rows so a reader is not decoding a wire format. A per-tool renderer
//! would be a second place that has to know each tool's schema, and it would go
//! stale the first time a tool grew an argument. When a tool earns a bespoke
//! view it can have one; until then the honest generic view beats a table of
//! special cases.
//!
//! # Its height is declared, never measured
//!
//! Collapsed, a chip is exactly [`crate::theme::CHIP_HEIGHT`]. Expanded, it is
//! that plus a card whose lines this module *counts* and multiplies by
//! [`crate::theme::CHIP_DETAIL_LINE`] — clipped at
//! [`crate::theme::CHIP_DETAIL_MAX_LINES`] per section, which is what makes the
//! count bounded. A fold whose height is measured turns every collapse into a
//! relayout of the whole transcript — the same lesson the code block learns by
//! rendering per line.

use gpui::{div, prelude::*, px, AnyElement, Hsla, SharedString};
use gpui_component::{Icon, IconName};
use luma_lib::agent::{ToolPart, ToolState};

use crate::theme::{self, Theme};
use crate::transcript::RowCtx;

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
/// would want. Deliberately shallow: this is the chip's *narration*, and the
/// whole argument object is one chevron away in [`detail_card`].
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

/// One tool call: the summary row, and its detail when it is open.
pub fn chip(tool: &ToolPart, ctx: &RowCtx) -> AnyElement {
    let theme = ctx.theme;
    let text = label(tool);
    let open = ctx.is_expanded(&tool.call_id);
    let chat = ctx.chat.clone();
    let call_id = SharedString::from(tool.call_id.clone());
    let id = SharedString::from(format!("chat-chip-{call_id}"));
    let row = ctx.ix;
    div()
        .flex()
        .flex_none()
        .flex_col()
        .rounded(px(theme::CONTROL_RADIUS))
        .bg(theme::card_glass_bg())
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        // The *summary* is the control, not the whole card: a click handler on
        // the container would collapse the chip out from under anyone reading
        // — or selecting from — the detail it just opened.
        .child(
            div()
                .id(id)
                .h(px(theme::CHIP_HEIGHT))
                .flex()
                .flex_none()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_SM))
                .px(px(theme::SPACE_MD))
                .cursor_pointer()
                .hover(|style| style.bg(theme::glass_hover()))
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| this.toggle_tool(call_id.clone(), row, cx));
                })
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
                .child(
                    div()
                        .size(px(theme::CHIP_CHEVRON))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(theme::CHIP_CHEVRON - 2.0))
                            .text_color(theme.text_faint),
                        ),
                ),
        )
        .when(open, |el| el.child(detail_card(tool, theme)))
        .into_any_element()
}

/// What the call actually was: its input, and what came back.
///
/// Painted on the mono face and never wrapped — one source line is one card
/// line, which is what keeps the open height a multiplication rather than a
/// measurement.
fn detail_card(tool: &ToolPart, theme: &Theme) -> impl IntoElement {
    let failed = matches!(tool.state, ToolState::OutputError);
    let answer = match (&tool.error_text, &tool.output) {
        (Some(error), _) => Some(lines_of_text(error)),
        (None, Some(output)) => Some(lines_of_json(output)),
        (None, None) => None,
    };
    div()
        .flex()
        .flex_col()
        .gap(px(theme::SPACE_SM))
        .px(px(theme::SPACE_MD))
        .pb(px(theme::SPACE_MD))
        .border_t_1()
        .border_color(theme.border)
        .pt(px(theme::SPACE_SM))
        .when_some(tool.input.as_ref(), |el, input| {
            el.child(section(
                "Input",
                lines_of_json(input),
                theme.text_muted,
                theme,
            ))
        })
        .when_some(answer, |el, lines| {
            el.child(section(
                if failed { "Error" } else { "Output" },
                lines,
                if failed {
                    theme.danger
                } else {
                    theme.text_muted
                },
                theme,
            ))
        })
}

/// One labelled block of the detail card.
fn section(
    heading: &'static str,
    lines: Vec<SharedString>,
    tone: Hsla,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(theme::CHIP_DETAIL_LINE))
                .text_size(px(9.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(heading)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .font_family(theme.font_mono.clone())
                .text_size(px(11.0))
                .text_color(tone)
                .children(lines.into_iter().map(|line| {
                    div()
                        .h(px(theme::CHIP_DETAIL_LINE))
                        .flex_none()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(line)
                })),
        )
}

/// Longest detail line a card carries. The card does not scroll horizontally —
/// a chip is a summary, and a reader who needs the untruncated argument wants
/// the tool's own output, not a wider chip.
const DETAIL_LINE_MAX: usize = 72;

/// One JSON value as card lines.
///
/// An object is unwrapped one level into `key  value` rows, and a string is
/// shown as itself. Both are the same decision: braces, quotes and `\n` escapes
/// are how a value is *transmitted*, and a card that shows them makes a reader
/// decode a wire format to read an argument they already understand. Anything
/// deeper stays JSON, which is the honest rendering for a shape this card has
/// no reading of.
fn lines_of_json(value: &serde_json::Value) -> Vec<SharedString> {
    let mut lines = Vec::new();
    match value {
        serde_json::Value::String(text) => push_text(&mut lines, text),
        serde_json::Value::Object(fields) => {
            for (key, field) in fields {
                match field {
                    serde_json::Value::String(text) if !text.trim().contains('\n') => {
                        lines.push(format!("{key}  {}", text.trim()));
                    }
                    serde_json::Value::String(text) => {
                        lines.push(key.clone());
                        push_text(&mut lines, text);
                    }
                    other => lines.push(format!("{key}  {other}")),
                }
            }
        }
        other => push_text(&mut lines, &other.to_string()),
    }
    clipped(lines)
}

/// One block of plain text as card lines.
fn lines_of_text(text: &str) -> Vec<SharedString> {
    let mut lines = Vec::new();
    push_text(&mut lines, text);
    clipped(lines)
}

fn push_text(lines: &mut Vec<String>, text: &str) {
    lines.extend(text.lines().map(|line| line.trim_end().to_string()));
}

/// At most [`theme::CHIP_DETAIL_MAX_LINES`] lines, each clipped to
/// [`DETAIL_LINE_MAX`], with an ellipsis standing in for what was dropped.
///
/// Both bounds exist so the card's height is a count this module knows before
/// it lays anything out — see the module docs.
fn clipped(lines: Vec<String>) -> Vec<SharedString> {
    let over = lines.len() > theme::CHIP_DETAIL_MAX_LINES;
    let mut out: Vec<SharedString> = lines
        .into_iter()
        .take(theme::CHIP_DETAIL_MAX_LINES)
        .map(|line| SharedString::from(clip(&line, DETAIL_LINE_MAX)))
        .collect();
    if over {
        out.push(SharedString::from("…"));
    }
    out
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

    /// The card shows the argument, not the JSON it travelled in.
    #[test]
    fn the_detail_card_unwraps_one_level_of_object() {
        let lines = lines_of_json(&json!({ "code": "ramp.peak()" }));
        assert_eq!(lines, vec![SharedString::from("code  ramp.peak()")]);
    }

    /// …and both of its bounds hold, so the open height stays a count.
    #[test]
    fn the_detail_card_is_bounded_in_both_directions() {
        let long = "y".repeat(400);
        let lines = lines_of_text(&format!("{long}\n").repeat(50));
        assert_eq!(lines.len(), theme::CHIP_DETAIL_MAX_LINES + 1);
        assert!(lines
            .iter()
            .all(|line| line.chars().count() <= DETAIL_LINE_MAX));
        assert_eq!(lines.last().unwrap().as_ref(), "…");
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
