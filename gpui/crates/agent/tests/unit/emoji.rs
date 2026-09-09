#![cfg(all(feature = "pixel", target_os = "linux"))]

use gpui::{point, px, FontRun, RenderGlyphParams};
use std::borrow::Cow;

#[test]
fn chat_emoji_uses_color_glyphs() {
    let system = gpui_platform::current_platform(true).text_system();
    system
        .add_fonts(vec![Cow::Borrowed(
            include_bytes!("../../../../../harness/fonts/Inter-Regular.ttf").as_slice(),
        )])
        .expect("Inter");
    let font = system
        .font_id(&luma_ui::fonts::font(luma_ui::fonts::FAMILY))
        .expect("font");
    for text in ["👍", "😀", "👍🏽", "👩‍💻", "🇺🇸", "❤️", "☺️"] {
        let line = system.layout_line(
            text,
            px(16.),
            &[FontRun {
                len: text.len(),
                font_id: font,
            }],
        );
        let glyphs: Vec<_> = line.runs.iter().flat_map(|run| &run.glyphs).collect();
        assert_eq!(
            glyphs.len(),
            1,
            "emoji sequence must remain one glyph: {text}"
        );
        assert!(
            glyphs.iter().all(|glyph| glyph.is_emoji),
            "{text}: {glyphs:?}"
        );
        for run in &line.runs {
            for glyph in &run.glyphs {
                let params = RenderGlyphParams {
                    font_id: run.font_id,
                    glyph_id: glyph.id,
                    font_size: px(16.),
                    subpixel_variant: point(0, 0),
                    scale_factor: 1.,
                    is_emoji: glyph.is_emoji,
                    subpixel_rendering: false,
                    dilation: 0,
                };
                let bounds = system.glyph_raster_bounds(&params).expect("glyph bounds");
                let (_, pixels) = system
                    .rasterize_glyph(&params, bounds)
                    .expect("color raster");
                assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0 && (pixel[0] != pixel[1] || pixel[1] != pixel[2])),
                    "{text} rendered without color");
            }
        }
    }
    for text in ["good 123", "❤", "❤︎"] {
        let line = system.layout_line(
            text,
            px(16.),
            &[FontRun {
                len: text.len(),
                font_id: font,
            }],
        );
        assert!(
            line.runs
                .iter()
                .flat_map(|run| &run.glyphs)
                .all(|glyph| !glyph.is_emoji),
            "ordinary text must keep its text font: {text}"
        );
    }
}
