//! The stage tab body: [`StagePage`], the floating controls over the room,
//! the objects list and the node menu, and the verbs that select and escape.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, Pixels, Point, Window};
use luma_scene::venue::NodeKind;
use luma_ui::float::{self, Dismiss};
use luma_ui::ladder;
use luma_ui::motion::FadeHover;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::fixture_library::FixtureLibrary;
use crate::Luma;

use super::chooser::chooser;
use super::hand::{Hand, Holding};
use super::row::{configure, ConfigureView};

/// The stage tab. Almost stateless: what a builder is doing lives on the
/// [`Build`] beside the picture, and this is the venue the tab dies with.
pub(crate) struct StagePage {
    /// The one search the add-element dialog has, and the fixture rows it
    /// fetched. The shared picker rather than a private query: the bundle is
    /// fifteen thousand definitions, so "what fixtures match this" is paged,
    /// generation-guarded and error-reporting wherever it is asked, and the
    /// stage page asking it a second way was the duplicate.
    ///
    /// Its field is the dialog's *only* field: [`FixtureLibrary::query`] is
    /// what the catalog section is filtered by too, so one string narrows
    /// both provenances.
    pub(crate) library: FixtureLibrary,
    /// The focus the dialog traps while it is up.
    pub(crate) focus: gpui::FocusHandle,
    /// Where the node menu is open, if it is — and which bead opened it, so
    /// the menu can offer that socket's own verbs. On the page and not the
    /// hand: a menu is chrome about the selection, not a thing the hand is
    /// doing.
    pub(crate) menu: Option<NodeMenuAt>,
    pub(crate) objects_open: bool,
    /// The add-element dialog's exit. The hand decides *whether* the dialog is
    /// up ([`Hand::Choosing`]); this only buys the card its leaving frames —
    /// see [`luma_ui::dialog::Popup`].
    pub(crate) closing: luma_ui::dialog::Popup<()>,
}

impl StagePage {
    pub(crate) fn new(cx: &mut Context<Luma>) -> Self {
        Self {
            library: FixtureLibrary::new("Search elements", cx, |luma, query, cx| {
                luma.stage_fixture_query(query, cx);
            }),
            focus: cx.focus_handle(),
            menu: None,
            objects_open: false,
            closing: luma_ui::dialog::Popup::default(),
        }
    }
}

impl Luma {
    /// The stage tab the dialog belongs to — the active one, because the hand
    /// it arms is the one beside the picture and there is only ever one of
    /// those.
    /// The stage page wherever it lives, for the shell's per-frame popup
    /// reap — the exit must finish even if another tab has focus.
    pub(crate) fn stage_page_for_tick(&mut self) -> Option<&mut StagePage> {
        self.stage_page_mut()
    }

    pub(super) fn stage_page_mut(&mut self) -> Option<&mut StagePage> {
        match self.workspace.active_body_mut()? {
            crate::shell::Body::Patch(page) => Some(&mut page.stage),
            _ => None,
        }
    }

    /// Open the node menu where the pointer is.
    pub(crate) fn stage_open_menu(
        &mut self,
        at: Point<Pixels>,
        node: String,
        socket: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(page) = self.stage_page_mut() {
            page.menu = Some(NodeMenuAt { at, node, socket });
        }
        cx.notify();
    }

    /// Close the node menu.
    pub(crate) fn stage_close_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = self.stage_page_mut() {
            page.menu = None;
        }
        cx.notify();
    }

    /// `Escape`: one rung down the flow, then the selection.
    /// Select whatever placed piece is under `at` — the headless twin of the
    /// viewport click, writing the same two stores the same way.
    pub(crate) fn stage_pick_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        let Some(ray) = state.stage_pick_ray(at) else {
            return;
        };
        let origin = luma_scene::coords::three_from_world(ray.origin).as_dvec3();
        let dir = luma_scene::coords::three_from_world(ray.dir)
            .as_dvec3()
            .normalize_or_zero();
        let picked = state
            .build
            .as_ref()
            .and_then(|build| build.room_pick(origin, dir));
        match picked.clone() {
            Some(id) => state.select_object(luma_render::frame::EditorObject::StagePiece(id)),
            None => state.clear_selection(),
        }
        if let Some(build) = state.build.as_mut() {
            build.select(picked);
        }
        cx.notify();
    }

    /// The element list and the viewport select the same authored object.
    pub(crate) fn stage_select_element(&mut self, id: String, cx: &mut Context<Self>) {
        self.stage_escape(cx);
        if let Some(crate::shell::Body::Patch(page)) = self.workspace.active_body_mut() {
            page.selected.clear();
            page.anchor = None;
            page.stage.objects_open = false;
        }
        if let Some(state) = self.visualizer_mut() {
            state.replace_selection([luma_render::frame::EditorObject::StagePiece(id.clone())]);
            if let Some(build) = state.build.as_mut() {
                build.select(Some(id));
            }
        }
        cx.notify();
    }

    pub(crate) fn stage_escape(&mut self, cx: &mut Context<Self>) {
        self.stage_close_menu(cx);
        if let Some(crate::shell::Body::Patch(page)) = self.workspace.active_body_mut() {
            page.selected.clear();
        }
        let mut was_choosing = false;
        if let Some(build) = self.build_mut() {
            was_choosing = matches!(build.hand, Hand::Choosing(_));
            build.hand = std::mem::take(&mut build.hand).escape();
            // Escape empties the hand *and* the head: whatever mode it left,
            // nothing stays selected — coming out of place mode with the last
            // selection still lit read as the editor holding a grudge.
            build.select(None);
        }
        if let Some(state) = self.visualizer_mut() {
            state.clear_selection();
        }
        if was_choosing {
            if let Some(page) = self.stage_page_mut() {
                page.closing.begin_close(cx);
            }
        }
        cx.notify();
    }
}

/// The builder, projected into the shapes the tab body draws.
///
/// Owned rather than borrowed because the tab body is rendered inside a
/// mutable borrow of the workspace, and the builder lives beside the picture.
/// Every field is also a *claim* the harness can read — see the module docs.
pub(crate) struct StageView {
    pub(crate) elements: Vec<(String, String, String)>,
    pub(crate) selected: Option<String>,
    /// The add-element dialog's keyboard cursor, when it is up.
    pub(crate) choosing: Option<usize>,
    pub(crate) configuring: Option<ConfigureView>,
}

impl Luma {
    /// The builder as the page draws it, or `None` when no room is up.
    pub(crate) fn stage_view(&self) -> Option<StageView> {
        let build = self.build_state()?;
        Some(StageView {
            elements: build
                .graph
                .nodes()
                .filter(|node| !matches!(node.kind, NodeKind::Venue | NodeKind::Fixture))
                .map(|node| {
                    (
                        node.id.clone(),
                        build.label_of(&node.id),
                        build
                            .graph
                            .edge(&node.id)
                            .map_or_else(String::new, |edge| build.label_of(&edge.parent)),
                    )
                })
                .collect(),
            selected: build.selected.clone(),
            choosing: build.hand.choosing().map(|c| c.cursor),
            configuring: build.hand.configuring().map(|row| ConfigureView {
                host: format!("{} {}", row.host_label, row.host_socket),
                what: row.what.label().to_string(),
                at: row.at,
                count: row.count,
                even: matches!(row.layout, luma_scene::distribute::Layout::Even),
                fits: match &row.preview {
                    Ok(stations) => Ok(stations.len()),
                    Err(luma_scene::distribute::Fit::TooLong {
                        needed_m,
                        available_m,
                    }) => Err(format!(
                        "Needs {needed_m:.2} m; the face is {available_m:.2} m"
                    )),
                },
                // Named off the length it would set, not off a refusal's prose:
                // the button says what pressing it does, and the line above it
                // already says why it is there.
                offer: match &row.preview {
                    Err(luma_scene::distribute::Fit::TooLong { needed_m, .. }) => {
                        Some(format!("Extend to {needed_m:.2} m"))
                    }
                    Ok(_) => None,
                },
                ready: row.preview.is_ok()
                    && matches!(&row.what, Holding::Fixture { mode, .. } if mode.is_some()),
            }),
        })
    }
}

/// The venue's tools live on the picture, alongside its render settings.
pub(crate) fn controls(
    state: &StagePage,
    app: &Entity<Luma>,
    view: Option<&StageView>,
    window: &Window,
) -> AnyElement {
    let Some(view) = view else {
        return div().into_any_element();
    };
    let toggle = app.clone();
    let close = app.clone();
    let objects = div()
        .relative()
        .child(
            float::btn("Objects", "stage-objects")
                .id("stage-objects")
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |this, cx| {
                        if let Some(page) = this.stage_page_mut() {
                            page.objects_open = !page.objects_open;
                        }
                        cx.notify();
                    })
                })
                .agent_node(Role::Toggle, "Stage objects")
                .agent_focused(state.objects_open),
        )
        .when(state.objects_open, |d| {
            d.child(float::anchored_above(
                "stage-objects-popover",
                luma_ui::CONTROL_HEIGHT,
                Dismiss::on_press_out(move |_, cx| {
                    close.update(cx, |this, cx| {
                        if let Some(page) = this.stage_page_mut() {
                            page.objects_open = false;
                        }
                        cx.notify();
                    })
                }),
                float::popover_card()
                    .w(px(300.0))
                    .p(px(12.0))
                    .child(
                        div()
                            .id("stage-object-list")
                            .max_h(px(320.0))
                            .overflow_y_scroll()
                            .child(elements(view, app)),
                    )
                    .agent_node(Role::Card, "Stage objects")
                    .into_any_element(),
            ))
        });
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(add_button(app))
        .child(objects)
        .children(
            (view.choosing.is_some() || state.closing.is_closing())
                .then(|| chooser(state, view.choosing.unwrap_or(0), app, window)),
        )
        .children(view.configuring.as_ref().map(|row| configure(row, app)))
        .children(state.menu.as_ref().map(|menu| node_menu(menu, app)))
        .into_any_element()
}

/// Opens the shared structure and fixture palette.
fn add_button(app: &Entity<Luma>) -> AnyElement {
    let open = app.clone();
    let bar = div().flex().items_center().gap(px(6.0)).occlude().child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                float::btn("Add", "stage-add")
                    .id("stage-add")
                    .child(
                        gpui_component::Icon::new(luma_ui::icons::IconName::Plus)
                            .size(px(18.0))
                            .text_color(ladder::foreground_alpha(0.7)),
                    )
                    .on_click(move |_, window, cx| {
                        open.update(cx, |this, cx| this.stage_open_chooser_focused(window, cx));
                    })
                    .agent_node(Role::Button, "Add element"),
            )
            .child(float::key_hint_text("A", "")),
    );
    bar.into_any_element()
}

/// What a right-click on a placed node offers.
///
/// A menu and not a toolbar: these three act on *this* node, so they belong
/// where the node is. A bar at the top of the screen carrying "Flip" while the
/// thing it would flip is four hundred pixels away is a control that has lost
/// its subject.
/// The bead the node menu was opened from, and where.
pub(crate) struct NodeMenuAt {
    pub(crate) at: Point<Pixels>,
    pub(crate) node: String,
    pub(crate) socket: Option<String>,
}

fn node_menu(menu_at: &NodeMenuAt, app: &Entity<Luma>) -> AnyElement {
    let (dup, flip, delete, close) = (app.clone(), app.clone(), app.clone(), app.clone());
    let mut menu = luma_ui::menu::ContextMenu::new("stage-node-menu", menu_at.at);
    if let Some(socket) = menu_at.socket.clone() {
        let extend = app.clone();
        let node = menu_at.node.clone();
        menu = menu
            .item("Extend run", move |_, cx| {
                let (node, socket) = (node.clone(), socket.clone());
                extend.update(cx, |this, cx| this.stage_extend_from(node, socket, cx));
            })
            .separator();
    }
    menu.item("Duplicate", move |_, cx| {
        dup.update(cx, |this, cx| this.stage_duplicate(cx));
    })
    .item("Flip", move |_, cx| {
        flip.update(cx, |this, cx| this.stage_flip(cx));
    })
    .separator()
    .destructive("Delete", move |_, cx| {
        delete.update(cx, |this, cx| this.stage_delete(cx));
    })
    .render(move |_, cx| {
        close.update(cx, |this, cx| this.stage_close_menu(cx));
    })
}

fn elements(view: &StageView, app: &Entity<Luma>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(float::label("Elements"))
        .when(view.elements.is_empty(), |d| {
            d.child(float::empty_row("No stage elements"))
        })
        .children(view.elements.iter().map(|(id, label, parent)| {
            let click = app.clone();
            let id = id.clone();
            let selected = view.selected.as_ref() == Some(&id);
            div()
                .id(gpui::SharedString::from(format!("venue-element-{id}")))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(7.0))
                .rounded(px(4.0))
                .bg(if selected {
                    luma_ui::glass::card_selected_bg()
                } else {
                    luma_ui::glass::wash(0.0)
                })
                .when(!selected, |line| {
                    line.hover(|style| style.bg(luma_ui::glass::glass_hover()))
                        .fade_hover()
                })
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    click.update(cx, |this, cx| this.stage_select_element(id.clone(), cx))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .child(label.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(ladder::muted_foreground())
                        .child(parent.clone()),
                )
                .agent_node(Role::Row, format!("Element {label}"))
                .agent_focused(selected)
        }))
        .into_any_element()
}
