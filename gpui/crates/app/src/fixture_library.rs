//! Browsing the bundled QLC+ fixture definitions.
//!
//! Shared, because two surfaces want it: the patch page's add dialog picks a
//! definition to patch N unplaced copies of, and the stage page's distribution
//! popup picks one to hang along a truss. They differ entirely in what they do
//! with the answer and not at all in how the question is asked, so the search,
//! the paging and the rows live here once.
//!
//! # The stage page has not adopted it yet
//!
//! `Luma::stage_search_fixtures` (`stage/mod.rs`) still keeps its own `query`
//! and `results` on `Distribute` and asks `search_fixtures` with a hard-coded
//! first page of 40 — no paging, no exhaustion, no manufacturer headings, and
//! its own spelling of the error case. That is the second browser, and it is
//! the one to delete: this module is already the general form of it. Adopting
//! it is `Distribute { library: FixtureLibrary, .. }`, [`FixtureLibrary::new`]
//! with an `on_edit` that routes back to the popup, [`rows`] with an `on_pick`
//! that distributes, and the host's existing fetch loop calling
//! [`FixtureLibrary::page`] / [`FixtureLibrary::landed`] — which is exactly
//! what `Luma::fetch_fixture_page` does for the add dialog. Nothing here is
//! shaped around the dialog: the component owns browsing, and the host owns
//! storage and the runtime, which is the whole point of the split below.
//!
//! # What this owns, and what its host owns
//!
//! It owns the query, the page cursor and the rows — everything that is *about
//! browsing*. It does not own where it is stored or which Tokio runtime its
//! calls go on, because those are facts about the host: the host hands it
//! [`FixtureLibrary::page`]'s future to await and calls [`FixtureLibrary::landed`]
//! with the result. That is what keeps one component usable from an overlay and
//! from a tab body without either of them being the other's special case.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{div, px, AnyElement, App, Context, Entity, SharedString, Subscription, Window};

use luma_lib::models::fixtures::FixtureEntry;
use luma_ui::float::{self, RowState};
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::text_input::{self, TextInput};

use crate::library::Library;
use crate::{LibraryError, Luma};

/// How many definitions a page holds. Big enough that the common search lands
/// in one, small enough that the empty query does not decode the whole bundle.
pub(crate) const PAGE: usize = 60;

pub(crate) struct FixtureLibrary {
    field: Entity<TextInput>,
    /// The query, mirrored out of the field. The field is the editor; this is
    /// what the fetch was issued for.
    query: String,
    entries: Vec<FixtureEntry>,
    /// How many rows have been asked for. The next page starts here.
    offset: usize,
    /// The last page came back short, so there is nothing further to ask for.
    exhausted: bool,
    loading: bool,
    error: Option<SharedString>,
    /// Bumped per query; a page landing under an older one is dropped.
    generation: u64,
    _subscription: Subscription,
}

impl FixtureLibrary {
    /// `on_edit` routes a keystroke back to wherever the host keeps this — the
    /// one thing the component cannot know.
    pub(crate) fn new(
        cx: &mut Context<Luma>,
        on_edit: impl Fn(&mut Luma, String, &mut Context<Luma>) + 'static,
    ) -> Self {
        let field = cx.new(|cx| TextInput::search("Search fixtures…", cx));
        let subscription = cx.subscribe(&field, move |luma, field, event, cx| {
            if event == &text_input::Event::Edited {
                let query = field.read(cx).text().to_string();
                on_edit(luma, query, cx);
            } else {
                cx.notify();
            }
        });
        Self {
            field,
            query: String::new(),
            entries: Vec::new(),
            offset: 0,
            exhausted: false,
            loading: true,
            error: None,
            generation: 0,
            _subscription: subscription,
        }
    }

    pub(crate) fn field(&self) -> &Entity<TextInput> {
        &self.field
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// A new query. Drops the rows it had — a list that kept the old ones while
    /// the new page flew would be answering a question nobody asked any more.
    pub(crate) fn set_query(&mut self, query: String) {
        self.query = query;
        self.entries.clear();
        self.offset = 0;
        self.exhausted = false;
        self.loading = true;
        self.error = None;
        self.generation += 1;
    }

    /// Ask for another page. `None` when there is nothing left to ask for or a
    /// page is already in flight — which is what makes "call this on scroll"
    /// safe to do every frame.
    pub(crate) fn page(
        &mut self,
        library: &Library,
    ) -> Option<impl std::future::Future<Output = Result<Vec<FixtureEntry>, LibraryError>> + use<>>
    {
        if self.exhausted || (self.loading && self.offset > 0) {
            return None;
        }
        self.loading = true;
        Some(library.search_fixtures(&self.query, self.offset, PAGE))
    }

    /// Take a page. `generation` is the one it was issued under.
    pub(crate) fn landed(
        &mut self,
        generation: u64,
        page: Result<Vec<FixtureEntry>, LibraryError>,
    ) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match page {
            Ok(page) => {
                self.exhausted = page.len() < PAGE;
                self.offset += page.len();
                self.entries.extend(page);
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
    }
}

/// What a picked row does. Boxed at render time rather than held in state, so
/// unlike [`crate::confirm::Action`] there is nothing here that outlives a
/// frame — a picker's callback is part of the element tree, not of the app.
pub(crate) type OnPick = Rc<dyn Fn(&FixtureEntry, &mut Window, &mut App)>;

/// The rows, grouped by manufacturer.
///
/// Grouped rather than flat because the bundle is fifteen thousand definitions
/// and a manufacturer is how anyone narrows it: `search_fixtures` matches both
/// halves of the name, so "chauvet rogue" and "rogue" both land, and the
/// heading is what tells you which of four Rogues you are looking at.
pub(crate) fn rows(state: &FixtureLibrary, picked: Option<&str>, on_pick: OnPick) -> AnyElement {
    if let Some(error) = &state.error {
        return float::viewport()
            .child(float::list().child(float::error_row(error.clone())))
            .into_any_element();
    }
    if state.entries.is_empty() {
        let message: SharedString = if state.loading {
            "Reading the fixture bundle…".into()
        } else {
            format!("No fixture matches “{}”", state.query).into()
        };
        return float::viewport()
            .child(
                float::list()
                    .child(float::empty_row(message.clone()).agent_node(Role::Text, message)),
            )
            .into_any_element();
    }

    let mut list = float::list().id("fixture-library").overflow_y_scroll();
    let mut heading: Option<&str> = None;
    for entry in &state.entries {
        if heading != Some(entry.manufacturer.as_str()) {
            heading = Some(&entry.manufacturer);
            list = list.child(
                float::section_heading(entry.manufacturer.clone())
                    .agent_node(Role::Text, entry.manufacturer.clone()),
            );
        }
        let label = format!("{} {}", entry.manufacturer, entry.model);
        let chosen = picked == Some(entry.path.as_str());
        let pick = on_pick.clone();
        let row = entry.clone();
        list = list.child(
            float::menu_row(
                RowState::of(chosen, false),
                format!("fixture-{}", entry.path),
            )
            .id(SharedString::from(format!("fixture-row-{}", entry.path)))
            .w_full()
            .h(px(30.0))
            .px(px(10.0))
            .on_click(move |_, window, cx| pick(&row, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.5))
                    .child(entry.model.clone()),
            )
            .agent_node(Role::Row, label),
        );
    }
    if state.loading {
        list = list.child(
            div()
                .px(px(10.0))
                .py(px(6.0))
                .text_size(px(11.0))
                .text_color(ladder::muted_foreground())
                .child("Loading more…"),
        );
    } else if !state.exhausted {
        list = list.child(
            div()
                .px(px(10.0))
                .py(px(6.0))
                .text_size(px(11.0))
                .text_color(ladder::muted_foreground())
                .child(format!("{} shown — scroll for more", state.entries.len())),
        );
    }
    float::viewport().child(list).into_any_element()
}

/// The search field, or its typed text painted flat for a morph copy in
/// flight — an in-flight layer owns no focus handle, so it cannot host the
/// live field.
pub(crate) fn search_field(state: &FixtureLibrary, interactive: bool, focused: bool) -> AnyElement {
    let slot = div().flex_1().min_w_0().text_size(px(14.0));
    if !interactive {
        return slot
            .text_color(if state.query.is_empty() {
                ladder::muted_foreground().into()
            } else {
                ladder::foreground_alpha(1.0)
            })
            .child(if state.query.is_empty() {
                "Search fixtures…".to_string()
            } else {
                state.query.clone()
            })
            .agent_node(Role::Input, "Search fixtures…")
            .agent_disabled(true)
            .into_any_element();
    }
    slot.child(state.field.clone())
        .agent_node(Role::Input, "Search fixtures…")
        .agent_focused(focused)
        .into_any_element()
}
