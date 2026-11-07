//! Clip body previews: decoding the seam's heatmaps and aim curves, painting
//! them, and the coalesced re-render after an edit.

use super::*;

/// A seam preview, decoded once for painting: a clip's body, a browser
/// thumbnail and a carried preset's ghost all hold one and draw it with
/// [`Preview::paint`]. Cheap to clone.
#[derive(Clone)]
pub(super) enum Preview {
    /// A heatmap, baked for the GPU — see [`bake`]. Two frames under one
    /// identity — frame 0 at [`BODY_ALPHA`], frame 1 opaque — so selecting
    /// a clip picks a frame instead of a second image.
    Heatmap(Arc<RenderImage>),
    /// An aim clip's pan and tilt curves — see [`paint_aim`].
    Aim(Arc<AimCurves>),
}

impl Preview {
    /// A seam row, baked under a fresh identity. `None` for a heatmap that is
    /// not `width * height` of RGBA: a seam drift this painter cannot draw,
    /// and skipping it leaves the flat fill, which is already what a missing
    /// preview means.
    pub(super) fn decode(row: AnnotationPreview) -> Option<Self> {
        if let Some(curves) = row.aim {
            return Some(Self::Aim(Arc::new(curves)));
        }
        if row.width == 0
            || row.height == 0
            || row.pixels.len() != (row.width * row.height * 4) as usize
        {
            return None;
        }
        Some(Self::Heatmap(Arc::new(bake(
            row.width,
            row.height,
            &row.pixels,
        ))))
    }

    /// Stretch the heatmap over `body`, or stroke the curves across it, as
    /// the resting body (`selected` false) or the opaque one. `corners`
    /// rounds the picture to a frame it sits in.
    ///
    /// Answers whether it painted, so the caller can put the flat fill down
    /// when there is nothing to draw — a body too narrow to read (under 8px
    /// the heatmap is noise), or too short for the curves.
    ///
    /// One `paint_image` of the whole body, whatever the zoom: the GPU
    /// stretches the grid, a cell a block. A clip that runs off the canvas is masked
    /// by the lane band around this call, not trimmed here.
    pub(super) fn paint(
        &self,
        body: Bounds<Pixels>,
        selected: bool,
        corners: Corners<Pixels>,
        window: &mut Window,
    ) -> bool {
        if f32::from(body.size.width) < 8. || f32::from(body.size.height) <= 0. {
            return false;
        }
        match self {
            Self::Aim(curves) => paint_aim(body, curves, selected, corners, window),
            Self::Heatmap(image) => {
                window
                    .paint_image(
                        body,
                        body,
                        corners,
                        Arc::clone(image),
                        usize::from(selected),
                        false,
                    )
                    .ok();
                true
            }
        }
    }
}

/// A clip's [`Preview`] in the timeline's map.
pub(super) struct Installed {
    pub(super) preview: Preview,
    /// Whether the atlas has been told about a heatmap's bytes. The
    /// identity is the clip's for as long as it has a preview: a re-render
    /// refreshes one atlas tile instead of abandoning a trail of them —
    /// `STAGE_IMAGE_ID`'s trick in `visualizer.rs`, one per clip. False for
    /// every fresh bake, so the painter refreshes the tile under the kept
    /// identity before drawing it.
    pub(super) published: bool,
}

/// The translucency of an unselected clip's body. It lets the beat grid,
/// painted first, read through both the flat fill and the heatmap; selection
/// goes opaque.
pub(super) const BODY_ALPHA: f32 = 0.75;

/// A clip's [`Preview`] over its body — see [`Preview::paint`] — after
/// telling the atlas about a fresh bake under the clip's kept identity.
pub(super) fn paint_preview(
    body: Bounds<Pixels>,
    clip: &Clip,
    previews: &RefCell<HashMap<SharedString, Installed>>,
    selected: bool,
    window: &mut Window,
) -> bool {
    let mut previews = previews.borrow_mut();
    let Some(installed) = previews.get_mut(&clip.id) else {
        return false;
    };
    if let (Preview::Heatmap(image), false) = (&installed.preview, installed.published) {
        // A re-render lands under the clip's kept atlas identity, and the
        // atlas answers paints from its cache, so it has to be told: same-size
        // tiles are refreshed in place, resized ones evicted for the paint
        // to re-insert.
        window.update_image(image).ok();
        installed.published = true;
    }
    installed
        .preview
        .paint(body, selected, Corners::default(), window)
}

/// The least body height that holds both of an aim clip's bands.
const AIM_BODY: f32 = 12.;

/// Stroke an aim clip's curves across its body: pan in the top band, tilt in
/// the bottom, each in its own trace colour on the dark ground. Both bands
/// are 0–1 from the seam, bottom to top, so zoom only changes where the
/// points land. Answers false for a body too short for two bands.
///
/// A curve keeps about one point per 2px of body: a zoomed-out clip strokes
/// a few dozen points, and a zoomed-in one every sample the seam sent.
fn paint_aim(
    body: Bounds<Pixels>,
    curves: &AimCurves,
    selected: bool,
    corners: Corners<Pixels>,
    window: &mut Window,
) -> bool {
    if f32::from(body.size.height) < AIM_BODY {
        return false;
    }
    let alpha = if selected { 1. } else { BODY_ALPHA };
    window.paint_quad(fill(body, fade(ladder::background(), alpha)).corner_radii(corners));
    let half = body.size.height / 2.;
    // The line between the bands.
    window.paint_quad(fill(
        Bounds {
            origin: point(body.origin.x, body.origin.y + half),
            size: size(body.size.width, px(1.)),
        },
        fade(ladder::foreground(), 0.12),
    ));
    let width = f32::from(body.size.width);
    for (top, band, color) in [
        (body.origin.y, &curves.pan, ladder::aim_pan()),
        (body.origin.y + half, &curves.tilt, ladder::aim_tilt()),
    ] {
        // 3px of air above and below each band, for the stroke's width.
        let (top, height) = (top + px(3.), half - px(6.));
        let mut path = PathBuilder::stroke(px(1.5));
        for curve in band.iter().filter(|curve| curve.len() > 1) {
            let last = curve.len() - 1;
            let stride = ((last as f32 / (width / 2.).max(1.)).ceil() as usize).max(1);
            let at = |i: usize| {
                point(
                    body.origin.x + body.size.width * (i as f32 / last as f32),
                    top + height * (1. - curve[i].clamp(0., 1.)),
                )
            };
            path.move_to(at(0));
            for i in (stride..last).step_by(stride).chain([last]) {
                path.line_to(at(i));
            }
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, fade(color, alpha));
        }
    }
    true
}

/// A seam heatmap as the two-frame [`RenderImage`] the painter draws: a texel
/// a cell, sampled nearest so every cell is a hard block at any zoom, frame 0
/// at [`BODY_ALPHA`], frame 1 opaque, RGBA swapped to the BGRA the sprite
/// atlas stores.
fn bake(width: u32, height: u32, pixels: &[u8]) -> RenderImage {
    let mut opaque = pixels.to_vec();
    for pixel in opaque.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let mut body = opaque.clone();
    for pixel in body.chunks_exact_mut(4) {
        pixel[3] = (f32::from(pixel[3]) * BODY_ALPHA) as u8;
    }
    let frame = |bytes: Vec<u8>| {
        image::Frame::new(
            image::RgbaImage::from_raw(width, height, bytes).expect("sized as width * height * 4"),
        )
    };
    let mut image = RenderImage::new(vec![frame(body), frame(opaque)]);
    image.nearest = true;
    image
}

impl Editor {
    /// Adopt one clip's re-rendered preview — `preview_annotation`'s answer.
    fn install_preview(&mut self, id: SharedString, preview: Option<Preview>) {
        let mut previews = self.previews.borrow_mut();
        let identity = match previews.remove(&id) {
            Some(Installed {
                preview: Preview::Heatmap(image),
                ..
            }) => Some(image.id),
            _ => None,
        };
        if let Some(mut preview) = preview {
            // A re-render keeps the clip's atlas identity, so the atlas
            // refreshes the tile in place.
            if let (Preview::Heatmap(image), Some(identity)) = (&mut preview, identity) {
                Arc::get_mut(image).expect("a fresh bake").id = identity;
            }
            previews.insert(
                id,
                Installed {
                    preview,
                    published: false,
                },
            );
        }
    }
}

impl Luma {
    /// Re-render clips' heatmaps from the working copy's rows.
    ///
    /// Coalesced per clip: an edit
    /// landing while this clip's render is in flight queues one trailing
    /// re-issue instead of a backlog, and because the render reads the clip's
    /// *current* state when it is issued, that one trailing render covers
    /// everything the burst did.
    pub(super) fn refresh_clip_previews(
        &mut self,
        ids: impl IntoIterator<Item = SharedString>,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.workspace.active().cloned() else {
            return;
        };
        self.refresh_clip_previews_for(target, ids, cx);
    }

    /// One call for the whole batch: the venue, the beat grid and each
    /// selection's cells are read once however many clips ask.
    pub(super) fn refresh_clip_previews_for(
        &mut self,
        target: Target,
        ids: impl IntoIterator<Item = SharedString>,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::TrackEditor(state)) = self.workspace.body_mut(&target) else {
            return;
        };
        let mut wanted = Vec::new();
        for id in ids {
            if state.preview_inflight.contains(&id) {
                state.preview_queued.insert(id);
            } else if state.clips.iter().any(|clip| clip.id == id) {
                // A clip deleted since the edit that asked has no body to preview.
                wanted.push(id);
            }
        }
        if wanted.is_empty() {
            return;
        }
        let score_id = state.score.id.clone();
        let candidate = match state.graph_candidate() {
            Ok(score) => score,
            Err(error) => {
                for id in wanted {
                    state.preview_errors.insert(id, error.clone());
                }
                return;
            }
        };
        state.preview_inflight.extend(wanted.iter().cloned());
        let pending = self.library.preview_score_clips(
            &score_id,
            wanted.iter().map(ToString::to_string).collect(),
            candidate,
        );
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(Body::TrackEditor(editor)) =
                    this.venues.body_mut(&mut this.workspace, &target)
                else {
                    return;
                };
                let shown = editor.preview_errors.is_empty();
                let mut again = Vec::new();
                let mut landed =
                    |editor: &mut Editor,
                     id: SharedString,
                     preview: Result<AnnotationPreview, String>| {
                        editor.preview_inflight.remove(&id);
                        if editor.preview_queued.remove(&id) {
                            again.push(id.clone());
                        }
                        if !editor.clips.iter().any(|clip| clip.id == id) {
                            return;
                        }
                        match preview {
                            Ok(row) => {
                                editor.preview_errors.remove(&id);
                                editor.install_preview(id, Preview::decode(row));
                            }
                            Err(error) => {
                                editor.previews.borrow_mut().remove(&id);
                                editor
                                    .preview_errors
                                    .insert(id, format!("Preview: {error}"));
                            }
                        }
                    };
                match result {
                    Ok(strips) => {
                        for strip in strips {
                            landed(editor, strip.clip_id.into(), strip.preview);
                        }
                    }
                    Err(error) => {
                        for id in wanted {
                            landed(editor, id, Err(error.to_string()));
                        }
                    }
                }
                // Pictures land on the timeline alone; an error reads out on
                // the toolbar, which is the tab's too.
                match editor.playback_surface.clone() {
                    Some(surface) if shown && editor.preview_errors.is_empty() => {
                        surface.update(cx, |_, cx| cx.notify())
                    }
                    _ => cx.notify(),
                }
                this.refresh_clip_previews_for(target, again, cx);
            })
            .ok();
        })
        .detach();
    }
}
