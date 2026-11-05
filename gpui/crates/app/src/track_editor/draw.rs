//! Painting one frame of the timeline: lanes, clips, cursor, insertion, loop
//! and playhead.

use super::*;

/// Paint the timeline in order: ground, ruler,
/// waveform, lanes and clips, playhead.
///
/// The beat grid goes down *before* the waveform and the clips, so its lines
/// run under both — which is what makes
/// a clip's translucent body show the beats through it.
pub(super) fn paint(bounds: Bounds<Pixels>, scene: &Scene, window: &mut Window, cx: &mut App) {
    window.paint_quad(fill(bounds, ladder::background()));
    window.paint_quad(fill(
        Bounds {
            origin: bounds.origin,
            size: size(bounds.size.width, px(HEADER_HEIGHT)),
        },
        ladder::band(),
    ));

    let view = scene.view;
    let start = view.time_at(0.);
    let end = view.time_at(f32::from(bounds.size.width));

    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        match &scene.beats {
            Some(beats) => paint_beat_grid(bounds, beats, view, start, end, window, cx),
            None => paint_time_ruler(bounds, view, start, end, window, cx),
        }
        // The hairline under the header, which both rulers stop at.
        window.paint_quad(fill(
            Bounds {
                origin: point(bounds.origin.x, bounds.origin.y + px(HEADER_HEIGHT)),
                size: size(bounds.size.width, px(1.)),
            },
            ladder::border(),
        ));
        paint_waveform(bounds, scene, window);
        // The lanes are masked to the band below the waveform, because that is
        // what "the lanes scroll and the navigation surface does not" means in
        // pixels: a lifted block runs *under* the waveform rather than over it.
        let layout = scene.layout(bounds);
        window.with_content_mask(
            Some(ContentMask {
                bounds: layout.band(bounds),
            }),
            |window| {
                paint_lanes(bounds, layout, window);
                for clip in scene.clips.iter() {
                    if clip.end < start || clip.start > end {
                        continue;
                    }
                    paint_clip(
                        clip_bounds(view, layout, bounds, clip),
                        clip,
                        &scene.previews,
                        scene.selected.contains(&clip.id),
                        window,
                        cx,
                    );
                }
                if let Some(cursor) = scene.cursor {
                    paint_cursor(bounds, layout, cursor, scene.view, start, end, window);
                }
                if let Some(menu) = scene.menu {
                    paint_insertion(bounds, layout, menu, scene.view, window);
                }
                if let Some(ghost) = &scene.ghost {
                    paint_ghost(bounds, layout, scene, ghost, window, cx);
                }
            },
        );
        if let Some(region) = scene.loop_region {
            paint_loop(bounds, region, scene.view, window);
        }
        paint_playhead(bounds, scene, start, end, window);
    });
}

/// The box a canvas 2D stroke of `width` centred on `x + 0.5` actually covers.
/// Every timeline coordinate is written that way; this is the one place the
/// spelling is converted.
pub(super) fn hairline(
    canvas: Bounds<Pixels>,
    x: f32,
    top: f32,
    bottom: f32,
    width: f32,
) -> Bounds<Pixels> {
    Bounds {
        origin: point(
            canvas.origin.x + px(x + 0.5 - width / 2.),
            canvas.origin.y + px(top),
        ),
        size: size(px(width), px(bottom - top)),
    }
}

/// The same color at a different alpha.
pub(super) fn fade(color: Rgba, alpha: f32) -> Hsla {
    let mut color: Hsla = color.into();
    color.a = alpha;
    color
}

/// Draw the main strip using the same pixel sampler as the overview.
fn paint_waveform(canvas: Bounds<Pixels>, scene: &Scene, window: &mut Window) {
    waveform::paint(
        Bounds {
            origin: point(canvas.origin.x, canvas.origin.y + px(HEADER_HEIGHT)),
            size: size(canvas.size.width, px(WAVEFORM_HEIGHT)),
        },
        scene.waveform.as_deref(),
        Some(scene.view),
        window,
    );
}

/// `drawAnnotations`' ground: the alternating lane fills and the hairline
/// under each. What is *not* a lane is left as the canvas's own ground.
fn paint_lanes(canvas: Bounds<Pixels>, layout: Layout, window: &mut Window) {
    let width = canvas.size.width;
    let strip = |top: f32, height: f32, color: Hsla, window: &mut Window| {
        window.paint_quad(fill(
            Bounds {
                origin: point(canvas.origin.x, canvas.origin.y + px(top)),
                size: size(width, px(height)),
            },
            color,
        ));
    };

    // The navigation surface above lane 0 and the floor below the last one are
    // painted by *not* painting: a region with no lane in it is the ground,
    // and the ground is already down. Darkening them would be depth below the
    // floor, which this ladder does not have.
    for lane in 0..layout.rows {
        let top = layout.top(lane);
        let alpha = if lane % 2 == 0 { 0.2 } else { 0.15 };
        strip(top, layout.lane, fade(ladder::card(), alpha), window);
        window.paint_quad(fill(
            Bounds {
                origin: point(
                    canvas.origin.x,
                    canvas.origin.y + px(top + layout.lane - 0.5),
                ),
                size: size(width, px(1.)),
            },
            ladder::border(),
        ));
    }
}

/// Where a carried preset would sit: in its lane, or across the boundary
/// where it would open a new one.
pub(super) fn ghost_box(
    canvas: Bounds<Pixels>,
    layout: Layout,
    scene: &Scene,
    ghost: &sheet::DropGhost,
) -> Bounds<Pixels> {
    let mut box_ = clip_bounds(scene.view, layout, canvas, &ghost.clip);
    if ghost.insert {
        box_.origin.y -= px(layout.lane / 2.);
    }
    box_
}

/// A carried preset, drawn as the clip it would make, with its strip.
fn paint_ghost(
    canvas: Bounds<Pixels>,
    layout: Layout,
    scene: &Scene,
    ghost: &sheet::DropGhost,
    window: &mut Window,
    cx: &mut App,
) {
    if ghost.insert {
        let menu = InsertMenu {
            start: ghost.clip.start,
            end: ghost.clip.end,
            row: ghost.clip.row,
            insert: true,
            active: 0,
        };
        paint_insertion(canvas, layout, menu, scene.view, window);
    }
    let previews = RefCell::new(
        ghost
            .strip
            .clone()
            .map(|preview| {
                (
                    ghost.clip.id.clone(),
                    Installed {
                        preview,
                        published: true,
                    },
                )
            })
            .into_iter()
            .collect(),
    );
    paint_clip(
        ghost_box(canvas, layout, scene, ghost),
        &ghost.clip,
        &previews,
        true,
        window,
        cx,
    );
}

/// One clip: an opaque header plate over a translucent body — the heatmap
/// where one has landed, the pattern colour where none has — inside a border,
/// with grab handles at both ends once it is selected.
fn paint_clip(
    box_: Bounds<Pixels>,
    clip: &Clip,
    previews: &RefCell<HashMap<SharedString, Installed>>,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) {
    // Every primitive outside a layer costs gpui a search of everything
    // painted so far, to order it; inside one, the layer's single ordering
    // stands for all of it. So a clip is two layers: the plate, body and
    // alpha line, then the frame and label over them. A layer draws its
    // sprites after its quads and paths, so a heatmap, which is a sprite,
    // goes down first on its own.
    let body_alpha = if selected { 1. } else { BODY_ALPHA };
    let header = Bounds {
        origin: box_.origin,
        size: size(box_.size.width, px(CLIP_HEADER)),
    };
    let pictured = paint_preview(clip_body(box_), clip, previews, selected, window);
    window.paint_layer(box_, |window| {
        window.paint_quad(fill(header, clip.color));
        if !pictured {
            window.paint_quad(fill(clip_body(box_), fade(clip.color, body_alpha)));
        }
        fades::paint(box_, clip, selected, window);
    });
    window.paint_layer(box_, |window| paint_frame(box_, clip, selected, window, cx));
}

/// A clip's border, header line, grips and label: everything over its body.
fn paint_frame(
    box_: Bounds<Pixels>,
    clip: &Clip,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(quad(
        box_,
        Corners::default(),
        transparent_black(),
        Edges::all(px(if selected { 1.5 } else { 1. })),
        fade(ladder::foreground(), if selected { 0.9 } else { 0.35 }),
        BorderStyle::Solid,
    ));
    // The line between the header and the body.
    window.paint_quad(fill(
        Bounds {
            origin: point(box_.origin.x, box_.origin.y + px(CLIP_HEADER)),
            size: size(box_.size.width, px(1.)),
        },
        fade(ladder::foreground(), 0.35),
    ));

    if selected {
        for x in [
            box_.origin.x,
            box_.origin.x + box_.size.width - px(HANDLE_MARK),
        ] {
            window.paint_quad(fill(
                Bounds {
                    origin: point(x, box_.origin.y),
                    size: size(px(HANDLE_MARK), px(CLIP_HEADER)),
                },
                fade(ladder::foreground(), 0.9),
            ));
            // Three grip dots down the middle of each plate. Each is a 2px
            // square; a round dot would look the same at this size.
            let centre = box_.origin.y + px(CLIP_HEADER / 2.);
            for step in -1..=1 {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(
                            x + px(HANDLE_MARK / 2.) - px(1.),
                            centre + px(step as f32 * GRIP_SPACING) - px(1.),
                        ),
                        size: size(px(2.), px(2.)),
                    },
                    fade(ladder::background(), 0.5),
                ));
            }
        }
    }

    // Below this the label is a smudge and shaping is the most expensive thing
    // on the canvas, so the shape stays and the text goes.
    if f32::from(box_.size.width) <= 30. {
        return;
    }
    // The clip clips its own label: a name too long for the header is cut off
    // at the edge. The name is the verb, bright; the summary after it is the
    // detail, dimmer.
    let text = Bounds {
        origin: point(box_.origin.x + px(8.), box_.origin.y),
        size: size(box_.size.width - px(16.), px(CLIP_HEADER)),
    };
    window.with_content_mask(Some(ContentMask { bounds: text }), |window| {
        let at = point(
            box_.origin.x + px(9.),
            box_.origin.y + px(12. - LABEL_SIZE * paint::ASCENT),
        );
        let color = ink(clip.color);
        let name = paint::shape(&clip.label, LABEL_SIZE, FontWeight::NORMAL, color, window);
        let width = name.width;
        name.paint(
            at,
            px(LABEL_SIZE * paint::LINE_HEIGHT),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
        if !clip.summary.is_empty() && clip.summary != clip.label {
            let detail: SharedString = format!(" · {}", clip.summary).into();
            paint::line(
                point(at.x + width, at.y),
                &detail,
                LABEL_SIZE,
                FontWeight::NORMAL,
                fade(color, 0.55).into(),
                window,
                cx,
            );
        }
    });
}

/// A clip's body: everything under the header plate.
pub(super) fn clip_body(box_: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds {
        origin: point(box_.origin.x, box_.origin.y + px(CLIP_HEADER)),
        size: size(box_.size.width, box_.size.height - px(CLIP_HEADER)),
    }
}

/// The drawn width of a selected clip's grab mark. Narrower than [`HANDLE`],
/// which is what the pointer gets: the mark is 6px and the grab is 8px.
const HANDLE_MARK: f32 = 6.;

/// The gap between two grip dots on a grab plate.
const GRIP_SPACING: f32 = 4.;

/// `drawSelectionCursor`: where the next edit lands, over the lane band it
/// covers. A point cursor is a 2px line; a range is a filled rectangle inside
/// a 2px outline.
fn paint_cursor(
    canvas: Bounds<Pixels>,
    layout: Layout,
    cursor: Cursor,
    view: View,
    start: f64,
    end: f64,
    window: &mut Window,
) {
    let (min_row, max_row) = cursor.rows();
    let top = layout.top(min_row);
    let height = (max_row - min_row + 1) as f32 * layout.lane;
    let accent = ladder::accent();

    let Some((from, to)) = cursor.span() else {
        if cursor.start < start || cursor.start > end {
            return;
        }
        window.paint_quad(fill(
            hairline(canvas, view.x_of(cursor.start), top, top + height, 2.),
            accent,
        ));
        return;
    };
    if to < start || from > end {
        return;
    }
    let (left, right) = (view.x_of(from), view.x_of(to));
    let box_ = Bounds {
        origin: point(canvas.origin.x + px(left), canvas.origin.y + px(top)),
        size: size(px(right - left), px(height)),
    };
    window.paint_quad(fill(box_, fade(accent, 0.15)));
    window.paint_quad(quad(
        box_,
        Corners::default(),
        transparent_black(),
        Edges::all(px(2.)),
        accent,
        BorderStyle::Solid,
    ));
}

/// The span the transport is looping: a wash over everything below the ruler,
/// with a line down each bound.
///
/// A colour of its own rather than a value off the grey ladder, and the one
/// place on this canvas that has one: the loop is the only mark here that
/// describes *playback* rather than the score, and a wash in the same family
/// as the clips under it would read as another layer of them.
fn paint_loop(canvas: Bounds<Pixels>, region: (f64, f64), view: View, window: &mut Window) {
    let (from, to) = region;
    let (left, right) = (view.x_of(from), view.x_of(to));
    let width = f32::from(canvas.size.width);
    if right < 0. || left > width {
        return;
    }
    let height = f32::from(canvas.size.height);
    let (clipped_left, clipped_right) = (left.max(0.), right.min(width));
    window.paint_quad(fill(
        Bounds {
            origin: point(
                canvas.origin.x + px(clipped_left),
                canvas.origin.y + px(HEADER_HEIGHT),
            ),
            size: size(px(clipped_right - clipped_left), px(height - HEADER_HEIGHT)),
        },
        fade(LOOP_BAND, 0.12),
    ));
    for edge in [left, right] {
        window.paint_quad(fill(
            hairline(canvas, edge, HEADER_HEIGHT, height, 1.),
            fade(LOOP_BAND, 0.7),
        ));
    }
}

/// The loop band's yellow, `rgb(234 179 8)`. It is not on the grey ladder because nothing else on this canvas needs
/// a hue.
pub(super) const LOOP_BAND: Rgba = Rgba {
    r: 234. / 255.,
    g: 179. / 255.,
    b: 8. / 255.,
    a: 1.,
};

/// Where a right-click's clip would land.
///
/// Add mode fills the target lane and outlines the span; insert mode draws a
/// line along the boundary a new layer would open at — the two are told apart
/// by their shape, because the pointer is in the same place for both.
fn paint_insertion(
    canvas: Bounds<Pixels>,
    layout: Layout,
    menu: InsertMenu,
    view: View,
    window: &mut Window,
) {
    let accent = ladder::accent();
    let left = view.x_of(menu.start);
    let width = (view.x_of(menu.end) - left).max(2.);
    if menu.insert {
        window.paint_quad(fill(
            hairline(
                canvas,
                left,
                layout.top(menu.row) - 1.,
                layout.top(menu.row) + 1.,
                width,
            ),
            accent,
        ));
        return;
    }
    let box_ = Bounds {
        origin: point(
            canvas.origin.x + px(left),
            canvas.origin.y + px(layout.top(menu.row)),
        ),
        size: size(px(width), px(layout.lane)),
    };
    window.paint_quad(fill(box_, fade(accent, 0.1)));
    window.paint_quad(quad(
        box_,
        Corners::default(),
        transparent_black(),
        Edges::all(px(1.)),
        fade(accent, 0.4),
        BorderStyle::Solid,
    ));
}

/// Black on a light plate, white on a dark one. `isLightColor`'s sRGB
/// luminance, the same threshold.
fn ink(on: Rgba) -> Rgba {
    let luminance = 0.299 * on.r + 0.587 * on.g + 0.114 * on.b;
    if luminance > 0.5 {
        rgb(0x000000)
    } else {
        rgb(0xffffff)
    }
}

/// `drawPlayhead`: a full-height line with a pointer at the top.
fn paint_playhead(
    canvas: Bounds<Pixels>,
    scene: &Scene,
    start: f64,
    end: f64,
    window: &mut Window,
) {
    let time = f64::from(scene.playhead);
    if time < start || time > end {
        return;
    }
    let x = scene.view.x_of(time);
    window.paint_quad(fill(
        hairline(canvas, x, 0., f32::from(canvas.size.height), 1.),
        ladder::playhead(),
    ));
    let tip = point(canvas.origin.x + px(x + 0.5), canvas.origin.y);
    let mut head = PathBuilder::fill();
    head.move_to(point(tip.x - px(6.), tip.y));
    head.line_to(point(tip.x + px(6.), tip.y));
    head.line_to(point(tip.x, tip.y + px(8.)));
    head.close();
    if let Ok(head) = head.build() {
        window.paint_path(head, ladder::playhead());
    }
}

/// One line of canvas text, placed by its baseline.
pub(super) fn label(
    canvas: Bounds<Pixels>,
    x: f32,
    baseline: f32,
    text: &SharedString,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    paint::line(
        point(
            canvas.origin.x + px(x),
            canvas.origin.y + px(baseline - LABEL_SIZE * paint::ASCENT),
        ),
        text,
        LABEL_SIZE,
        FontWeight::NORMAL,
        color,
        window,
        cx,
    );
}
