//! The shared offscreen renderer and image surface for picker dialogs.
use gpui::SharedString;
use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Corners, Hitbox, HitboxBehavior, ImageId,
    Pixels, RenderImage,
};
use luma_lib::stage_render::{self, Sequence};
use luma_ui::{
    ladder,
    node::{agent_paint_node, Instrument, Role},
};
use std::{collections::BTreeMap, sync::Arc};
static PREVIEW_IMAGE_ID: std::sync::OnceLock<ImageId> = std::sync::OnceLock::new();

pub(crate) fn install(
    rig: &crate::library::Rig,
    settings: Option<luma_render::scene_desc::RenderSettings>,
    size: (u32, u32),
) -> Result<Arc<Sequence>, String> {
    if rig.is_empty() {
        return Err("This venue has nothing patched".into());
    }
    let definitions: BTreeMap<_, _> = rig
        .definitions
        .iter()
        .map(|(path, def)| (path.clone(), stage_render::definition(def)))
        .collect();
    let mut scene = crate::visualizer::scene(rig, &definitions, rig.environment);
    if let Some(settings) = settings {
        scene.render = settings;
    }
    Sequence::install(
        scene,
        definitions,
        stage_render::meshes_root(None),
        luma_scene::View::Front,
        None,
        size,
    )
    .map(Arc::new)
}

/// Wrap a readback as the image gpui paints.
///
/// The offscreen renderer writes RGBA — the order a PNG wants — and
/// `RenderImage` reads its buffer as **BGRA**, so the two channels are
/// exchanged here. Swapping in place rather than through the renderer because
/// the byte order is this consumer's business and every other caller of
/// `Sequence` is encoding a PNG.
pub(crate) fn image_from_rgba(
    mut rgba: Vec<u8>,
    width: u32,
    height: u32,
) -> Result<RenderImage, String> {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "the frame was not width * height * 4 bytes".to_string())?;
    let mut image = RenderImage::new([image::Frame::new(buffer)]);
    image.id = *PREVIEW_IMAGE_ID.get_or_init(|| image.id);
    Ok(image)
}

pub(crate) fn image(
    label: &'static str,
    image: Option<&Arc<RenderImage>>,
    error: Option<&String>,
) -> AnyElement {
    let body: AnyElement = match (image, error) {
        (_, Some(error)) => centered(error.clone()),
        (Some(image), None) => {
            let image = Arc::clone(image);
            canvas(
                move |bounds: Bounds<Pixels>, window, cx| {
                    agent_paint_node(Role::Card, label, bounds, window, cx);
                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                },
                move |bounds, _: Hitbox, window, _| {
                    // New pixels under a fixed identity, so the atlas has to be
                    // told — see `PREVIEW_IMAGE_ID`.
                    window.update_image(&image).ok();
                    let pixels = image.size(0);
                    let scale = (f32::from(bounds.size.width) / (pixels.width.0 as f32))
                        .min(f32::from(bounds.size.height) / (pixels.height.0 as f32));
                    let size = gpui::size(
                        px((pixels.width.0 as f32) * scale),
                        px((pixels.height.0 as f32) * scale),
                    );
                    let fitted = Bounds::new(
                        bounds.center() - gpui::point(size.width / 2., size.height / 2.),
                        size,
                    );
                    window
                        .paint_image(
                            fitted,
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
        (None, None) => centered("Lighting the room…".to_string()),
    };
    body
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
