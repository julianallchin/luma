//! The preset browser: what the inspector shows while no clip is selected.
//!
//! The shipped presets, grouped by form, each a tile with a picture of the
//! preset on this rig: the timeline's clip strip (time across, heads down
//! along the form's axis), rendered once per preset and target selection.
//!
//! A click places the preset at the playhead on the selected lane; a drag
//! places it where it is dropped. Both go through the picker's insertion
//! path. While the pointer is over a tile, the stage plays the preset alone
//! on the target selection, a two-bar loop from the playhead. The score and
//! the transport do not change; leaving the tile gives the stage back.

use std::collections::VecDeque;

use luma_lib::models::universe::UniverseState;
use luma_lib::services::graph_scores::ClipPreview;
use luma_patterns::FormPreset;
use luma_ui::text_input::TextInput;

use super::*;

/// The id of the one clip in a thumbnail or audition score.
const CLIP_ID: &str = "preset";
/// Beats a thumbnail shows.
const THUMB_BEATS: f64 = 8.;
/// Beats the stage loops while a tile is hovered: two bars.
const AUDITION_BEATS: f64 = 8.;
/// Bars a placed preset lasts.
const PLACE_BARS: usize = 4;
/// Tiles in one row.
const COLUMNS: usize = 2;
const TILE_GAP: f32 = 4.;
const TILE_PAD: f32 = 4.;
const TILE_W: f32 = (FIELD_W - TILE_GAP * (COLUMNS as f32 - 1.)) / COLUMNS as f32;
const THUMB_H: f32 = 32.;
/// Prepared audition programs kept, newest last.
const PREPARED: usize = 8;

/// The browser's own state, on the sheet.
#[derive(Default)]
pub(crate) struct State {
    search: Option<Entity<TextInput>>,
    query: String,
    /// Thumbnails by preset name and target selection expression.
    thumbs: HashMap<(String, String), Thumbnail>,
    /// A thumbnail is being rendered. One at a time.
    thumbing: bool,
    /// The preset under the pointer.
    hovered: Option<String>,
    /// Prepared single-clip programs, newest last.
    prepared: VecDeque<(Key, Arc<ClipPreview>)>,
    /// What the stage plays instead of the score.
    audition: Option<Audition>,
}

enum Thumbnail {
    Ready(Arc<RenderImage>),
    Failed,
}

/// What an audition program was prepared for.
#[derive(Clone, PartialEq)]
struct Key {
    preset: String,
    selection: String,
    /// The whole beat the loop starts on.
    start: i64,
}

/// One preset, played alone on the stage in a loop. The stage samples it in
/// place of the score while it is set.
#[derive(Clone)]
pub(crate) struct Audition {
    pub(crate) name: SharedString,
    scene: Arc<ClipPreview>,
    started: std::time::Instant,
    arena: Rc<RefCell<luma_lib::eval::Arena>>,
}

impl Audition {
    fn new(name: &str, scene: Arc<ClipPreview>) -> Self {
        Self {
            name: name.to_owned().into(),
            scene,
            started: std::time::Instant::now(),
            arena: Rc::default(),
        }
    }

    /// The loop's track time now, and the rig at that time.
    pub(crate) fn sample(&self) -> Result<(f32, UniverseState), String> {
        let (start, end) = self.scene.span;
        let time = start + self.started.elapsed().as_secs_f32() % (end - start).max(1e-3);
        let mut frames = self.scene.scene.try_render(
            &[time],
            luma_lib::eval::Scope::Composite,
            &mut self.arena.borrow_mut(),
        )?;
        Ok((time, frames.pop().unwrap_or_default()))
    }
}

impl Editor {
    /// The preset the stage is playing in place of the score, if any.
    pub(crate) fn audition(&self) -> Option<Audition> {
        self.sheet.browser.audition.clone()
    }
}

// -- sync ---------------------------------------------------------------------

/// Called while no clip is selected: read the search and render the next
/// missing thumbnail.
pub(super) fn sync(editor: &mut Editor, cx: &mut Context<Luma>) {
    let search = editor
        .sheet
        .browser
        .search
        .get_or_insert_with(|| cx.new(|cx| TextInput::search("Search presets…", cx)))
        .clone();
    editor.sheet.browser.query = search.read(cx).text().to_string();
    fetch_thumbnail(editor, cx);
}

/// Called while a clip is selected: the browser is gone, and so is anything
/// it was playing.
pub(super) fn leave(editor: &mut Editor) {
    let browser = &mut editor.sheet.browser;
    browser.hovered = None;
    browser.audition = None;
}

/// The presets matching the query, in shipped order. A query matches a
/// preset's name or its form's name.
fn matching(query: &str) -> Vec<&'static FormPreset> {
    let query = query.trim().to_lowercase();
    luma_patterns::presets()
        .presets
        .iter()
        .filter(|preset| {
            preset.name.to_lowercase().contains(&query)
                || form_name(&preset.form).to_lowercase().contains(&query)
        })
        .collect()
}

/// The matching presets by form, forms in the order they first appear.
fn grouped(query: &str) -> Vec<(&'static str, Vec<&'static FormPreset>)> {
    let mut groups: Vec<(&'static str, Vec<&'static FormPreset>)> = Vec::new();
    for preset in matching(query) {
        let form = form_name(&preset.form);
        match groups.iter_mut().find(|(name, _)| *name == form) {
            Some((_, presets)) => presets.push(preset),
            None => groups.push((form, vec![preset])),
        }
    }
    groups
}

/// The selection a new clip from the browser lights: that of the clip on
/// the selected lane nearest the playhead, or everything.
fn target_selection(editor: &Editor) -> luma_patterns::Selection {
    editor
        .cursor
        .and_then(|cursor| lane_selection(editor, cursor.row, f64::from(editor.transport.position)))
        .unwrap_or_else(luma_patterns::Selection::all)
}

/// The selection of the clip on lane `row` nearest `time`.
fn lane_selection(editor: &Editor, row: usize, time: f64) -> Option<luma_patterns::Selection> {
    let distance = |clip: &Clip| {
        if time < clip.start {
            clip.start - time
        } else {
            (time - clip.end).max(0.)
        }
    };
    editor
        .clips
        .iter()
        .filter(|clip| clip.row == row)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .and_then(|clip| clip.core.as_ref())
        .map(|clip| clip.selection.clone())
}

/// A score holding only `preset` as one clip, for a thumbnail or an
/// audition.
fn single_clip_score(
    editor: &Editor,
    preset: &FormPreset,
    start: f64,
    duration: f64,
    selection: &luma_patterns::Selection,
) -> Result<luma_patterns::Score, String> {
    let mut score = editor.graph_candidate()?;
    score.clips.clear();
    let mut clip = preset.clip(start, duration);
    clip.selection = selection.clone();
    score.clips.insert(CLIP_ID.to_owned(), clip);
    Ok(score)
}

fn target(editor: &Editor) -> Target {
    Target::TrackEditor {
        track: editor.track_id.to_string(),
        venue: editor.venue_id.clone(),
    }
}

/// Render the first shown tile that has no thumbnail yet for the current
/// target selection.
fn fetch_thumbnail(editor: &mut Editor, cx: &mut Context<Luma>) {
    if editor.sheet.browser.thumbing || editor.graph_score.is_none() || editor.beats.is_none() {
        return;
    }
    let Some(score_id) = editor.score.as_ref().map(|score| score.id.clone()) else {
        return;
    };
    let selection = target_selection(editor);
    let browser = &editor.sheet.browser;
    let Some(preset) = matching(&browser.query).into_iter().find(|preset| {
        !browser
            .thumbs
            .contains_key(&(preset.name.clone(), selection.expression.clone()))
    }) else {
        return;
    };
    let key = (preset.name.clone(), selection.expression.clone());
    let score = match single_clip_score(editor, preset, 0., THUMB_BEATS, &selection) {
        Ok(score) => score,
        Err(_) => {
            editor.sheet.browser.thumbs.insert(key, Thumbnail::Failed);
            return;
        }
    };
    editor.sheet.browser.thumbing = true;
    let target = target(editor);
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| {
            this.library.preview_score_clip(&score_id, CLIP_ID, &score)
        }) else {
            return;
        };
        let result = pending.await;
        this.update(cx, |this, cx| {
            this.edit_track_tab(&target, cx, |editor| {
                let browser = &mut editor.sheet.browser;
                browser.thumbing = false;
                let thumb = match result {
                    Ok(row)
                        if row.width > 0
                            && row.height > 0
                            && row.pixels.len() == (row.width * row.height * 4) as usize =>
                    {
                        Thumbnail::Ready(Arc::new(bake(row.width, row.height, &row.pixels)))
                    }
                    _ => Thumbnail::Failed,
                };
                browser.thumbs.insert(key, thumb);
            });
        })
        .ok();
    })
    .detach();
}

// -- gestures -----------------------------------------------------------------

impl Luma {
    /// The pointer entered or left a tile. Entering plays the preset on the
    /// stage once its program is ready; leaving gives the stage back at once.
    fn hover_preset(&mut self, preset: &'static FormPreset, over: bool, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() else {
            return;
        };
        if !over {
            let browser = &mut editor.sheet.browser;
            if browser.hovered.as_deref() == Some(preset.name.as_str()) {
                browser.hovered = None;
                browser.audition = None;
                cx.notify();
            }
            return;
        }
        editor.sheet.browser.hovered = Some(preset.name.clone());
        editor.sheet.browser.audition = None;
        cx.notify();
        let Some(score_id) = editor.score.as_ref().map(|score| score.id.clone()) else {
            return;
        };
        let Some(start) = editor
            .beats
            .as_ref()
            .and_then(|grid| grid.timeline().ok())
            .and_then(|clock| clock.beat_at(f64::from(editor.transport.position)).ok())
            .map(|beat| beat.round() as i64)
        else {
            return;
        };
        let selection = target_selection(editor);
        let key = Key {
            preset: preset.name.clone(),
            selection: selection.expression.clone(),
            start,
        };
        let browser = &mut editor.sheet.browser;
        if let Some((_, scene)) = browser.prepared.iter().find(|(held, _)| *held == key) {
            browser.audition = Some(Audition::new(&preset.name, Arc::clone(scene)));
            return;
        }
        let Ok(score) = single_clip_score(editor, preset, start as f64, AUDITION_BEATS, &selection)
        else {
            return;
        };
        let pending = self
            .library
            .prepare_score_clip_preview(&score_id, CLIP_ID, &score);
        let target = target(editor);
        cx.spawn(async move |this, cx| {
            let Ok(scene) = pending.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.edit_track_tab(&target, cx, |editor| {
                    let browser = &mut editor.sheet.browser;
                    let scene = Arc::new(scene);
                    if browser.prepared.len() >= PREPARED {
                        browser.prepared.pop_front();
                    }
                    browser
                        .prepared
                        .push_back((key.clone(), Arc::clone(&scene)));
                    if browser.hovered.as_deref() == Some(key.preset.as_str()) {
                        browser.audition = Some(Audition::new(&key.preset, scene));
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// A click on a tile: place the preset at the playhead on the selected
    /// lane.
    fn place_preset(&mut self, preset: &'static FormPreset, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        if !editor.writable() {
            return;
        }
        let beats = editor.beats.as_deref();
        let playhead = f64::from(editor.transport.position);
        let start = snap(beats, playhead, editor.view.zoom, SNAP_CAPTURE).max(0.);
        let end = bars_after(beats, start, PLACE_BARS).min(f64::from(editor.transport.duration));
        if end - start < MIN_CLIP {
            return;
        }
        let menu = InsertMenu {
            start,
            end,
            row: editor.cursor.map_or(1, |cursor| cursor.row),
            insert: false,
            active: 0,
        };
        let selection = target_selection(editor);
        self.insert_pattern(menu, InsertChoice(preset), selection, cx);
    }

    /// A tile dropped on the timeline at `at`, a window position: place the
    /// preset there, on the selection of the lane it lands on.
    pub(in crate::track_editor) fn drop_preset(
        &mut self,
        drag: &PresetDrag,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        let Some(menu) = editor.insertion_at(at, PLACE_BARS) else {
            return;
        };
        let selection = (!menu.insert)
            .then(|| lane_selection(editor, menu.row, menu.start))
            .flatten()
            .unwrap_or_else(luma_patterns::Selection::all);
        self.insert_pattern(menu, InsertChoice(drag.0), selection, cx);
    }
}

/// What a tile drag carries.
pub(in crate::track_editor) struct PresetDrag(&'static FormPreset);

impl Render for PresetDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        float::chip().child(self.0.name.clone())
    }
}

// -- rendering ----------------------------------------------------------------

/// The browser's content: the search, then the tiles by form.
pub(super) fn body(state: &Editor, app: &Entity<Luma>) -> AnyElement {
    let pad = px(luma_ui::sheet::PAD);
    let browser = &state.sheet.browser;
    let selection = target_selection(state).expression;
    let groups = grouped(&browser.query);
    div()
        .size_full()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .px(pad)
                .pt(pad)
                .pb(px(12.))
                .children(browser.search.clone())
                .agent_node(Role::Input, "Search presets…"),
        )
        .child(luma_ui::float::divider())
        .child(
            luma_ui::float::viewport().child(
                div()
                    .id("preset-browser")
                    .size_full()
                    .overflow_y_scroll()
                    .px(pad)
                    .pt(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(ROW_GAP))
                    .when(groups.is_empty(), |list| {
                        list.child(float::empty_row("No matching presets"))
                    })
                    .children(groups.into_iter().map(|(form, presets)| {
                        let tiles: Vec<AnyElement> = presets
                            .into_iter()
                            .map(|preset| {
                                let thumb = browser
                                    .thumbs
                                    .get(&(preset.name.clone(), selection.clone()));
                                tile(preset, thumb, app)
                            })
                            .collect();
                        let mut rows = Vec::new();
                        let mut tiles = tiles.into_iter().peekable();
                        while tiles.peek().is_some() {
                            rows.push(
                                div()
                                    .flex()
                                    .gap(px(TILE_GAP))
                                    .children(tiles.by_ref().take(COLUMNS)),
                            );
                        }
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(luma_ui::caption(form))
                            .children(rows)
                    }))
                    .child(div().h(pad).flex_none()),
            ),
        )
        .into_any_element()
}

fn tile(preset: &'static FormPreset, thumb: Option<&Thumbnail>, app: &Entity<Luma>) -> AnyElement {
    let name: SharedString = preset.name.clone().into();
    let tip = name.clone();
    let hover = app.clone();
    let place = app.clone();
    div()
        .id(SharedString::from(format!("preset-tile-{}", preset.name)))
        .w(px(TILE_W))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(TILE_PAD))
        .rounded(px(luma_ui::radius::ROW))
        .cursor_pointer()
        .hover(|style| style.bg(luma_ui::glass::glass_hover()))
        .child(thumbnail(&preset.name, thumb))
        .child(
            div()
                .w_full()
                .text_size(px(12.))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .text_color(ladder::foreground_alpha(0.8))
                .child(name.clone()),
        )
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .on_hover(move |over, _, cx| {
            let over = *over;
            hover.update(cx, |this, cx| this.hover_preset(preset, over, cx));
        })
        .on_click(move |_, _, cx| {
            place.update(cx, |this, cx| this.place_preset(preset, cx));
        })
        .on_drag(PresetDrag(preset), move |_, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| PresetDrag(preset))
        })
        .agent_node(Role::Button, name)
        .into_any_element()
}

/// A tile's picture: the preset's strip on this rig, or an empty frame while
/// it renders.
fn thumbnail(name: &str, thumb: Option<&Thumbnail>) -> Div {
    let frame = div()
        .w_full()
        .h(px(THUMB_H))
        .flex_none()
        .rounded(px(3.))
        .overflow_hidden()
        .border_1()
        .border_color(luma_ui::glass::hairline(0.12))
        .bg(luma_ui::glass::ink(0.03));
    match thumb {
        Some(Thumbnail::Ready(image)) => {
            let image = Arc::clone(image);
            let label = format!("{name} thumbnail");
            frame.child(
                canvas(
                    move |bounds, window, cx| {
                        agent_paint_node(Role::Card, label.clone(), bounds, window, cx);
                    },
                    move |bounds, _, window, _| {
                        // Frame 1 is the opaque one — see `bake`.
                        window
                            .paint_image(
                                bounds,
                                bounds,
                                Corners::all(px(2.)),
                                Arc::clone(&image),
                                1,
                                false,
                            )
                            .ok();
                    },
                )
                .size_full(),
            )
        }
        Some(Thumbnail::Failed) | None => frame,
    }
}

#[cfg(test)]
mod tests {
    use super::{grouped, matching};

    #[test]
    fn a_query_matches_a_preset_or_its_form() {
        let names = |query: &str| -> Vec<&str> {
            matching(query).iter().map(|p| p.name.as_str()).collect()
        };
        assert_eq!(names("bounce"), ["Bounce"]);
        let chases = names("chase");
        assert!(chases.contains(&"Wave") && chases.contains(&"Stepped chase"));
        assert!(!chases.contains(&"Wash"));
        assert_eq!(matching("").len(), luma_patterns::presets().presets.len());
        assert!(matching("no such preset").is_empty());
    }

    #[test]
    fn presets_group_by_form_in_shipped_order() {
        let forms: Vec<&str> = grouped("").iter().map(|(form, _)| *form).collect();
        assert_eq!(
            forms,
            [
                "Constant color",
                "Color over time",
                "Color across space",
                "Chase",
                "Sparkle",
                "Noise",
                "Strobe"
            ]
        );
        let color_time = &grouped("")[1].1;
        assert_eq!(
            color_time
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["Color fade", "Rainbow"]
        );
    }
}
