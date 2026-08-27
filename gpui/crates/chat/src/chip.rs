//! Tool calls, as comet's rail: an icon tile, the verb, the argument, and a
//! trailing chevron — unboxed rows at a declared height, with the call's
//! input and output one chevron away. Consecutive calls share one rail and,
//! past one of them, a summary line above it.
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
use luma_ui::node::{Instrument as _, Role as NodeRole};

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

/// One clipped line of narration after the verb.
///
/// Routed by tool, exactly as the TypeScript original routes `formatLabel`:
/// each tool owns what its own call is *about*, and this is the shared layer
/// that bounds the result. The whole argument object is one chevron away in
/// the detail card, so this is a title, not a summary of the arguments.
fn detail(tool: &ToolPart) -> Option<String> {
    let line = match tool.tool_name() {
        "python" => python_detail(tool),
        "skill" => string_arg(tool, "name"),
        // An unknown tool narrates by its verb alone. Guessing at a field name
        // would put whichever argument happened to match into the title.
        _ => None,
    }?;
    Some(clip(&one_line(&line), DETAIL_MAX))
}

/// A python cell's title: **the model-authored purpose, never the code.**
///
/// The purpose is the whole reason the tool asks for one — it is a four-word
/// noun phrase written to complete "Running …", and it says what the cell is
/// *for*. Code says what it does, which the reader can already see by opening
/// the chip, and a clipped first line of source is the least useful 48
/// characters available. The TypeScript side has an explicit test that this
/// does not fall back to code; so does this one.
fn python_detail(tool: &ToolPart) -> Option<String> {
    let purpose = string_arg(tool, "purpose");
    match (purpose, failure_marker(tool)) {
        (None, None) => None,
        (Some(purpose), None) => Some(purpose),
        (None, Some(marker)) => Some(marker),
        (Some(purpose), Some(marker)) => Some(format!("{purpose} · {marker}")),
    }
}

/// `error 1.5s` — appended only when the cell did not simply succeed, so an
/// ordinary run is titled by its purpose alone.
fn failure_marker(tool: &ToolPart) -> Option<String> {
    let output = tool.output.as_ref()?;
    let status = output.get("status")?.as_str()?;
    if status == "ok" {
        return None;
    }
    let seconds = output
        .get("durationMs")
        .and_then(serde_json::Value::as_u64)
        .filter(|ms| *ms > 0)
        .map(|ms| format!(" {:.1}s", ms as f64 / 1000.0))
        .unwrap_or_default();
    Some(format!("{status}{seconds}"))
}

/// One non-empty string argument, trimmed.
fn string_arg(tool: &ToolPart, key: &str) -> Option<String> {
    let value = tool.input.as_ref()?.get(key)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Collapse to one line: a title that wrapped would break the chip's declared
/// height, and a newline in a noun phrase is never meaningful.
fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let head: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// A run of consecutive tool calls, as one rail. With more than one call the
/// rail carries comet's group header — a chevron tile and a summary — above
/// the rows; a lone call is its own row and needs no introduction.
pub fn rail(tools: &[&ToolPart], ctx: &RowCtx) -> AnyElement {
    let theme = ctx.theme;
    let mut rows = div().flex().flex_none().flex_col().min_w_0().flex_1();
    if tools.len() > 1 {
        let running = tools.iter().any(|tool| {
            matches!(
                tool.state,
                ToolState::InputStreaming | ToolState::InputAvailable
            )
        });
        let summary = if running {
            format!("Running {} tools", tools.len())
        } else {
            format!("Ran {} tools", tools.len())
        };
        rows = rows.child(
            div()
                .h(px(theme::CHIP_HEIGHT))
                .flex()
                .flex_none()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_SM))
                .child(tile(IconName::ChevronDown, theme))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(summary)),
                ),
        );
    }
    for tool in tools {
        rows = rows.child(row(tool, ctx));
    }
    // The guide: one hairline running the group's whole height, with the rows
    // inset past it. It is what makes a run of calls read as *one step* rather
    // than as a stack of unrelated chips — and it is a rail, not a border, so a
    // group of one still gets it and the grammar never changes with the count.
    div()
        .flex()
        .flex_row()
        .flex_none()
        .min_w_0()
        .child(
            div()
                .flex_none()
                .ml(px(theme::RAIL_INSET))
                .mr(px(theme::RAIL_GUTTER))
                .w(px(theme::RAIL_WIDTH))
                .bg(theme::ink(0.08)),
        )
        .child(rows)
        .into_any_element()
}

/// The 24px icon tile at a row's leading edge — a rounded wash square holding
/// the tool's mark. The one place a failed call shows its colour.
fn tile(icon: IconName, theme: &Theme) -> gpui::Div {
    tinted_tile(icon, theme.text_muted, theme)
}

fn tinted_tile(icon: IconName, tint: Hsla, _theme: &Theme) -> gpui::Div {
    div()
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(luma_ui::radius::CONTROL))
        .bg(theme::wash(0.06))
        .child(Icon::new(icon).size(px(13.0)).text_color(tint))
}

/// The mark a tool wears in its tile.
fn tool_icon(tool: &str) -> IconName {
    match tool {
        "python" => IconName::SquareTerminal,
        "skill" => IconName::BookOpen,
        _ => IconName::Bot,
    }
}

/// The open card's exact height.
///
/// Declared, never measured — the same rule the module docs state for the chip
/// itself, and the reason a fold can be *animated* at all: a tween needs to
/// know where it is going before it starts, and a measured card only knows
/// after it has been laid out at full size.
///
/// Mirrors the card element below term for term. The two are a pair, and the
/// tests at the bottom of this file are what hold them together.
#[must_use]
pub fn card_height(tool: &ToolPart) -> f32 {
    let mut sections = 0.0;
    let mut lines = 0.0;
    if let Some(input) = tool.input.as_ref() {
        sections += 1.0;
        lines += lines_of_json(input).len() as f32;
    }
    let answer = match (&tool.error_text, &tool.output) {
        (Some(error), _) => Some(lines_of_text(error).len() as f32),
        (None, Some(output)) => Some(lines_of_json(output).len() as f32),
        (None, None) => None,
    };
    if let Some(count) = answer {
        sections += 1.0;
        lines += count;
    }
    if sections == 0.0 {
        return 0.0;
    }
    // Per section: one heading line plus its content lines. Per card: the top
    // rule and its padding, the bottom padding, and one gap between sections.
    let content = (sections + lines) * theme::CHIP_DETAIL_LINE;
    let chrome = 1.0 + theme::SPACE_SM + theme::SPACE_MD;
    content + chrome + (sections - 1.0) * theme::SPACE_SM
}

/// How open a chip's detail is right now, 0..1.
///
/// A tween the panel drives by hand rather than a gpui animation: gpui keys an
/// animation by element id and replays it on remount, and in a virtualized list
/// every scroll back into view *is* a remount — so an animated fold flashes
/// open every time it scrolls past. Comet works around that with an arming
/// window; not creating the replay in the first place is cheaper and cannot
/// drift.
fn openness(open: bool, fold: Option<f32>) -> f32 {
    match fold {
        Some(progress) if open => progress,
        Some(progress) => 1.0 - progress,
        None => f32::from(u8::from(open)),
    }
}

/// One tool call: an unboxed 38px row — tile, narration, trailing chevron —
/// and its detail card when the chevron has been answered.
fn row(tool: &ToolPart, ctx: &RowCtx) -> AnyElement {
    let theme = ctx.theme;
    let text = label(tool);
    let open = ctx.is_expanded(&tool.call_id);
    let openness = openness(open, ctx.fold_progress(&tool.call_id));
    let chat = ctx.chat.clone();
    let call_id = SharedString::from(tool.call_id.clone());
    let id = SharedString::from(format!("chat-chip-{call_id}"));
    let row_ix = ctx.ix;
    let tint = match tool.state {
        ToolState::OutputError => theme.danger,
        _ => theme.text_muted,
    };
    div()
        .flex()
        .flex_none()
        .flex_col()
        // The *summary* is the control, not the whole block: a click handler
        // on the container would collapse the detail out from under anyone
        // reading — or selecting from — what they just opened.
        .child(
            div()
                .id(id)
                .h(px(theme::CHIP_HEIGHT))
                .flex()
                .flex_none()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_SM))
                .rounded(px(luma_ui::radius::CONTROL))
                .cursor_pointer()
                .hover(|style| style.bg(theme::wash(0.04)))
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| this.toggle_tool(call_id.clone(), row_ix, cx));
                })
                .child(tinted_tile(tool_icon(tool.tool_name()), tint, theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(text.clone()),
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
                )
                .agent_node(NodeRole::Chip, text),
        )
        .when(openness > 0.0, |el| {
            // Indented under the narration, past the tile, so the detail reads
            // as the row's own and not as a new block.
            let card = div().pl(px(32.0)).child(detail_card(tool, theme));
            el.child(if openness >= 1.0 {
                // Fully open renders at its natural height. Clamping a settled
                // card to a computed number would turn any drift between
                // `card_height` and the element into a permanent clip; while
                // it is moving, a pixel of drift is a pixel nobody can see.
                card.into_any_element()
            } else {
                div()
                    .h(px(card_height(tool) * openness))
                    .overflow_hidden()
                    .child(card)
                    .into_any_element()
            })
        })
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

    /// A chip with nothing to show has no card, so a fold on it animates
    /// nothing rather than opening an empty box.
    #[test]
    fn a_chip_with_no_detail_has_no_card() {
        let bare = part("python", ToolState::OutputAvailable, None);
        assert_eq!(card_height(&bare), 0.0);
    }

    /// The card's height is a count, and it stays bounded however much the
    /// tool printed — the property that lets a fold be tweened at all.
    #[test]
    fn the_card_height_is_counted_and_bounded() {
        let one = part(
            "python",
            ToolState::OutputAvailable,
            Some(json!({ "code": "a" })),
        );
        let single = card_height(&one);
        assert!(single > 0.0);

        let mut many = one.clone();
        many.output = Some(json!("x\n".repeat(500)));
        let both = card_height(&many);
        assert!(both > single, "a second section must add height");

        // Both sections at their clipped maximum: the ceiling a card can reach.
        let max_lines = (theme::CHIP_DETAIL_MAX_LINES + 1) as f32;
        let ceiling = (2.0 + 2.0 * max_lines) * theme::CHIP_DETAIL_LINE
            + 1.0
            + theme::SPACE_SM
            + theme::SPACE_MD
            + theme::SPACE_SM;
        assert!(both <= ceiling, "{both} exceeded the declared ceiling");
    }

    /// The three fold states, and the one that matters: a chip nobody clicked
    /// is at rest, whichever way it is set. That is what stops a virtualized
    /// list from replaying a fold every time a row scrolls back into view.
    #[test]
    fn a_chip_nobody_clicked_is_at_rest() {
        // No fold in flight: fully open or fully shut, never a fraction. This
        // is the case a virtualized list hits on every scroll-back-into-view.
        assert_eq!(openness(true, None), 1.0);
        assert_eq!(openness(false, None), 0.0);
        // Mid-fold: opening walks up, closing walks down.
        assert_eq!(openness(true, Some(0.25)), 0.25);
        assert_eq!(openness(false, Some(0.25)), 0.75);
        // Both directions still land exactly on their endpoints.
        assert_eq!(openness(true, Some(1.0)), 1.0);
        assert_eq!(openness(false, Some(1.0)), 0.0);
    }

    /// The chip is titled by the model-authored purpose. This is the whole
    /// point of the tool asking for one.
    #[test]
    fn a_python_chip_is_titled_by_its_purpose() {
        let tool = part(
            "python",
            ToolState::OutputAvailable,
            Some(json!({
                "purpose": "an onset analysis",
                "code": "kicks = luma.features.drum_onsets",
            })),
        );
        assert_eq!(label(&tool), "Ran python cell · an onset analysis");
    }

    /// …and **never** falls back to the code. A call with no purpose is titled
    /// by its verb alone rather than by a clipped line of source — the same
    /// assertion the TypeScript side makes.
    #[test]
    fn a_python_chip_never_falls_back_to_showing_code() {
        let tool = part(
            "python",
            ToolState::OutputAvailable,
            Some(json!({ "code": "print(luma.catalog())" })),
        );
        assert_eq!(label(&tool), "Ran python cell");
        assert!(!label(&tool).contains("print"));
    }

    /// A cell that did not simply succeed says so, with how long it took.
    #[test]
    fn a_failed_cell_appends_its_status_and_duration() {
        let mut tool = part(
            "python",
            ToolState::OutputError,
            Some(json!({ "purpose": "a validation pass", "code": "boom()" })),
        );
        tool.output = Some(json!({ "status": "error", "durationMs": 1500 }));
        assert_eq!(
            label(&tool),
            "Ran python cell · a validation pass · error 1.5s"
        );
        // A cell that succeeded carries no marker at all.
        tool.output = Some(json!({ "status": "ok", "durationMs": 1500 }));
        assert_eq!(label(&tool), "Ran python cell · a validation pass");
    }

    /// A skill is titled by the skill it read — its own argument, not python's.
    #[test]
    fn a_skill_chip_is_titled_by_its_name() {
        let tool = part(
            "skill",
            ToolState::OutputAvailable,
            Some(json!({ "name": "beatgrid", "code": "ignored" })),
        );
        assert_eq!(label(&tool), "Read skill · beatgrid");
    }

    /// A tool the vocabulary does not know narrates by its verb alone rather
    /// than promoting whichever argument happens to match a guessed key.
    #[test]
    fn an_unknown_tool_gets_no_detail_from_its_arguments() {
        let tool = part(
            "wobble",
            ToolState::OutputAvailable,
            Some(json!({ "purpose": "nope", "name": "nope", "code": "nope" })),
        );
        assert_eq!(label(&tool), "Ran tool");
    }

    /// However long the purpose, the title stays one clipped line — the chip's
    /// height is declared, so a wrapping title would overflow it.
    #[test]
    fn detail_is_one_clipped_line() {
        let tool = part(
            "python",
            ToolState::OutputAvailable,
            Some(json!({ "purpose": format!("a\n  {}", "x".repeat(200)) })),
        );
        let label = label(&tool);
        assert!(label.starts_with("Ran python cell · a x"), "{label}");
        assert!(label.ends_with('…'));
        assert!(!label.contains('\n'));
    }
}
