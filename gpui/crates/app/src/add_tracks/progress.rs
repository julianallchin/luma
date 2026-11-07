//! The import's progress, drawn: the Progress route's per-song rows and the
//! status bar item that stays up while an import runs or after one fails.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::*;
use gpui_component::Icon;
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::node::{Instrument, Role};
use luma_ui::{glass, ladder};

use super::import::{stage_label, ItemStatus, TrackImportActivity};
use super::rows::{empty_body, loading_body, track_row};
use crate::Luma;

/// The import, one row per song: copying first, then each song's analysis.
pub(super) fn import_progress(
    activity: Option<&TrackImportActivity>,
    app: &Entity<Luma>,
    cx: &mut gpui::App,
) -> AnyElement {
    let Some(activity) = activity else {
        return empty_body("No import running");
    };
    if activity.items.is_empty() {
        if activity.active {
            return loading_body(
                "add-tracks-import-skeleton",
                "Copying songs into the library…",
                app.entity_id(),
                cx,
            );
        }
        let message = activity
            .error
            .clone()
            .unwrap_or_else(|| "Nothing was imported".to_string());
        return float::viewport()
            .child(
                float::list()
                    .child(float::error_row(message.clone()).agent_node(Role::Text, message)),
            )
            .into_any_element();
    }
    let rows: Vec<(String, String, Option<String>, ItemStatus)> = activity
        .items
        .iter()
        .map(|item| {
            (
                item.track_id.clone(),
                item.title.clone(),
                item.art.clone(),
                item.analysis.status(),
            )
        })
        .collect();
    let rows: Rc<[_]> = rows.into();
    float::viewport()
        .child(
            uniform_list("track-import-progress", rows.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let (id, title, art, status) = &rows[index];
                        let key = format!("import-{id}");
                        let (label, subtitle) = import_status_words(status);
                        track_row(
                            RowState::Rest,
                            &key,
                            art.as_deref(),
                            title,
                            subtitle.as_deref(),
                            false,
                        )
                        .child(import_status_mark(status, &label))
                        .id(SharedString::from(key))
                        .agent_node(Role::Row, format!("{title}: {label}"))
                    })
                    .collect()
            })
            .size_full()
            .px(px(8.0)),
        )
        .into_any_element()
}

/// A row's status word, and the line under its title.
///
/// The word is what the row's automation label carries, so a script reads
/// the same sentence a person does: `Analyzing · beats, stems (2 of 8)`.
fn import_status_words(status: &ItemStatus) -> (String, Option<String>) {
    match status {
        ItemStatus::Queued => ("Queued".to_string(), None),
        ItemStatus::Analyzing {
            running,
            settled,
            total,
        } => {
            let stages = running
                .iter()
                .map(|stage| stage_label(stage))
                .collect::<Vec<_>>()
                .join(", ");
            let label = if stages.is_empty() {
                format!("Analyzing ({settled} of {total})")
            } else {
                format!("Analyzing · {stages} ({settled} of {total})")
            };
            let subtitle = (!stages.is_empty()).then(|| capitalized(&stages));
            (label, subtitle)
        }
        ItemStatus::Ready => ("Ready".to_string(), None),
        ItemStatus::Failed(reason) => ("Failed".to_string(), Some(reason.clone())),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Width of the trailing status column, so the stage bar under a word has
/// one length on every row and rows that change state do not shift.
const STATUS_WIDTH: f32 = 96.0;
/// The stage bar's thickness: a hairline you can read a fill on.
const STATUS_BAR_HEIGHT: f32 = 3.0;

/// The trailing column: the status word, and while analysing, how many of
/// the song's stages have settled as a thin bar under it. Stage-count rather
/// than a percentage because no stage reports fractional progress — a bar
/// that crept inside a stage would be inventing it.
fn import_status_mark(status: &ItemStatus, label: &str) -> Div {
    let (word, color): (&str, Hsla) = match status {
        ItemStatus::Queued => ("Queued", ladder::muted_foreground().into()),
        ItemStatus::Analyzing { .. } => ("Analyzing", ladder::foreground_alpha(0.7)),
        ItemStatus::Ready => ("Ready", ladder::muted_foreground().into()),
        ItemStatus::Failed(_) => ("Failed", ladder::danger().into()),
    };
    let fraction = match status {
        ItemStatus::Analyzing { settled, total, .. } if *total > 0 => {
            Some(*settled as f32 / *total as f32)
        }
        _ => None,
    };
    div()
        .flex_none()
        .w(px(STATUS_WIDTH))
        .flex()
        .flex_col()
        .items_end()
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_size(px(11.0))
                .text_color(color)
                .when(*status == ItemStatus::Ready, |el| {
                    el.child(Icon::new(IconName::Check).size(px(11.0)))
                })
                .child(word.to_string())
                .agent_node(Role::Text, format!("Import status: {label}")),
        )
        .children(fraction.map(|fraction| {
            div()
                .w_full()
                .h(px(STATUS_BAR_HEIGHT))
                .rounded_full()
                .bg(glass::ink(0.08))
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction.clamp(0.0, 1.0)))
                        .rounded_full()
                        .bg(glass::ink(0.55)),
                )
        }))
}

/// The status bar's item for an import: there while one runs, and after one
/// that failed until its page has been opened. A press opens that page.
pub(crate) fn item(shell: &Luma) -> Option<crate::status_bar::Item> {
    let activity = shell.track_import.as_ref()?;
    let failed = !activity.active && (activity.failed() > 0 || activity.error.is_some());
    if !activity.active && (!failed || activity.seen) {
        return None;
    }
    let mut item = crate::status_bar::Item::new("track-import-status", activity.summary());
    item.icon = Some(IconName::ArrowDown);
    item.danger = failed;
    item.press = Some(|this, cx| this.show_track_import(cx));
    Some(item)
}
