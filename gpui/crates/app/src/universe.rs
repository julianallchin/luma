//! The Universe setup tab's first honest surface.
//!
//! The web designer has fixture grouping, patch editing and footprint tools.
//! Those writes are not ported yet, so this tab does not counterfeit them: it
//! lists the venue's real patched fixtures from [`crate::Library::venue_rig`]
//! and names the editing surface it will grow into.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::ScrollableElement;
use luma_ui::ladder;
use luma_ui::node::{Instrument, Role};

use crate::library::Rig;
use crate::shell::Body;
use crate::tabs::Target;
use crate::{LibraryError, Luma};

#[derive(Debug, Clone)]
pub(crate) struct FixtureRow {
    label: String,
    model: String,
    patch: String,
}

pub(crate) struct Universe {
    venue_name: String,
    rows: Vec<FixtureRow>,
    loaded: bool,
    error: Option<String>,
}

impl Universe {
    pub(crate) fn loading(venue_name: String) -> Self {
        Self {
            venue_name,
            rows: Vec::new(),
            loaded: false,
            error: None,
        }
    }

    pub(crate) fn venue_name(&self) -> &str {
        &self.venue_name
    }

    pub(crate) fn loaded(&mut self, result: Result<Rig, LibraryError>) {
        self.loaded = true;
        match result {
            Ok(rig) => {
                self.rows = rig
                    .fixtures
                    .into_iter()
                    .map(|fixture| FixtureRow {
                        label: fixture.label.unwrap_or_else(|| fixture.model.clone()),
                        model: format!("{} · {}", fixture.manufacturer, fixture.model),
                        patch: format!(
                            "Universe {} · {}–{}",
                            fixture.universe,
                            fixture.address,
                            fixture
                                .address
                                .saturating_add(fixture.num_channels.saturating_sub(1))
                        ),
                    })
                    .collect();
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

impl Luma {
    /// Reveal the selected venue's patch as one target-keyed workspace tab.
    pub(crate) fn open_universe(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let venue_name = browser.venue_name().to_string();
        let target = Target::Universe {
            venue: venue_id.clone(),
        };
        if self.workspace.body_mut(&target).is_some() {
            self.workspace.select(&target);
            cx.notify();
            return;
        }

        let pending = self.library.venue_rig(&venue_id);
        let state = Universe::loading(venue_name);
        self.open_tab(target.clone(), move || Body::Universe(Box::new(state)), cx);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(Body::Universe(state)) = this.workspace.body_mut(&target) {
                    state.loaded(result);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

pub(crate) fn universe(state: &Universe) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .child(
            div()
                .flex_shrink_0()
                .px(px(18.0))
                .py(px(14.0))
                .border_b_1()
                .border_color(ladder::trim())
                .child(luma_ui::silkscreen("UNIVERSE SETUP"))
                .child(
                    div()
                        .mt(px(5.0))
                        .text_size(px(12.0))
                        .text_color(ladder::muted_foreground())
                        .child(
                            "Patched fixtures now; groups, addressing and footprint editing next.",
                        ),
                ),
        )
        .child(match &state.error {
            Some(error) => luma_ui::plate(
                format!("Failed to load the venue patch: {error}"),
                ladder::danger(),
            ),
            None if !state.loaded => luma_ui::plate(
                "Loading venue patch…".to_string(),
                ladder::muted_foreground(),
            ),
            None if state.rows.is_empty() => luma_ui::plate(
                "No fixtures are patched in this venue.".to_string(),
                ladder::muted_foreground(),
            ),
            None => fixture_rows(&state.rows).into_any_element(),
        })
        .agent_node(Role::Card, format!("{} Universe setup", state.venue_name))
}

fn fixture_rows(rows: &[FixtureRow]) -> impl IntoElement {
    div()
        .flex_1()
        .overflow_y_scrollbar()
        .py(px(8.0))
        .children(rows.iter().enumerate().map(|(index, fixture)| {
            div()
                .h(px(46.0))
                .px(px(18.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .when(index.is_multiple_of(2), |row| row.bg(ladder::stripe()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(ladder::foreground_90())
                                .child(fixture.label.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(10.0))
                                .text_color(ladder::muted_foreground())
                                .child(fixture.model.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(ladder::muted_foreground())
                        .child(fixture.patch.clone()),
                )
                .agent_node(Role::Row, fixture.label.clone())
        }))
}
