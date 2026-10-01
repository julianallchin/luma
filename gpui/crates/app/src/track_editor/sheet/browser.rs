//! The preset browser: what the inspector shows while no clip is selected.
//!
//! The shipped presets, grouped by output kind, one row each: the name, and
//! a strip of the preset (time across, heads down). The
//! strip is the timeline's clip strip on this rig, rendered once per preset
//! on every head. Until that lands, or when there is no score to
//! render it against, the row shows the same strip on a stand-in rig, a
//! straight line of heads that needs no venue or score.
//!
//! A click places the preset at the playhead on the selected lane; a drag
//! places it where it is dropped. Both go through the picker's insertion
//! path. While the pointer is over a tile, the stage plays the preset alone
//! on the target selection, a two-bar loop from the playhead. The score and
//! the transport do not change; leaving the tile gives the stage back.

use std::collections::VecDeque;

use luma_lib::models::universe::UniverseState;
use luma_lib::services::graph_scores::ClipPreview;
use luma_patterns::ClipPreset;
use luma_ui::text_input::TextInput;

use super::*;

/// The id of the one clip in a thumbnail or audition score.
const CLIP_ID: &str = "preset";
/// Beats a thumbnail shows: as long as a placed preset, so the strip a drag
/// carries onto the timeline is the clip it will make.
const THUMB_BEATS: f64 = 16.;
/// Beats the stage loops while a tile is hovered: two bars.
const AUDITION_BEATS: f64 = 8.;
/// Bars a placed preset lasts.
const PLACE_BARS: usize = 4;
/// A preset's row, and the strip at its right.
const ROW_H: f32 = 40.;
const STRIP: [f32; 2] = [112., 24.];
/// Prepared audition programs kept, newest last.
const PREPARED: usize = 8;

/// The browser's own state, on the sheet.
#[derive(Default)]
pub(crate) struct State {
    search: Option<Entity<TextInput>>,
    query: String,
    /// Thumbnails by preset key (see [`key`]), each on every head of the
    /// rig — not the target selection, so a tile holds still as the
    /// playhead moves.
    thumbs: HashMap<String, Thumbnail>,
    /// A thumbnail is being rendered. One at a time.
    thumbing: bool,
    /// Every preset's strip on the stand-in rig, by key, once rendered.
    stand_ins: Option<Rc<HashMap<String, Preview>>>,
    stand_ins_asked: bool,
    /// The key of the preset under the pointer.
    hovered: Option<String>,
    /// Prepared single-clip programs, newest last.
    prepared: VecDeque<(Key, Arc<ClipPreview>)>,
    /// Where a preset carried over the timeline would land.
    drop: Option<(&'static ClipPreset, InsertMenu)>,
    /// What the stage plays instead of the score.
    audition: Option<Audition>,
}

enum Thumbnail {
    Ready(Preview),
    Failed,
}

/// What an audition program was prepared for.
#[derive(Clone, PartialEq)]
struct Key {
    /// The preset's [`key`].
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
    // A drag let go anywhere but the timeline places nothing.
    if !cx.has_active_drag() {
        editor.sheet.browser.drop = None;
    }
    fetch_stand_ins(editor, cx);
    fetch_thumbnail(editor, cx);
}

/// Render every preset's strip on the stand-in rig, once, off the UI thread.
fn fetch_stand_ins(editor: &mut Editor, cx: &mut Context<Luma>) {
    if std::mem::replace(&mut editor.sheet.browser.stand_ins_asked, true) {
        return;
    }
    let target = target(editor);
    cx.spawn(async move |this, cx| {
        let strips = cx
            .background_executor()
            .spawn(async move {
                luma_patterns::presets()
                    .clips
                    .iter()
                    .filter_map(|preset| {
                        let row = luma_lib::services::graph_scores::stand_in_strip(
                            &preset.graph,
                            THUMB_BEATS,
                        )
                        .ok()?;
                        Some((key(preset), Preview::decode(row, None)?))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .await;
        this.update(cx, |this, cx| {
            this.edit_track_tab(&target, cx, |editor| {
                editor.sheet.browser.stand_ins = Some(Rc::new(strips));
            });
        })
        .ok();
    })
    .detach();
}

/// Called while a clip is selected: the browser is gone, and so is anything
/// it was playing.
pub(super) fn leave(editor: &mut Editor) {
    let browser = &mut editor.sheet.browser;
    browser.hovered = None;
    browser.audition = None;
    browser.drop = None;
}

/// A preset carried over the timeline, drawn where and as it would land.
pub(in crate::track_editor) struct DropGhost {
    pub(in crate::track_editor) clip: Clip,
    pub(in crate::track_editor) strip: Option<Preview>,
    /// It would open a new lane at the boundary above `clip.row`.
    pub(in crate::track_editor) insert: bool,
}

impl Editor {
    /// What a drop of the carried preset would place, if one is over the
    /// timeline.
    pub(in crate::track_editor) fn drop_ghost(&self) -> Option<DropGhost> {
        let (preset, menu) = self.sheet.browser.drop?;
        let mut clip = Clip {
            id: "drop-ghost".into(),
            output: SharedString::default(),
            label: SharedString::default(),
            summary: SharedString::default(),
            color: ladder::pattern(""),
            start: menu.start,
            end: menu.end,
            row: menu.row,
            z: 0,
            blend: preset.blend_mode,
            core: Some(preset.clip(0., 1.)),
        };
        clip.refresh();
        Some(DropGhost {
            clip,
            strip: strip(&self.sheet.browser, preset),
            insert: menu.insert,
        })
    }
}

/// A preset's strip: on the real rig once it has rendered, else on the
/// stand-in rig.
fn strip(browser: &State, preset: &ClipPreset) -> Option<Preview> {
    match browser.thumbs.get(&key(preset)) {
        Some(Thumbnail::Ready(preview)) => Some(preview.clone()),
        _ => browser
            .stand_ins
            .as_ref()
            .and_then(|strips| strips.get(&key(preset)))
            .cloned(),
    }
}

/// What tells a preset apart: its name, unique across kinds.
fn key(preset: &ClipPreset) -> String {
    preset.name.clone()
}

/// The presets matching the query, in shipped order. A query matches a
/// preset's name or its output kind.
fn matching(query: &str) -> Vec<&'static ClipPreset> {
    let query = query.trim().to_lowercase();
    luma_patterns::presets()
        .clips
        .iter()
        .filter(|preset| {
            preset.name.to_lowercase().contains(&query)
                || preset.output_kind().label().to_lowercase().contains(&query)
        })
        .collect()
}

/// The matching presets by output kind, kinds in the order they first
/// appear.
fn grouped(query: &str) -> Vec<(&'static str, Vec<&'static ClipPreset>)> {
    let mut groups: Vec<(&'static str, Vec<&'static ClipPreset>)> = Vec::new();
    for preset in matching(query) {
        let form = preset.output_kind().label();
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
    preset: &ClipPreset,
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

/// Render the first shown tile that has no thumbnail yet, on every head.
fn fetch_thumbnail(editor: &mut Editor, cx: &mut Context<Luma>) {
    if editor.sheet.browser.thumbing || editor.graph_score.is_none() || editor.beats.is_none() {
        return;
    }
    let Some(score_id) = editor.score.as_ref().map(|score| score.id.clone()) else {
        return;
    };
    let selection = luma_patterns::Selection::all();
    let browser = &editor.sheet.browser;
    let Some(preset) = matching(&browser.query)
        .into_iter()
        .find(|preset| !browser.thumbs.contains_key(&key(preset)))
    else {
        return;
    };
    let key = key(preset);
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
                let thumb = match result.ok().and_then(|row| Preview::decode(row, None)) {
                    Some(preview) => Thumbnail::Ready(preview),
                    None => Thumbnail::Failed,
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
    fn hover_preset(&mut self, preset: &'static ClipPreset, over: bool, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() else {
            return;
        };
        if !over {
            let browser = &mut editor.sheet.browser;
            if browser.hovered.as_deref() == Some(key(preset).as_str()) {
                browser.hovered = None;
                browser.audition = None;
                cx.notify();
            }
            return;
        }
        editor.sheet.browser.hovered = Some(key(preset));
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
            preset: self::key(preset),
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
        let name = preset.name.clone();
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
                        browser.audition = Some(Audition::new(&name, scene));
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// A click on a tile: place the preset at the playhead on the selected
    /// lane.
    fn place_preset(&mut self, preset: &'static ClipPreset, cx: &mut Context<Self>) {
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
        self.insert_pattern(menu, InsertChoice::Preset(preset), selection, cx);
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
        if let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() {
            editor.sheet.browser.drop = None;
        }
        self.insert_pattern(menu, InsertChoice::Preset(drag.0), selection, cx);
    }
}

/// What a tile drag carries.
#[derive(Clone, Copy)]
pub(in crate::track_editor) struct PresetDrag(&'static ClipPreset);

/// What follows the pointer during a drag: the preset's strip and name.
struct Carried {
    name: SharedString,
    strip: Option<Preview>,
}

impl Render for Carried {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        float::frosted_card(
            float::popover_card()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .p(px(6.))
                .text_size(px(13.))
                .text_color(ladder::foreground())
                .child(thumbnail(&self.name, self.strip.clone()))
                .child(self.name.clone()),
        )
    }
}

impl Luma {
    /// The carried preset moved: over the timeline, it would land at `at`;
    /// anywhere else (`None`), it would not land at all.
    pub(in crate::track_editor) fn carry_preset(
        &mut self,
        drag: &PresetDrag,
        at: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() else {
            return;
        };
        let drop = at
            .and_then(|at| editor.insertion_at(at, PLACE_BARS))
            .map(|menu| (drag.0, menu));
        let same = match (&drop, &editor.sheet.browser.drop) {
            (Some((a, m)), Some((b, n))) => {
                std::ptr::eq(*a, *b)
                    && m.start == n.start
                    && m.end == n.end
                    && m.row == n.row
                    && m.insert == n.insert
            }
            (None, None) => true,
            _ => false,
        };
        if !same {
            editor.sheet.browser.drop = drop;
            cx.notify();
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// The browser's content: the search, then a row per preset under its form.
pub(super) fn body(state: &Editor, app: &Entity<Luma>) -> AnyElement {
    let pad = px(luma_ui::sheet::PAD);
    let browser = &state.sheet.browser;
    let groups = grouped(&browser.query);
    div()
        .size_full()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .p(px(10.))
                .children(browser.search.clone())
                .agent_node(Role::Input, "Search presets…"),
        )
        .child(luma_ui::float::divider())
        .child(
            luma_ui::float::viewport().child(
                luma_ui::float::list()
                    .id("preset-browser")
                    .overflow_y_scroll()
                    .when(groups.is_empty(), |list| {
                        list.child(float::empty_row("No matching presets"))
                    })
                    .children(groups.into_iter().map(|(form, presets)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .pb(px(8.))
                            .child(
                                div()
                                    .h(px(CONTROL_HEIGHT))
                                    .px(px(float::ROW_INSET))
                                    .flex()
                                    .items_center()
                                    .child(luma_ui::caption(form)),
                            )
                            .children(
                                presets
                                    .into_iter()
                                    .map(|preset| row(preset, strip(browser, preset), app)),
                            )
                    }))
                    .child(div().h(pad).flex_none()),
            ),
        )
        .into_any_element()
}

fn row(preset: &'static ClipPreset, strip: Option<Preview>, app: &Entity<Luma>) -> AnyElement {
    let name: SharedString = preset.name.clone().into();
    let key = format!("preset-row-{}", key(preset));
    let carried = strip.clone();
    let hover = app.clone();
    let place = app.clone();
    // The row's own hover is its fade; the stage preview listens on a
    // wrapper, as the insert picker's rows do.
    div()
        .id(SharedString::from(format!("{key}-hover")))
        .w_full()
        .flex_none()
        .on_hover(move |over, _, cx| {
            let over = *over;
            hover.update(cx, |this, cx| this.hover_preset(preset, over, cx));
        })
        .child(
            float::menu_row(float::RowState::Rest, key.clone())
                .h(px(ROW_H))
                .w_full()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(name.clone()),
                )
                .child(thumbnail(&preset.name, strip))
                .id(SharedString::from(key))
                .on_click(move |_, _, cx| {
                    place.update(cx, |this, cx| this.place_preset(preset, cx));
                })
                .on_drag(PresetDrag(preset), move |_, _, _, cx| {
                    cx.stop_propagation();
                    let strip = carried.clone();
                    cx.new(|_| Carried {
                        name: preset.name.clone().into(),
                        strip,
                    })
                })
                .agent_node(Role::Row, name),
        )
        .into_any_element()
}

/// A row's strip: on the real rig, else on the stand-in rig, else a flat
/// frame for the moment the stand-ins take to render.
fn thumbnail(name: &str, strip: Option<Preview>) -> Div {
    let frame = div()
        .w(px(STRIP[0]))
        .h(px(STRIP[1]))
        .flex_none()
        .rounded(px(3.))
        .overflow_hidden()
        .border_1()
        .border_color(luma_ui::glass::hairline(0.12))
        .bg(luma_ui::glass::ink(0.06));
    let Some(preview) = strip else {
        return frame;
    };
    let label = format!("{name} thumbnail");
    frame.child(
        canvas(
            move |bounds, window, cx| {
                agent_paint_node(Role::Card, label.clone(), bounds, window, cx);
            },
            move |bounds, _, window, _| {
                // Opaque, as a selected clip's body.
                preview.paint(bounds, true, Corners::all(px(2.)), window);
            },
        )
        .size_full(),
    )
}

#[cfg(test)]
mod tests {
    use super::{grouped, matching};

    #[test]
    fn a_query_matches_a_preset_or_its_output() {
        let names = |query: &str| -> Vec<&str> {
            matching(query).iter().map(|p| p.name.as_str()).collect()
        };
        assert_eq!(names("ballyhoo"), ["Ballyhoo"]);
        let chases = names("chase");
        assert!(chases.contains(&"Chase") && chases.contains(&"Stepped chase"));
        assert!(!chases.contains(&"Wash"));
        // The output's name matches too: every aim preset is an Aim.
        assert!(names("aim").contains(&"Sweep"));
        assert_eq!(matching("").len(), luma_patterns::presets().clips.len());
        assert!(matching("no such preset").is_empty());
    }

    #[test]
    fn presets_group_by_output_in_shipped_order() {
        let kinds: Vec<&str> = grouped("").iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, ["Color", "Aim", "Strobe"]);
        let color = &grouped("")[0].1;
        assert_eq!(color.first().map(|p| p.name.as_str()), Some("Wash"));
    }
}
