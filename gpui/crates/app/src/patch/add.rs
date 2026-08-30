//! Adding N fixtures to the patch, in two pages of one card.
//!
//! Page one picks a definition out of the bundle; page two says which mode and
//! how many, and reads back where they will land. It is a morph rather than two
//! dialogs because it is one decision being narrowed — the same reason the
//! subagents card morphs — and because the table behind it must not blink: the
//! card never resizes between routes, so nothing under it is re-laid out.
//!
//! # The fixtures it makes are unplaced, and that is the whole point
//!
//! Adding here writes a `fixtures` row and an unplaced node and **no edge**
//! (`services::fixture_create`). The rig gains inventory, not geometry; where
//! each one hangs is a drag on the stage page. That is why the preview line
//! ends in `unplaced` rather than offering somewhere to put them.
//!
//! # The role in that line is not computed here
//!
//! `fixture_role` asks the grouping derivation what a definition patched in a
//! mode is *for*. A page that classified it locally would be the second copy of
//! a rule the gauntlet fails a surface for having (AF10) — and it would drift,
//! because the first copy is 80 lines of QLC+ table.

use gpui::prelude::*;
use gpui::Focusable as _;
use gpui::{
    div, px, AnyElement, Context, Entity, FocusHandle, FontWeight, SharedString, Subscription,
    Window,
};

use luma_lib::models::fixtures::{FixtureDefinition, FixtureEntry};
use luma_lib::services::group_derivation::FixtureRole;
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float;
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::fixture_library::{self, FixtureLibrary};
use crate::library::NewFixtures;
use crate::shell::Overlay;
use crate::Luma;

/// One size for both routes: a card that resized between them would spend the
/// morph animating its box instead of its content.
const CARD: MorphSize = MorphSize::new(720.0, 520.0);
/// How many copies the count field will take. A rig is not built one thousand
/// fixtures at a time, and a typo that patched a universe full is worse than a
/// second trip through the dialog.
const MAX_COUNT: f64 = 128.0;

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Route {
    /// Browsing the bundle.
    Library,
    /// One definition picked, by bundle path.
    Configure(SharedString),
}

impl Route {
    fn descriptor(&self) -> RouteDescriptor<Self> {
        RouteDescriptor::exact(self.clone(), CARD.width, CARD.height)
    }
}

/// What page two is deciding about the definition page one picked.
pub(crate) struct Chosen {
    pub(crate) entry: FixtureEntry,
    pub(crate) definition: Option<FixtureDefinition>,
    pub(crate) mode_name: Option<String>,
    /// The role the derivation says this mode lands under, once it has answered.
    pub(crate) role: Option<FixtureRole>,
    pub(crate) count: Entity<DraftedNumber>,
    pub(crate) mode_menu_open: bool,
    pub(crate) error: Option<SharedString>,
    /// Repaints the preview line when the count commits. Held here so it dies
    /// with the choice it belongs to.
    _count_subscription: Subscription,
}

impl Chosen {
    /// The channels the chosen mode patches, or `None` before the definition
    /// has landed. Read out of the definition rather than stored, because a
    /// width beside the mode that produced it is a width that can be stale.
    pub(crate) fn channels(&self) -> Option<usize> {
        let definition = self.definition.as_ref()?;
        let name = self.mode_name.as_deref()?;
        definition
            .modes
            .iter()
            .find(|mode| mode.name == name)
            .map(|mode| mode.channels.len())
    }
}

pub(crate) struct AddFixtures {
    pub(crate) venue_id: String,
    morph: MorphDialog<Route>,
    library: FixtureLibrary,
    chosen: Option<Chosen>,
    search_focus: FocusHandle,
    add_focus: FocusHandle,
    /// Whether the card has claimed the keyboard once. After that focus moves
    /// only when the *route* does — a card that re-took it every frame would
    /// pull the caret out of the count field between keystrokes.
    seated: bool,
}

impl AddFixtures {
    fn route(&self) -> Route {
        self.morph.target_key().clone()
    }
}

impl Luma {
    pub(crate) fn open_add_fixtures(
        &mut self,
        venue_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let venue = venue_id.clone();
        let library = FixtureLibrary::new("Search fixtures…", cx, move |luma, query, cx| {
            luma.add_fixtures_query(query, cx);
        });
        let search_focus = library.field().read(cx).focus_handle(cx);
        let state = AddFixtures {
            venue_id: venue,
            morph: MorphDialog::new(Route::Library.descriptor(), CARD),
            library,
            chosen: None,
            search_focus,
            add_focus: cx.focus_handle().tab_stop(true),
            seated: false,
        };
        self.overlay.open(Overlay::AddFixtures(Box::new(state)));
        self.fetch_fixture_page(cx);
        cx.notify();
    }

    fn add_fixtures_query(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(Overlay::AddFixtures(state)) = self.overlay.open_mut() {
            state.library.set_query(query);
        }
        self.fetch_fixture_page(cx);
        cx.notify();
    }

    /// Ask the bundle for the next page, if there is one to ask for.
    pub(crate) fn fetch_fixture_page(&mut self, cx: &mut Context<Self>) {
        // Two disjoint fields of `self`: the overlay's browsing state, and the
        // command seam it asks through.
        let Some(Overlay::AddFixtures(state)) = self.overlay.open_mut() else {
            return;
        };
        let generation = state.library.generation();
        let Some(pending) = state.library.page(&self.library) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let page = pending.await;
            this.update(cx, |this, cx| {
                if let Some(Overlay::AddFixtures(state)) = this.overlay.open_mut() {
                    state.library.landed(generation, page);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Page one's answer: morph to page two and read the definition.
    fn choose_fixture(&mut self, entry: FixtureEntry, window: &mut Window, cx: &mut Context<Self>) {
        let reduced = luma_ui::motion::reduced_motion(cx);
        let pending = self.library.fixture_definition(&entry.path);
        let path = entry.path.clone();
        let count = cx.new(|cx| {
            DraftedNumber::new("count", 1.0, 1.0, MAX_COUNT, COUNT_FIELD_WIDTH, window, cx)
        });
        let count_subscription =
            cx.subscribe(&count, |_: &mut Luma, _, event: &NumberEvent, cx| {
                let NumberEvent::Committed(_) = *event;
                cx.notify();
            });
        let Some(Overlay::AddFixtures(state)) = self.overlay.open_mut() else {
            return;
        };
        state.chosen = Some(Chosen {
            entry,
            definition: None,
            mode_name: None,
            role: None,
            count,
            mode_menu_open: false,
            error: None,
            _count_subscription: count_subscription,
        });
        state.morph.request(
            Route::Configure(path.clone().into()).descriptor(),
            std::time::Instant::now(),
            reduced,
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let definition = pending.await;
            this.update(cx, |this, cx| {
                let Some(Overlay::AddFixtures(state)) = this.overlay.open_mut() else {
                    return;
                };
                let Some(chosen) = state.chosen.as_mut() else {
                    return;
                };
                if chosen.entry.path != path {
                    return;
                }
                match definition {
                    Ok(definition) => {
                        // The first mode is the definition's own default, and a
                        // dialog that opened on "no mode" would have nothing to
                        // preview.
                        chosen.mode_name = definition.modes.first().map(|mode| mode.name.clone());
                        chosen.definition = Some(definition);
                        chosen.error = None;
                    }
                    Err(error) => chosen.error = Some(error.to_string().into()),
                }
                let mode = chosen.mode_name.clone();
                cx.notify();
                if let Some(mode) = mode {
                    this.read_fixture_role(path, mode, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Ask the derivation which branch this mode lands under.
    fn read_fixture_role(&mut self, path: String, mode_name: String, cx: &mut Context<Self>) {
        let pending = self.library.fixture_role(&path, &mode_name);
        cx.spawn(async move |this, cx| {
            let role = pending.await;
            this.update(cx, |this, cx| {
                if let Some(Overlay::AddFixtures(state)) = this.overlay.open_mut() {
                    if let Some(chosen) = state.chosen.as_mut() {
                        if chosen.entry.path == path
                            && chosen.mode_name.as_deref() == Some(&mode_name)
                        {
                            chosen.role = role.ok();
                            cx.notify();
                        }
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn pick_add_mode(&mut self, mode_name: String, cx: &mut Context<Self>) {
        let Some(Overlay::AddFixtures(state)) = self.overlay.open_mut() else {
            return;
        };
        let Some(chosen) = state.chosen.as_mut() else {
            return;
        };
        chosen.mode_name = Some(mode_name.clone());
        chosen.mode_menu_open = false;
        chosen.role = None;
        let path = chosen.entry.path.clone();
        cx.notify();
        self.read_fixture_role(path, mode_name, cx);
    }

    fn toggle_add_mode_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(Overlay::AddFixtures(state)) = self.overlay.open_mut() {
            if let Some(chosen) = state.chosen.as_mut() {
                chosen.mode_menu_open = !chosen.mode_menu_open;
            }
        }
        cx.notify();
    }

    /// Back to the bundle. The chosen definition is dropped with the route it
    /// belonged to — reopening it re-reads the same file.
    pub(crate) fn add_fixtures_back(&mut self, cx: &mut Context<Self>) -> bool {
        let reduced = luma_ui::motion::reduced_motion(cx);
        let Some(Overlay::AddFixtures(state)) = self.overlay.open_mut() else {
            return false;
        };
        if state.route() == Route::Library {
            return false;
        }
        state.chosen = None;
        state.morph.request(
            Route::Library.descriptor(),
            std::time::Instant::now(),
            reduced,
        );
        cx.notify();
        true
    }

    /// Patch the batch, then close. The dialog closes first so the table it
    /// refreshes is not doing so behind a scrim.
    fn commit_add_fixtures(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::AddFixtures(state)) = self.overlay.as_open() else {
            return;
        };
        let venue_id = state.venue_id.clone();
        let Some(chosen) = state.chosen.as_ref() else {
            return;
        };
        let (Some(mode_name), Some(channels)) = (chosen.mode_name.clone(), chosen.channels())
        else {
            return;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = chosen.count.read(cx).value().round().max(1.0) as usize;
        let spec = NewFixtures {
            manufacturer: chosen.entry.manufacturer.clone(),
            model: chosen.entry.model.clone(),
            mode_name,
            fixture_path: chosen.entry.path.clone(),
            channels: i64::try_from(channels).unwrap_or(1),
            count,
        };
        let pending = self.library.add_fixtures(&venue_id, spec);
        self.close_overlay(cx);
        cx.spawn(async move |this, cx| {
            let made = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.patch_mut(&venue_id) {
                    // What the button just made true, in the page's own words:
                    // inventory, not geometry. The count is the batch's, not
                    // the field's, so a partial batch says what it managed.
                    match &made {
                        Ok(rows) => state.say(format!(
                            "Added {}, unplaced",
                            crate::patch::plural(rows.len(), "fixture")
                        )),
                        Err(error) => state.say(crate::patch::refusal_message(error)),
                    }
                }
                this.reload_patch(venue_id, cx);
            })
            .ok();
        })
        .detach();
    }
}

const COUNT_FIELD_WIDTH: f32 = 72.0;

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub(crate) fn render(
    state: &AddFixtures,
    app: &Entity<Luma>,
    window: &Window,
    _cx: &mut gpui::App,
) -> AnyElement {
    let sample = state.morph.sample(std::time::Instant::now());
    let app = app.clone();
    morph::card(&sample, "Add fixtures dialog", move |route, mode| {
        frame(state, route, mode, &app, window)
    })
}

/// Keep the morph running. Called from the shell's per-frame tick like every
/// other animated dialog.
pub(crate) fn tick(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) {
    let reduced = luma_ui::motion::reduced_motion(cx);
    let Some(Overlay::AddFixtures(state)) = app.overlay.open_mut() else {
        return;
    };
    if state.morph.tick(std::time::Instant::now(), reduced) {
        window.request_animation_frame();
    }
    // A route that has just landed wants the keyboard; a route that landed
    // some frames ago does not, because by then the operator may have put it
    // somewhere — the count field, most of the time.
    let landed = state.morph.take_focus_after_commit().is_some();
    if state.seated && !landed {
        return;
    }
    state.seated = true;
    let wanted = match state.morph.target_key() {
        Route::Library => state.search_focus.clone(),
        Route::Configure(_) => state.add_focus.clone(),
    };
    window.focus(&wanted, cx);
}

fn frame(
    state: &AddFixtures,
    route: &Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground())
        .child(header(state, route, mode, app, window))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(body(state, route, mode, app)),
        )
        .child(footer(state, route, mode, app, window))
        .into_any_element()
}

fn header(
    state: &AddFixtures,
    route: &Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> impl IntoElement {
    let close = app.clone();
    let back = app.clone();
    let interactive = mode == ContentMode::Interactive;
    float::header_band()
        .when(matches!(route, Route::Configure(_)), |band| {
            band.child(
                float::key_cap_pressable(float::key_cap())
                    .id("add-fixtures-back")
                    .tab_index(0)
                    .on_click(move |_, _, cx| {
                        back.update(cx, |this, cx| {
                            this.add_fixtures_back(cx);
                        });
                    })
                    .child("←")
                    .agent_node(Role::Button, "Back"),
            )
        })
        .child(match route {
            Route::Library => fixture_library::search_field(
                &state.library,
                interactive,
                state.search_focus.is_focused(window),
            ),
            Route::Configure(_) => {
                let title: SharedString = state
                    .chosen
                    .as_ref()
                    .map_or_else(
                        || "Fixture".to_string(),
                        |chosen| format!("{} {}", chosen.entry.manufacturer, chosen.entry.model),
                    )
                    .into();
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.clone())
                    .agent_node(Role::Text, title)
                    .into_any_element()
            }
        })
        .child(
            float::key_cap_pressable(float::key_cap())
                .id("close-add-fixtures")
                .tab_index(0)
                .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
                .child("esc")
                .agent_node(Role::Button, "Close"),
        )
}

fn body(state: &AddFixtures, route: &Route, mode: ContentMode, app: &Entity<Luma>) -> AnyElement {
    match route {
        Route::Library => {
            let picked = app.clone();
            let interactive = mode == ContentMode::Interactive;
            fixture_library::rows(
                &state.library,
                state.chosen.as_ref().map(|c| c.entry.path.as_str()),
                std::rc::Rc::new(move |entry, window, cx| {
                    if !interactive {
                        return;
                    }
                    let entry = entry.clone();
                    picked.update(cx, |this, cx| this.choose_fixture(entry, window, cx));
                }),
            )
        }
        Route::Configure(_) => configure(state, app),
    }
}

/// Page two: mode, count, and what the two of them will produce.
fn configure(state: &AddFixtures, app: &Entity<Luma>) -> AnyElement {
    let Some(chosen) = state.chosen.as_ref() else {
        return div().size_full().into_any_element();
    };
    if let Some(error) = &chosen.error {
        return luma_ui::plate(error.to_string(), ladder::danger()).into_any_element();
    }
    let Some(definition) = chosen.definition.as_ref() else {
        return luma_ui::plate(
            "Reading the definition…".to_string(),
            ladder::muted_foreground(),
        )
        .into_any_element();
    };
    let mode_name: SharedString = chosen
        .mode_name
        .clone()
        .unwrap_or_else(|| "—".to_string())
        .into();
    let channels = chosen.channels().unwrap_or(0);
    let toggled = app.clone();
    // The chip sizes itself to the widest thing it can ever say (see
    // `select::ghost_stack`). Handed nothing, it collapses to the chevron and
    // the mode name spills outside the plate — which is also outside the
    // clickable area.
    let mode_names: Vec<&str> = definition
        .modes
        .iter()
        .map(|mode| mode.name.as_str())
        .collect();

    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(18.0))
        .p(px(20.0))
        .child(float::field_row(
            "Mode",
            div()
                .relative()
                .child(
                    float::picker_chip(mode_name.as_ref(), &mode_names)
                        .id("add-mode")
                        .on_click(move |_, _, cx| {
                            toggled.update(cx, |this, cx| this.toggle_add_mode_menu(cx));
                        })
                        .agent_node(Role::Select, mode_name.clone()),
                )
                .when(chosen.mode_menu_open, |slot| {
                    slot.child(mode_list(definition, app))
                }),
        ))
        .child(float::field_row("Count", chosen.count.clone()))
        .child(preview(state, chosen, channels))
        .into_any_element()
}

fn mode_list(definition: &FixtureDefinition, app: &Entity<Luma>) -> AnyElement {
    let dismissed = app.clone();
    float::anchored_below(
        "add-mode-menu",
        luma_ui::CONTROL_HEIGHT,
        float::Dismiss::on_press_out(move |_, cx| {
            dismissed.update(cx, |this, cx| this.toggle_add_mode_menu(cx));
        }),
        float::popover_card()
            .children(definition.modes.iter().map(|mode| {
                let picked = app.clone();
                let name = mode.name.clone();
                let channels = mode.channels.len();
                float::menu_row(float::RowState::Rest, format!("add-mode-{}", mode.name))
                    .id(SharedString::from(format!("add-mode-row-{}", mode.name)))
                    .h(px(26.0))
                    .px(px(10.0))
                    .child(format!("{} · {channels} ch", mode.name))
                    .on_click(move |_, _, cx| {
                        let name = name.clone();
                        picked.update(cx, |this, cx| this.pick_add_mode(name, cx));
                    })
                    .agent_node(Role::Button, format!("{} · {channels} ch", mode.name))
            }))
            .into_any_element(),
    )
}

/// The sentence the Add button is about to make true.
fn preview(state: &AddFixtures, chosen: &Chosen, channels: usize) -> AnyElement {
    let _ = state;
    let role: SharedString = chosen.role.map_or_else(
        || SharedString::from("…"),
        |role| SharedString::from(role.display_name()),
    );
    let line = format!(
        "Lands in {role} / unplaced · {} each",
        crate::patch::plural(channels, "channel")
    );
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .pt(px(4.0))
        .child(
            div()
                .text_size(px(12.5))
                .text_color(ladder::foreground_alpha(0.72))
                .child(line.clone())
                .agent_node(Role::Text, line),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.45))
                .child(
                    "Addresses come from the venue's allocator. Nothing is placed \
                     in the room until you hang it on the stage.",
                ),
        )
        .into_any_element()
}

fn footer(
    state: &AddFixtures,
    route: &Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> impl IntoElement {
    let added = app.clone();
    let ready = state
        .chosen
        .as_ref()
        .is_some_and(|chosen| chosen.channels().is_some());
    let interactive = mode == ContentMode::Interactive;
    float::footer_band().child(div().flex_1().min_w_0()).when(
        matches!(route, Route::Configure(_)),
        |band| {
            band.child(
                float::btn_primary("Add")
                    .id("add-fixtures-commit")
                    .track_focus(&state.add_focus)
                    .tab_index(0)
                    .when(!ready, |button| button.opacity(float::INERT_OPACITY))
                    .on_click(move |_, _, cx| {
                        if !interactive || !ready {
                            return;
                        }
                        added.update(cx, |this, cx| this.commit_add_fixtures(cx));
                    })
                    .agent_node(Role::Button, "Add")
                    .agent_disabled(!ready)
                    .agent_focused(state.add_focus.is_focused(window)),
            )
        },
    )
}
