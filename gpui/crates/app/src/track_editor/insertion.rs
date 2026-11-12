//! Putting a new clip down: where a right-click or the toolbar would place it,
//! the patterns on offer, and the insert itself.

use super::*;

/// A right-click's pending insertion: where the clip would go, and the
/// patterns on offer.
///
/// `insert` distinguishes the two modes — dropping a clip *onto* the
/// lane under the pointer, or opening a *new* lane at the boundary the pointer
/// is within a quarter-lane of, shifting everything at or above it up.
#[derive(Clone, Copy)]
pub(super) struct InsertMenu {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) row: usize,
    pub(super) insert: bool,
    /// The pattern highlighted by either keyboard or pointer navigation.
    pub(super) active: usize,
}

/// A row of the insertion picker: a shipped preset, or a blank start with
/// only an output node.
#[derive(Clone, Copy)]
pub(super) enum InsertChoice {
    Preset(&'static luma_patterns::ClipPreset),
    Blank(luma_patterns::clip_graph::Kind),
}

impl InsertChoice {
    /// The blank starts, after the presets.
    const BLANKS: [luma_patterns::clip_graph::Kind; 3] = [
        luma_patterns::clip_graph::Kind::Color,
        luma_patterns::clip_graph::Kind::Aim,
        luma_patterns::clip_graph::Kind::Strobe,
    ];

    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Preset(preset) => &preset.name,
            Self::Blank(kind) => kind.label(),
        }
    }
    /// What tells rows apart. Preset names are unique across kinds.
    pub(super) fn id(&self) -> String {
        match self {
            Self::Preset(preset) => preset.name.clone(),
            Self::Blank(kind) => format!("blank-{}", kind.name()),
        }
    }
    /// What the picker shows beside the name: the output kind, or "Blank".
    pub(super) fn origin(&self) -> &'static str {
        match self {
            Self::Preset(preset) => preset.output_kind().label(),
            Self::Blank(_) => "Blank",
        }
    }
    pub(super) fn graph(&self) -> luma_patterns::ClipGraph {
        match self {
            Self::Preset(preset) => preset.graph.clone(),
            Self::Blank(kind) => luma_patterns::ClipGraph::new([(
                format!("{}1", kind.name()),
                luma_patterns::clip_graph::Node::new(*kind),
            )]),
        }
    }
    /// A clip of this choice from `start` for `duration` beats. A blank
    /// start is named after its output, as the checker wants a name.
    pub(super) fn clip(&self, start: f64, duration: f64) -> luma_patterns::Clip {
        match self {
            Self::Preset(preset) => preset.clip(start, duration),
            Self::Blank(kind) => luma_patterns::Clip {
                name: kind.label().into(),
                label: None,
                start,
                duration,
                seed: 0,
                selection: luma_patterns::Selection::all(),
                z_index: 0,
                blend_mode: BlendMode::Replace,
                graph: self.graph(),
            },
        }
    }
}

/// How close to a lane *boundary* a right-click has to be, in lanes, to mean
/// "open a new layer here" rather than "drop it on this lane".
const INSERT_BOUNDARY: f32 = 0.25;

impl Editor {
    /// Walk the insertion menu's active row. Clamped at both ends: a menu that
    /// wrapped would commit the wrong pattern to a hand
    /// that held the key a beat too long.
    fn step_menu(&mut self, down: bool) {
        let last = self.insertion_choices().len().saturating_sub(1);
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        menu.active = if down {
            (menu.active + 1).min(last)
        } else {
            menu.active.saturating_sub(1)
        };
        self.menu_scroll.scroll_to_item(menu.active);
    }

    /// The presets the picker lists for the current query, in menu order,
    /// then the blank starts. A query matches a row's name or its output
    /// kind.
    pub(super) fn insertion_choices(&self) -> Vec<InsertChoice> {
        let query = self.menu_query.to_lowercase();
        luma_patterns::presets()
            .clips
            .iter()
            .map(InsertChoice::Preset)
            .chain(InsertChoice::BLANKS.map(InsertChoice::Blank))
            .filter(|choice| {
                choice.name().to_lowercase().contains(&query)
                    || choice.origin().to_lowercase().contains(&query)
            })
            .collect()
    }

    /// Where a clip `bars` long would go for a gesture at `at`, a window
    /// position: the snapped time under it, and either the lane under it or
    /// a new lane at the boundary it is within a quarter-lane of. `None` off
    /// the lanes, or with no room before the end.
    pub(super) fn insertion_at(&self, at: Point<Pixels>, bars: usize) -> Option<InsertMenu> {
        let canvas = self.canvas.get();
        let time = self.view.time_at(f32::from(at.x - canvas.origin.x));
        let y = f32::from(at.y - canvas.origin.y);
        let layout = self.layout();
        if y < TRACK_AREA_Y {
            return None;
        }
        let offset = ((y - layout.start) / layout.lane).max(0.);
        // Inside a swept selection, the clip fills its time span on the lane
        // under the pointer.
        if let Some((start, end)) = self.cursor.and_then(|cursor| {
            let (start, end) = cursor.span()?;
            let (top, bottom) = cursor.rows();
            let row = offset.floor() as usize;
            (time >= start && time <= end && (top..=bottom).contains(&row))
                .then_some((start.max(0.), end.min(f64::from(self.transport.duration))))
        }) {
            return (end - start >= MIN_CLIP).then_some(InsertMenu {
                start,
                end,
                row: offset.floor() as usize,
                insert: false,
                active: 0,
            });
        }
        let beats = self.beats.as_deref();
        let start = snap(beats, time, self.view.zoom, SNAP_CAPTURE).max(0.);
        let end = bars_after(beats, start, bars).min(f64::from(self.transport.duration));
        if end - start < MIN_CLIP {
            return None;
        }
        let layers = z_ladder(&self.clips).len();
        let boundary = offset.round();
        let insert = (offset - boundary).abs() < INSERT_BOUNDARY
            && (1. ..=layers as f32).contains(&boundary);
        Some(InsertMenu {
            start,
            end,
            row: if insert { boundary } else { offset.floor() } as usize,
            insert,
            active: 0,
        })
    }

    fn menu_choice(&self) -> Option<(InsertMenu, InsertChoice)> {
        let menu = self.menu?;
        Some((menu, *self.insertion_choices().get(menu.active)?))
    }
}

impl Luma {
    pub(super) fn add_pattern(&mut self, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            let time = editor
                .cursor
                .map_or(f64::from(editor.transport.position), |cursor| cursor.start);
            let beats = editor.beats.as_deref();
            let start = snap(beats, time, editor.view.zoom, SNAP_CAPTURE).max(0.);
            let end = editor
                .cursor
                .and_then(Cursor::span)
                .map_or_else(|| start + bar_length(beats, start), |(_, end)| end)
                .min(f64::from(editor.transport.duration));
            if end - start < MIN_CLIP {
                return;
            }
            editor.menu = Some(InsertMenu {
                start,
                end,
                row: editor.cursor.map_or(1, |cursor| cursor.row),
                insert: false,
                active: 0,
            });
            editor.menu_scroll.scroll_to_item(0);
        });
        picker::open(self, cx);
    }

    /// A right-click: work out where a clip would go and offer the patterns.
    ///
    /// `computeInsertionTarget`. The span is one bar — the next downbeat if
    /// there is one, the mean downbeat interval otherwise — and the vertical
    /// answer is two-valued: within a quarter of a lane of a *boundary* the
    /// gesture opens a new layer there, and anywhere else it drops onto the
    /// lane under the pointer.
    pub(super) fn timeline_insert_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            editor.menu = editor.insertion_at(at, 1);
            editor.menu_scroll.scroll_to_item(0);
        });
        picker::open(self, cx);
    }

    /// `ArrowUp` / `ArrowDown` in the insertion menu. A no-op with no menu
    /// open, which is what makes the bare arrows safe to bind at all: they stay
    /// unbound everywhere else in the editor.
    pub(crate) fn step_insert_menu(&mut self, down: bool, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| editor.step_menu(down));
    }

    /// `Enter` in the insertion menu: put down whichever pattern the arrows
    /// left active, exactly as a click on that row would.
    pub(crate) fn commit_insert_menu(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body() else {
            return;
        };
        let Some((menu, pattern)) = state.menu_choice() else {
            return;
        };
        self.insert_pattern(menu, pattern, luma_patterns::Selection::all(), cx);
    }

    /// Close an open insertion menu, if there is one. What `Escape` means
    /// before it means anything else — [`Luma::dismiss_overlay`] asks first,
    /// so the key puts down what the editor has open before it touches an
    /// overlay.
    pub(crate) fn dismiss_insert_menu(&mut self) -> bool {
        match self.workspace.active_body_mut() {
            Some(Body::TrackEditor(state)) => state.menu.take().is_some(),
            _ => false,
        }
    }

    /// Place the chosen preset as a new clip on `selection`.
    pub(super) fn insert_pattern(
        &mut self,
        menu: InsertMenu,
        choice: InsertChoice,
        selection: luma_patterns::Selection,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.overlay.as_open(),
            Some(crate::shell::Overlay::InsertPattern(_))
        ) {
            self.close_overlay(cx);
        }
        let mut failed = None;
        self.track_command(
            |editor| {
                if let Err(error) = editor.insert_preset(menu, choice, selection) {
                    failed = Some(error);
                }
            },
            cx,
        );
        if let Some(error) = failed {
            self.report_failure("Could not insert the pattern", error, cx);
        }
    }
}
