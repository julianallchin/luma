//! The add-element dialog: the fixture library or the catalog palette, as a
//! searchable list, and the verbs that open it and take a row into the hand.

use std::collections::BTreeMap;

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Div, Entity, Window};
use luma_scene::catalog::{pieces, PaletteGroup};
use luma_scene::venue::NodeKind;
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::fixture_library::{self, FixtureLibrary};
use crate::Luma;

use super::hand::{self, Hand, Held, Holding, Offer};
use super::page::StagePage;
use super::preview;

/// One palette row: a catalog entry, plus how this row means to put it down.
///
/// A tower and a stick are the same generator on different footings, so the
/// palette is a list of *rows* rather than a list of pieces — the row is what
/// carries the operator's intent.
pub(crate) struct PaletteRow {
    pub(crate) catalog_ref: &'static str,
    pub(crate) label: String,
    pub(crate) group: PaletteGroup,
    /// `true` for the row that stands a stick on end.
    pub(crate) tower: bool,
}

/// Every palette row, in catalog order within each group.
pub(crate) fn palette_rows() -> Vec<PaletteRow> {
    let mut rows: Vec<PaletteRow> = pieces()
        .iter()
        .map(|piece| PaletteRow {
            catalog_ref: piece.id,
            label: piece.display_name.to_string(),
            group: piece.palette_group,
            tower: false,
        })
        .collect();
    // The tower rides beside the stick it is made of, in the same group.
    let at = rows
        .iter()
        .position(|row| row.catalog_ref == hand::TRUSS_STRAIGHT)
        .map_or(rows.len(), |i| i + 1);
    rows.insert(
        at,
        PaletteRow {
            catalog_ref: hand::TRUSS_STRAIGHT,
            label: "Truss · tower".to_string(),
            group: PaletteGroup::Trusses,
            tower: true,
        },
    );
    rows
}

impl Luma {
    /// Open the add-element dialog.
    /// Open the chooser and put the caret in its search field — the button's
    /// and the `A` key's shared path. Search-first is the whole dialog: it
    /// opens ready to be typed at.
    pub(crate) fn stage_open_chooser_focused(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.stage_open_chooser(cx);
        if let Some(page) = self.stage_page_mut() {
            let field = page.library.field().clone();
            let handle = gpui::Focusable::focus_handle(field.read(cx), cx);
            window.focus(&handle, cx);
        }
    }

    pub(crate) fn stage_open_chooser(&mut self, cx: &mut Context<Self>) {
        // The preview pane's thumbnails render on their own thread; the first
        // open kicks them off and this poll repaints as the files land.
        preview::warm();
        if !preview::all_ready() {
            cx.spawn(async move |this, cx| {
                for _ in 0..120 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                    let done = preview::all_ready();
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                    if done {
                        return;
                    }
                }
            })
            .detach();
        }
        // The page's own view says what is being built: its stage list adds
        // structure, everything else adds lights.
        let offer = match self.workspace.active_body() {
            Some(crate::shell::Body::Patch(state)) if state.view == crate::patch::View::Stage => {
                Offer::Pieces
            }
            _ => Offer::Fixtures,
        };
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Choosing(hand::Choosing {
                offer,
                ..Default::default()
            });
        }
        if let Some(page) = self.stage_page_mut() {
            page.closing.finish_close();
            page.closing.open(());
        }
        // Opened on a fresh question. A dialog that came back holding the last
        // search would be answering one nobody asked — and the field is the
        // page's, not the hand's, so nothing else clears it.
        if let Some(page) = self.stage_page_mut() {
            page.library.set_query(String::new());
            let field = page.library.field().clone();
            field.update(cx, |field, cx| field.set_text("", cx));
        }
        self.stage_fetch_fixture_page(cx);
        cx.notify();
    }

    /// Offer the other half of the catalogue, from its first row.
    pub(super) fn stage_offer(&mut self, offer: Offer, cx: &mut Context<Self>) {
        if let Some(build) = self.build_mut() {
            if let Hand::Choosing(choosing) = &mut build.hand {
                choosing.offer = offer;
                choosing.cursor = 0;
            }
        }
        cx.notify();
    }

    /// A press on the preview: the start of turning it.
    fn stage_grab_preview(&mut self, at: gpui::Pixels, cx: &mut Context<Self>) {
        if let Some(Hand::Choosing(choosing)) = self.build_mut().map(|build| &mut build.hand) {
            choosing.turning = Some((at, choosing.turn));
        }
        cx.notify();
    }

    /// The pointer moved with the preview held: turn it by how far.
    fn stage_turn_preview(&mut self, at: gpui::Pixels, cx: &mut Context<Self>) {
        if let Some(Hand::Choosing(choosing)) = self.build_mut().map(|build| &mut build.hand) {
            if let Some((from, turn)) = choosing.turning {
                let steps = (f32::from(at - from) / PREVIEW_TURN_PX).round() as i32;
                // Dragging right brings the subject's left side round, as if
                // it were held by the edge nearest the pointer.
                if choosing.turn != turn - steps {
                    choosing.turn = turn - steps;
                    cx.notify();
                }
            }
        }
    }

    fn stage_release_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(Hand::Choosing(choosing)) = self.build_mut().map(|build| &mut build.hand) {
            if choosing.turning.take().is_some() {
                cx.notify();
            }
        }
    }

    /// A keystroke in the dialog's field.
    pub(super) fn stage_fixture_query(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(page) = self.stage_page_mut() {
            page.library.set_query(query);
        }
        self.stage_fetch_fixture_page(cx);
        cx.notify();
    }

    /// Ask the bundle for the next page of fixture rows, if there is one.
    fn stage_fetch_fixture_page(&mut self, cx: &mut Context<Self>) {
        // Two disjoint fields of `self`: the tab's browsing state, and the
        // command seam it asks through.
        let Some(crate::shell::Body::Patch(state)) = self.workspace.active_body_mut() else {
            return;
        };
        let page = &mut state.stage;
        let generation = page.library.generation();
        let Some(pending) = page.library.page(&self.library) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let page = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.stage_page_mut() {
                    state.library.landed(generation, page);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Take a row out of the dialog and into the hand.
    ///
    /// A library fixture arrives without its mode or its measurements — the
    /// definition is a file read — so the hand takes it immediately and the
    /// two facts land when they land. What the hand *cannot* do until they do
    /// is commit, which is what [`ConfigureView::ready`] gates.
    pub(crate) fn stage_take(&mut self, what: Holding, cx: &mut Context<Self>) {
        if let Some(page) = self.stage_page_mut() {
            page.closing.begin_close(cx);
        }
        let path = match &what {
            Holding::Fixture { path, .. } => Some(path.clone()),
            _ => None,
        };
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Placing(Held::new(what));
        }
        if let Some(path) = path {
            let pending = self.library.fixture_definition(&path);
            let shown = path;
            cx.spawn(async move |this, cx| {
                let Ok(definition) = pending.await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    // The room draws a fixture from its definition, and one
                    // this venue has never held is not among the rig's.
                    if let Some(stage) = this.visualizer_mut() {
                        stage.learn_definition(&shown, &definition);
                    }
                    let mode = definition.modes.first().map(|mode| mode.name.clone());
                    let width = luma_lib::services::distribute::body_width_m(&definition);
                    // Millimetres in the file, metres in the room — the same
                    // fallback the renderer's `dimensions_m` uses, so the
                    // collision box is the drawn housing's box.
                    let body = definition
                        .physical
                        .as_ref()
                        .and_then(|p| p.dimensions.as_ref())
                        .map(|d| {
                            let axis = |v: f32| if v > 0.0 { f64::from(v) / 1000.0 } else { 0.3 };
                            [axis(d.width), axis(d.height), axis(d.depth)]
                        });
                    if let Some(build) = this.build_mut() {
                        build.set_fixture_facts(mode, width, body);
                        build.repreview();
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }
}

/// How big the add-element card is. Fixed, for the reason every palette in the
/// app is: a card that resized as the query narrowed would move the row under
/// the pointer between one keystroke and the next.
const CHOOSER_SIZE: luma_ui::dialog::morph::MorphSize = luma_ui::dialog::morph::MorphSize {
    width: 640.0,
    height: 420.0,
};

/// One row the dialog can offer, and what taking it puts in the hand.
pub(crate) struct ChooserRow {
    pub(crate) label: String,
    pub(crate) section: String,
    /// What the preview says about it beyond its name: for a library
    /// fixture, how well it is checked and the modes it can be patched in.
    pub(crate) detail: Option<String>,
    pub(crate) take: Holding,
}

/// What the dialog offers for its query: the fixtures a venue can be given, or
/// the catalog's pieces.
///
/// Already narrowed, because the two narrow differently and only this function
/// knows which is which: the catalog is in memory and filtered here, while the
/// library's rows *are* the answer to the query —
/// [`Library::search_fixtures`] matched them, and re-filtering the page it
/// returned would drop rows it matched on a field the label does not carry.
pub(crate) fn chooser_rows(library: &FixtureLibrary, offer: Offer) -> Vec<ChooserRow> {
    match offer {
        Offer::Fixtures => library
            .entries()
            .iter()
            .map(|entry| {
                let label = format!("{} {}", entry.manufacturer, entry.model);
                ChooserRow {
                    label: label.clone(),
                    section: entry.manufacturer.clone(),
                    detail: entry.library.as_ref().map(library_detail),
                    take: Holding::Fixture {
                        path: entry.path.clone(),
                        label,
                        mode: None,
                        width_m: DEFAULT_FIXTURE_WIDTH_M,
                        dims: None,
                        moving: None,
                    },
                }
            })
            .collect(),
        Offer::Pieces => {
            let needle = library.query().trim().to_lowercase();
            let matches = |label: &str, section: &str| {
                needle.is_empty()
                    || label.to_lowercase().contains(&needle)
                    || section.to_lowercase().contains(&needle)
            };
            palette_rows()
                .into_iter()
                .filter(|row| matches(&row.label, row.group.as_str()))
                .map(|row| ChooserRow {
                    label: row.label.clone(),
                    section: row.group.as_str().to_string(),
                    detail: None,
                    take: Holding::Piece {
                        catalog_ref: row.catalog_ref.to_string(),
                        kind: NodeKind::Piece,
                        display_name: row.label,
                        footing: hand::footing_for(row.catalog_ref, row.tower),
                        params: BTreeMap::new(),
                    },
                })
                .collect()
        }
    }
}

/// A library fixture's line in the preview: `Fixture library · geometry
/// verified · 3ch, 4ch, 14ch`.
fn library_detail(entry: &luma_lib::fixtures::library::Entry) -> String {
    let modes: Vec<String> = entry
        .modes
        .iter()
        .map(|mode| {
            if mode.name == format!("{}ch", mode.footprint) {
                mode.name.clone()
            } else {
                format!("{} ({}ch)", mode.name, mode.footprint)
            }
        })
        .collect();
    format!(
        "Fixture library · geometry {} · {}",
        entry.quality.geometry,
        modes.join(", ")
    )
}

/// The width a fixture is assumed to be until its definition lands. The same
/// fallback `luma_lib`'s `body_width_m` uses, so an un-fetched preview and a
/// fetched one differ only where the definition actually says something.
const DEFAULT_FIXTURE_WIDTH_M: f64 = 0.3;

/// What the row under the keyboard is, at a size worth looking at.
///
/// A pane and not a tooltip, because choosing between two trusses is a
/// comparison and a comparison wants one place the answer always appears.
fn chooser_preview(
    row: Option<&ChooserRow>,
    turn: i32,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let pane = div()
        .w(px(CHOOSER_PREVIEW_W))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(CHOOSER_PREVIEW_PAD));
    let Some(row) = row else {
        return pane.child(float::empty_row("Nothing highlighted"));
    };
    let detail = match &row.take {
        Holding::Piece { footing, .. } => footing.map_or_else(
            || "Seats on its own underside".to_string(),
            |footing| format!("Stands on {footing}"),
        ),
        // The dialog never offers one — a duplicate comes off a selection.
        Holding::Duplicate { .. } => "A copy of what is selected".to_string(),
        Holding::Fixture { .. } => row
            .detail
            .clone()
            .unwrap_or_else(|| "Placed as a row along a face".to_string()),
    };
    let subject = match &row.take {
        Holding::Piece { catalog_ref, .. } => Some(preview::Subject::Piece(catalog_ref.clone())),
        Holding::Fixture { path, .. } => Some(preview::Subject::Fixture(path.clone())),
        Holding::Duplicate { .. } => None,
    };
    let image = subject.and_then(|subject| {
        let view = preview::View::of(subject.clone(), turn);
        match preview::thumbnail(&view) {
            preview::Thumbnail::Ready(path) => {
                *last_shown().lock().unwrap() = Some((subject, path.clone()));
                Some(path)
            }
            preview::Thumbnail::Rendering => {
                // On its way: look again next frame, and until then keep the
                // angle this subject was last seen from.
                window.request_animation_frame();
                last_shown()
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|(shown, _)| *shown == subject)
                    .map(|(_, path)| path.clone())
            }
            preview::Thumbnail::Unavailable => None,
        }
    });
    let grab = app.clone();
    // A box of the picture's own shape whether or not the picture has
    // landed, so the words under it never move.
    let frame = div()
        .id("stage-chooser-preview")
        .w_full()
        .h(px(
            (CHOOSER_PREVIEW_W - 2.0 * CHOOSER_PREVIEW_PAD) * PREVIEW_ASPECT
        ))
        .flex_none()
        .cursor_grab()
        .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
            grab.update(cx, |this, cx| this.stage_grab_preview(event.position.x, cx));
        })
        .children(image.map(|path| gpui::img(path).size_full().rounded(px(6.0))))
        .agent_node(Role::Card, format!("Preview picture at {turn}"));
    pane.child(frame)
        .child(float::label(row.section.to_string()))
        .child(
            div()
                .text_size(px(15.0))
                .text_color(ladder::foreground())
                .child(row.label.clone())
                .agent_node(Role::Text, format!("Preview {}", row.label)),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.5))
                .child(detail),
        )
}

/// The picture the preview last showed, and of what: what stays up while the
/// same subject's next angle renders.
fn last_shown() -> &'static std::sync::Mutex<Option<(preview::Subject, std::path::PathBuf)>> {
    static SHOWN: std::sync::OnceLock<
        std::sync::Mutex<Option<(preview::Subject, std::path::PathBuf)>>,
    > = std::sync::OnceLock::new();
    SHOWN.get_or_init(Default::default)
}

/// The preview pane's padding.
const CHOOSER_PREVIEW_PAD: f32 = 16.0;
/// The thumbnails' height over their width.
const PREVIEW_ASPECT: f32 = 0.75;
/// How far the pointer travels to turn the preview one step.
const PREVIEW_TURN_PX: f32 = 6.0;

/// How wide the dialog's preview pane is — a third of the card, so the list
/// beside it still shows a full row without truncating.
const CHOOSER_PREVIEW_W: f32 = 208.0;

/// The add-element dialog: a search field, a preview, a sectioned list.
pub(super) fn chooser(
    state: &StagePage,
    (offer, cursor, turn): (Offer, usize, i32),
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let rows = chooser_rows(&state.library, offer);
    let dismiss = app.clone();

    let mut list = float::list().id("stage-chooser-list").overflow_y_scroll();
    let mut section = "";
    for (index, row) in rows.iter().enumerate() {
        if row.section != section {
            section = &row.section;
            list = list.child(float::section_heading(section.to_string()));
        }
        let app = app.clone();
        let take = row.take.clone();
        let label = row.label.clone();
        list = list.child(
            float::menu_row(RowState::of(false, index == cursor), label.clone())
                .id(gpui::SharedString::from(format!("choose:{index}")))
                .child(label.clone())
                .on_click(move |_, window, cx| {
                    let take = take.clone();
                    // The field held focus; the dialog it lived in is gone.
                    // Focus goes back to the tab, where the stage's own keys
                    // (W/E/A/F, escape) route from — a dangling handle would
                    // eat them all, and bare blur leaves them just as dead.
                    let focus = app.update(cx, |this, cx| {
                        this.stage_take(take, cx);
                        this.focus.clone()
                    });
                    window.focus(&focus, cx);
                })
                .agent_node(Role::Row, label),
        );
    }
    if rows.is_empty() {
        list = list.child(float::empty_row("Nothing matches"));
    }

    let keys = app.clone();
    let taken: Vec<Holding> = rows.iter().map(|row| row.take.clone()).collect();
    let body = div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground())
        // The preview is turned from the whole card: a drag that starts on
        // the picture does not end at its edge.
        .on_mouse_move({
            let turn = app.clone();
            move |event, _, cx| {
                if event.pressed_button == Some(gpui::MouseButton::Left) {
                    turn.update(cx, |this, cx| this.stage_turn_preview(event.position.x, cx));
                }
            }
        })
        .on_mouse_up(gpui::MouseButton::Left, {
            let release = app.clone();
            move |_, _, cx| release.update(cx, |this, cx| this.stage_release_preview(cx))
        })
        // The list is walked from the frame, not from the field: the query is
        // short and edited with backspace, so the arrows belong to the rows —
        // the same call `luma_ui::float`'s pickers make.
        .on_key_down(move |event, window, cx| {
            let step: isize = match event.keystroke.key.as_str() {
                "down" => 1,
                "up" => -1,
                "tab" => {
                    let other = match offer {
                        Offer::Fixtures => Offer::Pieces,
                        Offer::Pieces => Offer::Fixtures,
                    };
                    keys.update(cx, |this, cx| this.stage_offer(other, cx));
                    cx.stop_propagation();
                    return;
                }
                // Handled here as well as at the shell's rung, because the
                // search field holds focus and a field taking typed text
                // out-ranks the window's own Escape.
                "escape" => {
                    let focus = keys.update(cx, |this, cx| {
                        this.stage_escape(cx);
                        this.focus.clone()
                    });
                    window.focus(&focus, cx);
                    return;
                }
                "enter" => {
                    let take = taken.get(cursor).cloned();
                    let focus = keys.update(cx, |this, cx| {
                        if let Some(take) = take {
                            this.stage_take(take, cx);
                        }
                        this.focus.clone()
                    });
                    window.focus(&focus, cx);
                    return;
                }
                _ => return,
            };
            let len = taken.len();
            keys.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    if let Hand::Choosing(choosing) = &mut build.hand {
                        // Wraps at both ends, so a list is a ring and neither
                        // end is a dead key.
                        choosing.cursor = if len == 0 {
                            0
                        } else {
                            (choosing.cursor as isize + step).rem_euclid(len as isize) as usize
                        };
                    }
                }
                cx.notify();
            });
        })
        .child(
            float::header_band()
                .child(
                    float::segmented()
                        .flex_none()
                        .children(Offer::ALL.map(|other| {
                            let offered = app.clone();
                            float::segment(other.label(), other == offer, other.label())
                                .id(other.label())
                                .on_click(move |_, _, cx| {
                                    offered.update(cx, |this, cx| this.stage_offer(other, cx));
                                })
                                .agent_node(
                                    Role::Toggle,
                                    format!("Add {}", other.label().to_lowercase()),
                                )
                                .agent_focused(other == offer)
                        })),
                )
                .child(fixture_library::search_field(&state.library, true, true))
                .child({
                    let close = app.clone();
                    float::key_cap_pressable(float::key_cap())
                        .id("stage-chooser-close")
                        .on_click(move |_, window, cx| {
                            let focus = close.update(cx, |this, cx| {
                                this.stage_escape(cx);
                                this.focus.clone()
                            });
                            window.focus(&focus, cx);
                        })
                        .child("esc")
                        .agent_node(Role::Button, "Close add element")
                }),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(chooser_preview(rows.get(cursor), turn, app, window))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .border_l_1()
                        .border_color(luma_ui::glass::hairline(0.06))
                        .child(float::viewport().child(list)),
                ),
        )
        .child(
            float::footer_band()
                .child(float::key_hint_pair(
                    IconName::ArrowUp,
                    IconName::ArrowDown,
                    "Navigate",
                ))
                .child(float::key_hint_text("tab", "Fixtures or pieces"))
                .child(float::key_hint_text("enter", "Take"))
                .child(float::key_hint_text("esc", "Close")),
        )
        .into_any_element();

    let host = luma_ui::dialog::Host {
        id: "stage-chooser".into(),
        viewport: window.viewport_size(),
        focus: &state.focus,
        focused: true,
        label: "Add element dialog".into(),
        scrim_dismiss: luma_ui::dialog::ScrimDismiss::Enabled(Box::new(move |window, cx| {
            let focus = dismiss.update(cx, |this, cx| {
                this.stage_escape(cx);
                this.focus.clone()
            });
            window.focus(&focus, cx);
        })),
        closing: state.closing.closing_since(),
    }
    .render(luma_ui::dialog::morph::fixed_card(
        "Add element card",
        CHOOSER_SIZE,
        body,
    ));
    // Anchored to the *window*, not to the page: the stage tab lives inside
    // the room's own box, and a card centred in that box sits off the
    // screen's centre. The host is already sized to the whole viewport, so
    // pinning its top-left to the window's is what centres it for real.
    gpui::deferred(
        gpui::anchored()
            .position(gpui::point(px(0.0), px(0.0)))
            .anchor(gpui::Anchor::TopLeft)
            .child(host),
    )
    .priority(1)
    .into_any_element()
}
