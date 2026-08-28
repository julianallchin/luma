//! Choosing fixtures by pointing at them.
//!
//! The strip's expression field is the power user's spelling of a selection.
//! This dialog is the other one: the room on the left, the venue's groups as a
//! column of checkboxes on the right, and the heads a tick would light going
//! white in the picture. An LD who has never typed `front_wash | back_movers`
//! writes exactly that by ticking two boxes.
//!
//! # What the picker can and cannot say
//!
//! A set of ticked rows is a **union**, and nothing else. So the picker reads
//! an expression back through [`or_terms`] — the resolver's own parser, not a
//! second one here — and an expression that uses `&`, `~`, `^` or `?` comes
//! back as `None`. That one is shown verbatim and read-only at the top, its
//! ticks left empty, and applying it *unchanged* leaves it exactly as it was
//! ([`FixturePicker::touched`]). Ticking a row is what says "replace this with
//! a union"; opening and closing the dialog is not.
//!
//! # Why the picture is pulled, not pumped
//!
//! A highlight does not move: [`luma_lib::stage_render::highlight_state`] opens
//! the matched heads white and leaves everything else dark, with no strobe, no
//! motion and no clock. So the preview is one still per selection, drawn
//! through [`Sequence`] — the process-wide offscreen renderer, on the same
//! `luma_render` device, the same frame builder and the same `View::Front`
//! camera fit the visualizer uses. The venue is installed once when the dialog
//! opens and lit repeatedly, which is what that type is for.
//!
//! The alternative — a second [`luma_render::AsyncViewport`] inside the card —
//! would buy nothing (there is no motion to pump) and cost a second presentation
//! chain, a second atlas identity, a second idle gate and a copy of the
//! visualizer's prepaint. One renderer, two ways of asking it for a frame, is
//! the seam that already exists.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Context, Corners, Entity, FocusHandle,
    FontWeight, Hitbox, HitboxBehavior, ImageId, KeyDownEvent, Pixels, RenderImage, SharedString,
    Window,
};
use luma_lib::models::node_graph::PatternArgDef;
use luma_lib::models::selection::{Selection, Subset};
use luma_lib::services::groups::{or_expression, or_terms};
use luma_lib::stage_render::{self, Continuity, Sequence};
use luma_scene::View;
use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::float::{self, RowState};
use luma_ui::ladder;
use luma_ui::node::{agent_paint_node, AgentNode as _, Instrument as _, Role};

use crate::shell::Overlay;
use crate::track_editor::SUBSETS;
use crate::Luma;

/// The card. Wide enough for a landscape frame beside a column of group names,
/// tall enough for a dozen rows without the list becoming the whole dialog.
const CARD_SIZE: MorphSize = MorphSize::new(880.0, 540.0);
/// The frame's box inside the card. The renderer is asked for exactly
/// [`PREVIEW_PIXELS`] and the result is painted into this, so a retina and a
/// non-retina machine get the same picture and the same render cost.
///
/// The height is the card minus its two bands, taken from the bands
/// themselves — a number written here would be a second opinion about how tall
/// a footer is, and the footer's height is a consequence of its padding.
const PREVIEW_W: f32 = 520.0;
const PREVIEW_H: f32 = CARD_SIZE.height - float::HEADER_HEIGHT - float::FOOTER_HEIGHT;
/// What the offscreen renderer is asked for, in pixels: the box at 2x.
///
/// Fixed rather than derived from the window's scale factor — every distinct
/// size costs the renderer a full reallocation, and a preview that reallocated
/// when the dialog moved between displays would pay for a picture nobody asked
/// to change. Derived from the box rather than written out so the camera's fit,
/// which is a function of the aspect ratio, cannot disagree with the box the
/// frame is painted into.
const PREVIEW_PIXELS: (u32, u32) = ((PREVIEW_W * 2.0) as u32, (PREVIEW_H * 2.0) as u32);
/// A group row.
const ROW_HEIGHT: f32 = 26.0;

/// The atlas identity every preview frame is published under.
///
/// One identity for the life of the process, for the reason
/// `visualizer::STAGE_IMAGE_ID` is: the tile is refreshed in place
/// ([`Window::update_image`]) rather than reinserted, so a fresh id per frame
/// would create and strand a texture on every hover.
static PREVIEW_IMAGE_ID: std::sync::OnceLock<ImageId> = std::sync::OnceLock::new();

/// The dialog's state.
pub(crate) struct FixturePicker {
    /// The arg being edited, which is what [`Luma::arg_selection`] writes back
    /// through. Held whole because the write path wants the def, not just its id.
    def: PatternArgDef,
    venue_id: String,
    /// Every group in the venue, in venue order — the rows, and the only names
    /// a tick can produce.
    groups: Vec<SharedString>,
    /// Ticked rows, in tick order, because that is the order the expression is
    /// written in.
    checked: Vec<SharedString>,
    /// The expression the picker could not read back as a union, kept verbatim.
    /// `Some` puts the card in read-only-expression mode until a row is ticked.
    opaque: Option<SharedString>,
    /// Whether the person changed anything. An untouched opaque expression is
    /// applied as itself — closing a dialog must not rewrite an expression
    /// nobody edited.
    touched: bool,
    subset: Subset,
    /// The row under the pointer. The picture previews that group *alone*, so
    /// the answer to "what is this one?" costs no clicks.
    hovered: Option<SharedString>,
    subset_open: bool,
    preview: Preview,
    apply_focus: FocusHandle,
    cancel_focus: FocusHandle,
}

/// The picture, and the one request in flight for it.
#[derive(Default)]
struct Preview {
    /// The venue parked on the offscreen render thread. `None` until the rig
    /// lands; dropping it uninstalls.
    sequence: Option<Arc<Sequence>>,
    /// The frame on screen and the expression it pictures.
    shown: Option<(String, Arc<RenderImage>)>,
    /// The expression a frame is being drawn for. At most one at a time: a
    /// hover that arrives mid-render is picked up when this one lands, which
    /// is what keeps a sweep down the list from queueing twenty frames.
    inflight: Option<String>,
    /// Why there is no picture. Shown in place of one.
    error: Option<String>,
}

impl FixturePicker {
    /// The expression the picture should be showing: the hovered group alone,
    /// or the composed selection.
    fn wanted(&self) -> String {
        match &self.hovered {
            Some(group) => group.to_string(),
            None => self.expression(),
        }
    }

    /// What Apply would write. An untouched opaque expression is itself.
    fn expression(&self) -> String {
        match (&self.opaque, self.touched) {
            (Some(raw), false) => raw.to_string(),
            _ => or_expression(
                &self
                    .checked
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            ),
        }
    }

    fn is_checked(&self, group: &SharedString) -> bool {
        self.checked.contains(group)
    }

    fn toggle(&mut self, group: &SharedString) {
        self.touched = true;
        if let Some(at) = self.checked.iter().position(|name| name == group) {
            self.checked.remove(at);
        } else {
            self.checked.push(group.clone());
        }
    }
}

impl Luma {
    /// Open the picker on one selection arg.
    ///
    /// `groups` and `selection` are read out by the caller because it already
    /// holds the track editor — the strip knows which arg is being edited and
    /// what it currently says; the dialog knows nothing about clips.
    pub(crate) fn open_fixture_picker(
        &mut self,
        def: PatternArgDef,
        venue_id: String,
        groups: Vec<SharedString>,
        selection: &Selection,
        cx: &mut Context<Self>,
    ) {
        let terms = or_terms(&selection.expression);
        let state = FixturePicker {
            def,
            venue_id: venue_id.clone(),
            // Only names the venue actually has can be ticked; a term naming a
            // group that was renamed away is still shown in the raw expression
            // line, but there is no row to tick for it.
            checked: terms
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(SharedString::from)
                .filter(|name| groups.contains(name))
                .collect(),
            opaque: terms
                .is_none()
                .then(|| SharedString::from(selection.expression.clone())),
            groups,
            touched: false,
            subset: selection.subset,
            hovered: None,
            subset_open: false,
            preview: Preview::default(),
            apply_focus: cx.focus_handle().tab_stop(true),
            cancel_focus: cx.focus_handle().tab_stop(true),
        };
        self.overlay.open(Overlay::FixturePicker(Box::new(state)));
        self.install_preview_scene(venue_id, cx);
        cx.notify();
    }

    /// Load the venue's geometry and park it on the offscreen renderer.
    ///
    /// Editor-lit, because a highlight is a work light on a dark rig: the same
    /// combination `venue.render` uses for a highlight, and the same one the
    /// visualizer picks for a venue with no score composited onto it.
    fn install_preview_scene(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let rig = self.library.venue_rig(&venue_id);
        cx.spawn(async move |this, cx| {
            let loaded = rig.await;
            let installed = match loaded {
                Err(error) => Err(error.to_string()),
                Ok(rig) if rig.fixtures.is_empty() && rig.pieces.is_empty() => {
                    Err("this venue has nothing patched".to_string())
                }
                Ok(rig) => {
                    let definitions: BTreeMap<_, _> = rig
                        .definitions
                        .iter()
                        .map(|(path, def)| (path.clone(), stage_render::definition(def)))
                        .collect();
                    let mut scene = crate::visualizer::scene(&rig, &definitions);
                    scene.render = luma_render::scene_desc::RenderSettings::editor_lit(
                        crate::visualizer::FOV_Y_DEG,
                        luma_render::LIVE_HAZE_RESOLUTION,
                    );
                    Sequence::install(
                        scene,
                        definitions,
                        stage_render::meshes_root(None),
                        View::Front,
                        None,
                        PREVIEW_PIXELS,
                    )
                    .map(Arc::new)
                }
            };
            this.update(cx, |this, cx| {
                let Some(Overlay::FixturePicker(state)) = this.overlay.open_mut() else {
                    return;
                };
                if state.venue_id != venue_id {
                    return;
                }
                match installed {
                    Ok(sequence) => state.preview.sequence = Some(sequence),
                    Err(error) => state.preview.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Draw the selection the picker is currently pointing at, if the picture
    /// is not already it and nothing else is being drawn.
    fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::FixturePicker(state)) = self.overlay.open_mut() else {
            return;
        };
        let wanted = state.wanted();
        if state.preview.inflight.is_some()
            || state
                .preview
                .shown
                .as_ref()
                .is_some_and(|(shown, _)| shown == &wanted)
        {
            return;
        }
        let Some(sequence) = state.preview.sequence.clone() else {
            return;
        };
        let sequence_size = sequence.size();
        let venue = state.venue_id.clone();
        let selection = Selection::new(wanted.clone()).with_subset(state.subset);
        state.preview.inflight = Some(wanted.clone());
        // The call is minted here, not inside the spawn: `Library` owns a
        // runtime and is not `Clone`, so what crosses the await is its future.
        let pending = self.library.highlight_selection(&venue, &selection);
        cx.spawn(async move |this, cx| {
            let frame = match pending.await {
                Err(error) => Err(error.to_string()),
                Ok(state) => {
                    // `Sequence::frame` blocks on the offscreen render
                    // thread's reply, so it may not run on the UI thread.
                    cx.background_executor()
                        .spawn(async move {
                            sequence.frame(
                                Some(&state),
                                0.0,
                                luma_render::DEFAULT_SUBFRAMES,
                                Continuity::Cut,
                            )
                        })
                        .await
                        .and_then(|rgba| {
                            let (width, height) = sequence_size;
                            image_from_rgba(rgba, width, height)
                        })
                        .map(Arc::new)
                }
            };
            this.update(cx, |this, cx| {
                let Some(Overlay::FixturePicker(state)) = this.overlay.open_mut() else {
                    return;
                };
                state.preview.inflight = None;
                match frame {
                    Ok(image) => {
                        state.preview.error = None;
                        state.preview.shown = Some((wanted, image));
                    }
                    Err(error) => state.preview.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Write the picker's answer onto the clip and close.
    fn apply_fixture_picker(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::FixturePicker(state)) = self.overlay.as_open() else {
            return;
        };
        let (def, expression, subset) = (state.def.clone(), state.expression(), state.subset);
        self.arg_selection(&def.id.clone(), &def, cx, |selection| {
            selection.expression = expression;
            selection.subset = subset;
        });
        self.close_overlay(cx);
    }

    pub(crate) fn fixture_picker_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if !matches!(self.overlay.as_open(), Some(Overlay::FixturePicker(_))) {
            return;
        }
        // Escape is deliberately absent: the shell's own binding reaches
        // `dismiss_overlay`, and two handlers on one key would close twice.
        if event.keystroke.key == "enter" {
            self.apply_fixture_picker(cx);
        }
    }
}

/// Wrap a readback as the image gpui paints.
///
/// The offscreen renderer writes RGBA — the order a PNG wants — and
/// `RenderImage` reads its buffer as **BGRA**, so the two channels are
/// exchanged here. Swapping in place rather than through the renderer because
/// the byte order is this consumer's business and every other caller of
/// `Sequence` is encoding a PNG.
fn image_from_rgba(mut rgba: Vec<u8>, width: u32, height: u32) -> Result<RenderImage, String> {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "the frame was not width * height * 4 bytes".to_string())?;
    let mut image = RenderImage::new([image::Frame::new(buffer)]);
    match PREVIEW_IMAGE_ID.get() {
        Some(id) => image.id = *id,
        None => {
            let _ = PREVIEW_IMAGE_ID.set(image.id);
        }
    }
    Ok(image)
}

/// Keep the picture in step with what the pointer is on. Runs every frame the
/// shell draws, like every other overlay's tick.
pub(crate) fn tick(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) {
    app.refresh_preview(cx);
    let Some(Overlay::FixturePicker(state)) = app.overlay.open_mut() else {
        return;
    };
    if state.apply_focus.is_focused(window) || state.cancel_focus.is_focused(window) {
        return;
    }
    let wanted = state.apply_focus.clone();
    window.focus(&wanted, cx);
}

pub(crate) fn render(
    state: &FixturePicker,
    app: &Entity<Luma>,
    window: &Window,
    _cx: &mut gpui::App,
) -> AnyElement {
    morph::fixed_card("Fixture picker dialog", CARD_SIZE, body(state, app, window))
}

fn body(state: &FixturePicker, app: &Entity<Luma>, window: &Window) -> AnyElement {
    let keys = app.clone();
    // No `track_focus`: the dialog host already owns this card's focus trap.
    div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground())
        .on_key_down(move |event, _, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.fixture_picker_key(&event, cx));
        })
        .child(header(state, app, window))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(preview(state))
                .child(rows(state, app, window)),
        )
        .child(footer(state))
        .into_any_element()
}

/// Title, the note about an unreadable expression, and the card's two actions.
///
/// The actions live *here*, not in the footer, because that is where every
/// other dialog in the app keeps them (`welcome`, `add_tracks`): the primary
/// is a [`float::btn_primary_chip`] carrying the chord the footer legend
/// promises, and the dismissal is the `esc` [`float::key_cap`] at the trailing
/// edge. The footer is a legend.
fn header(state: &FixturePicker, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    let title: SharedString = "Fixtures".into();
    let applied = app.clone();
    let cancelled = app.clone();
    float::header_band()
        .child(
            div()
                .flex_none()
                .text_size(px(14.0))
                .font_weight(FontWeight::MEDIUM)
                .child(title.clone())
                .agent_node(Role::Text, title),
        )
        .child(div().flex_1().min_w_0())
        // An expression the ticks cannot spell is shown rather than hidden: it
        // is what the clip currently does, and applying without touching a row
        // leaves it alone.
        .when_some(state.opaque.clone(), |band, raw| {
            let note: SharedString = format!("{raw} — tick a group to replace it").into();
            band.child(
                div()
                    .max_w(px(360.0))
                    .truncate()
                    .text_size(px(11.0))
                    .text_color(ladder::muted_foreground())
                    .child(note.clone())
                    .agent_node(Role::Text, note),
            )
        })
        .child(
            float::btn_primary_chip()
                .id("fixture-picker-apply")
                .track_focus(&state.apply_focus)
                .tab_index(0)
                .on_click(move |_, _, cx| {
                    applied.update(cx, |this, cx| this.apply_fixture_picker(cx));
                })
                // The glyph must name the chord the footer legend promises.
                .child("↵")
                .child("Apply")
                .agent_node(Role::Button, "Apply")
                .agent_focused(state.apply_focus.is_focused(window)),
        )
        .child(
            float::key_cap_pressable(float::key_cap())
                .id("fixture-picker-cancel")
                .track_focus(&state.cancel_focus)
                .tab_index(0)
                .on_click(move |_, _, cx| {
                    cancelled.update(cx, |this, cx| this.dismiss_overlay(cx));
                })
                .child("esc")
                .agent_node(Role::Button, "Cancel")
                .agent_focused(state.cancel_focus.is_focused(window)),
        )
}

/// The room, lit by whatever the picker is pointing at.
fn preview(state: &FixturePicker) -> impl IntoElement {
    let body: AnyElement = match (&state.preview.shown, &state.preview.error) {
        (Some((_, image)), _) => {
            let image = Arc::clone(image);
            canvas(
                move |bounds: Bounds<Pixels>, window, cx| {
                    agent_paint_node(Role::Card, "Selection preview", bounds, window, cx);
                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                },
                move |bounds, _: Hitbox, window, _| {
                    // New pixels under a fixed identity, so the atlas has to be
                    // told — see `PREVIEW_IMAGE_ID`.
                    window.update_image(&image).ok();
                    window
                        .paint_image(
                            bounds,
                            bounds,
                            Corners::default(),
                            Arc::clone(&image),
                            0,
                            false,
                        )
                        .ok();
                },
            )
            .size_full()
            .into_any_element()
        }
        (None, Some(error)) => centered(error.clone()),
        (None, None) => centered("Lighting the room…".to_string()),
    };
    div()
        .flex_none()
        .w(px(PREVIEW_W))
        .h(px(PREVIEW_H))
        .bg(ladder::background())
        .child(body)
}

fn centered(message: String) -> AnyElement {
    let message: SharedString = message.into();
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .px(px(24.0))
        .text_size(px(12.0))
        .text_color(ladder::foreground_alpha(0.45))
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
}

/// One row per venue group.
fn rows(state: &FixturePicker, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    let list = if state.groups.is_empty() {
        let message = "This venue has no groups yet";
        float::list()
            .child(float::empty_row(message).agent_node(Role::Text, message))
            .into_any_element()
    } else {
        float::list()
            .id("fixture-picker-groups")
            .overflow_y_scroll()
            .children(state.groups.iter().map(|group| row(state, group, app)))
            .into_any_element()
    };
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(ladder::trim())
        .child(float::viewport().child(list))
        .child(float::divider())
        .child(how_many(state, app, window))
}

fn row(state: &FixturePicker, group: &SharedString, app: &Entity<Luma>) -> AnyElement {
    let checked = state.is_checked(group);
    let hovered = state.hovered.as_ref() == Some(group);
    let clicked = app.clone();
    let entered = app.clone();
    let name = group.clone();
    let hover_name = group.clone();
    // The hover listener sits on a wrapper: `float::menu_row` already spends
    // its element's one `on_hover` on the row's own fade, and gpui allows
    // exactly one per element.
    div()
        .id(gpui::ElementId::Name(format!("hover-{group}").into()))
        .w_full()
        .flex_none()
        // Hovering previews this group alone — the answer to "which ones are
        // these?" without spending a tick to ask it.
        .on_hover(move |over, _, cx| {
            let next = over.then(|| hover_name.clone());
            entered.update(cx, |this, cx| {
                if let Some(Overlay::FixturePicker(state)) = this.overlay.open_mut() {
                    if state.hovered != next {
                        state.hovered = next;
                        cx.notify();
                    }
                }
            });
        })
        .child(
            float::menu_row(RowState::of(checked, hovered), format!("group-{group}"))
                .id(gpui::ElementId::Name(group.clone()))
                .w_full()
                .h(px(ROW_HEIGHT))
                .px(px(10.0))
                .gap(px(10.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .child(group.clone()),
                )
                // The float tier's chosen-mark, the same one an open menu's
                // rows wear — a ticked row here is a chosen row there.
                .child(float::check(checked))
                .on_click(move |_, _, cx| {
                    let name = name.clone();
                    clicked.update(cx, |this, cx| {
                        if let Some(Overlay::FixturePicker(state)) = this.overlay.open_mut() {
                            state.toggle(&name);
                            cx.notify();
                        }
                    });
                })
                .agent_node(Role::Checkbox, group.clone()),
        )
        .into_any_element()
}

/// The subset ladder, spelled exactly as the strip's cell spells it.
fn how_many(state: &FixturePicker, app: &Entity<Luma>, _window: &Window) -> impl IntoElement {
    let labels: Vec<&str> = SUBSETS.iter().map(|(label, _)| *label).collect();
    let toggle = app.clone();
    let pick = app.clone();
    div()
        .flex_none()
        .h(px(44.0))
        .px(px(10.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(float::label("Use"))
        .child(luma_ui::arg::select::luma_arg_select(
            "fixture-picker:subset",
            &crate::track_editor::subset_label(state.subset),
            &labels,
            state.subset_open,
            move |_, cx| {
                toggle.update(cx, |this, cx| {
                    if let Some(Overlay::FixturePicker(state)) = this.overlay.open_mut() {
                        state.subset_open = !state.subset_open;
                        cx.notify();
                    }
                });
            },
            move |picked, _, cx| {
                pick.update(cx, |this, cx| {
                    if let Some(Overlay::FixturePicker(state)) = this.overlay.open_mut() {
                        state.subset_open = false;
                        state.subset = SUBSETS[picked].1;
                        cx.notify();
                    }
                });
            },
        ))
}

/// The key legend, and — after the spacer, where `add_tracks` puts its import
/// status — what Apply would write. The readout is not an action, so it sits
/// on the quiet side of the band rather than competing with the chip above it.
fn footer(state: &FixturePicker) -> impl IntoElement {
    let summary: SharedString = state.expression().into();
    float::footer_band()
        .child(float::key_hint_text("↵", "Apply"))
        .child(div().flex_1().min_w_0())
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(11.0))
                .text_color(ladder::muted_foreground())
                .child(summary.clone())
                .agent_node(Role::Text, summary),
        )
}
