//! Text on a custom-painted canvas.
//!
//! A screen that paints its own surface — the pattern graph, the track
//! timeline — has no element to hang a string on, so it shapes and places the
//! line itself. That is three easy things to get subtly wrong (the family, the
//! run, the leading), and getting them wrong differently on two canvases is how
//! one screen ends up in a different face from the next. So it lives here once,
//! beside [`crate::fonts`], which is what says what the face is.
//!
//! ```ignore
//! use luma_ui::paint;
//!
//! paint::line(at, &title, 12., FontWeight::MEDIUM, ladder::foreground(), window, cx);
//! ```

use gpui::{px, App, FontWeight, Pixels, Point, Rgba, ShapedLine, TextAlign, TextRun, Window};

/// Leading, as a multiple of the font size.
pub const LINE_HEIGHT: f32 = 1.3;

/// Where a line's box starts, given the baseline a canvas 2D context would
/// have drawn it on. `fillText` places the baseline; gpui places the top edge,
/// so a port of canvas coordinates has to walk back up by the ascent.
///
/// One ratio for every size rather than a metric read from the font: the
/// canvas side's own positions are hand-tuned round numbers, so matching the
/// face's exact ascent would be false precision.
pub const ASCENT: f32 = 0.8;

/// Shape one line in the app's face.
///
/// The text system caches layouts, so shaping a label that has not changed is
/// a lookup — which is what makes it affordable to shape a line *before*
/// deciding where to put it (right-aligning against a port, clipping to a clip
/// header).
pub fn shape(
    text: &gpui::SharedString,
    font_size: f32,
    weight: FontWeight,
    color: Rgba,
    window: &Window,
) -> ShapedLine {
    shape_in(crate::fonts::FAMILY, text, font_size, weight, color, window)
}

/// Shape one line in a named family — [`crate::fonts::MONO`] for the numeric
/// readouts the web side sets in `font-mono`. [`shape`] is this in the app's
/// UI face, which is what almost everything wants.
pub fn shape_in(
    family: &'static str,
    text: &gpui::SharedString,
    font_size: f32,
    weight: FontWeight,
    color: Rgba,
    window: &Window,
) -> ShapedLine {
    let mut font = gpui::font(family);
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.clone(), px(font_size), &[run], None)
}

/// Draw one line with its top-left at `at`.
pub fn line(
    at: Point<Pixels>,
    text: &gpui::SharedString,
    font_size: f32,
    weight: FontWeight,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    shape(text, font_size, weight, color, window)
        .paint(
            at,
            px(font_size * LINE_HEIGHT),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
}

/// The width one line will occupy at `tracking` (CSS `letter-spacing`), in
/// pixels. CSS adds the spacing *after* every character, including the last,
/// so a tracked run is that much wider than its shaped advance.
pub fn tracked_width(
    text: &gpui::SharedString,
    font_size: f32,
    weight: FontWeight,
    tracking: f32,
    window: &Window,
) -> f32 {
    let shaped = shape(text, font_size, weight, gpui::rgb(0), window);
    f32::from(shaped.width) + tracking * text.chars().count() as f32
}

/// Draw one line with CSS `letter-spacing`.
///
/// gpui's `TextRun` has no letter-spacing, so the run is shaped and placed one
/// character at a time with `tracking` added to each advance — which is what
/// the browser does to the glyph positions anyway. Only worth it for the
/// screen's tracked styles (the 9px uppercase control face); everything else
/// should use [`line`], which shapes the run once.
// One more argument than [`line`], and the extra one is the whole point.
#[allow(clippy::too_many_arguments)]
pub fn tracked(
    at: Point<Pixels>,
    text: &gpui::SharedString,
    font_size: f32,
    weight: FontWeight,
    color: Rgba,
    tracking: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let mut x = at.x;
    for character in text.chars() {
        let glyph: gpui::SharedString = character.to_string().into();
        let shaped = shape(&glyph, font_size, weight, color, window);
        let advance = shaped.width;
        shaped
            .paint(
                gpui::point(x, at.y),
                px(font_size * LINE_HEIGHT),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
        x += advance + px(tracking);
    }
}
