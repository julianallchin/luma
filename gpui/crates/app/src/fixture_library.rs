//! Browsing the bundled QLC+ fixture definitions.
//!
//! The stage page's add-element dialog asks one question — "what goes in
//! next" — over two provenances: catalog pieces and this bundle. So it takes
//! the state and not the picture: its own field drives
//! [`FixtureLibrary::set_query`], [`FixtureLibrary::query`] is what narrows the
//! catalog section, and [`FixtureLibrary::entries`] becomes rows in its own
//! sectioned list. What it does *not* keep is a second query, a second page
//! cursor and a second spelling of the error — which is what
//! `Luma::stage_search_fixtures` was before it was deleted.
//!
//! # What this owns, and what its host owns
//!
//! It owns the query, the page cursor and the rows — everything that is *about
//! browsing*. It does not own where it is stored or which Tokio runtime its
//! calls go on, because those are facts about the host: the host hands it
//! [`FixtureLibrary::page`]'s future to await and calls [`FixtureLibrary::landed`]
//! with the result.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, SharedString, Subscription};

use luma_lib::models::fixtures::FixtureEntry;
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
    /// What the field says when it is empty, and the name it answers to in the
    /// automation tree. The stage page's one field also narrows catalog
    /// pieces.
    placeholder: SharedString,
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
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Luma>,
        on_edit: impl Fn(&mut Luma, String, &mut Context<Luma>) + 'static,
    ) -> Self {
        let placeholder = placeholder.into();
        let field = cx.new({
            let placeholder = placeholder.clone();
            |cx| TextInput::search(placeholder, cx)
        });
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
            placeholder,
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

    /// The rows the last page landed with, for a host that renders them in a
    /// list of its own rather than through [`rows`].
    pub(crate) fn entries(&self) -> &[FixtureEntry] {
        &self.entries
    }

    /// What the last fetch was issued for. The one query — a host that narrows
    /// other lists beside these rows narrows them by this string.
    pub(crate) fn query(&self) -> &str {
        &self.query
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
                state.placeholder.to_string()
            } else {
                state.query.clone()
            })
            .agent_node(Role::Input, state.placeholder.clone())
            .agent_disabled(true)
            .into_any_element();
    }
    slot.child(state.field.clone())
        .agent_node(Role::Input, state.placeholder.clone())
        .agent_focused(focused)
        .into_any_element()
}
