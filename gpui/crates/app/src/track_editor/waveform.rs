//! GPU waveform integration. Audio is loaded once; navigation only renders views.
use super::*;
use luma_render::{
    device::DeviceContext,
    waveform::{View as GpuView, Waveform},
};

/// Diagnostic isolation: retain the first strip while playback and layout keep
/// running. This changes the displayed waveform and is never a quality mode.
pub(super) fn updates_frozen() -> bool {
    static FROZEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FROZEN.get_or_init(|| std::env::var("LUMA_WAVEFORM_FREEZE").as_deref() == Ok("1"))
}

#[derive(Default)]
pub(super) struct Resource {
    gpu: Option<Arc<Waveform>>,
    loading: bool,
    pub error: Option<String>,
}

pub(super) enum Image {
    Surface(luma_render::Surface),
    Pixels(Arc<gpui::RenderImage>),
}

pub(super) struct Painted {
    pub camera: Option<(View, f32)>,
    view: GpuView,
    image: Image,
}

#[derive(Default)]
pub(super) struct Strip {
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    pub frame: Option<Rc<Painted>>,
    pending: bool,
    failed: Option<GpuView>,
    pub error: Option<String>,
}

impl Editor {
    fn waveform_strip(&self, overview: bool) -> &Strip {
        if overview {
            &self.overview_waveform
        } else {
            &self.timeline_waveform
        }
    }
    fn waveform_strip_mut(&mut self, overview: bool) -> &mut Strip {
        if overview {
            &mut self.overview_waveform
        } else {
            &mut self.timeline_waveform
        }
    }
}

pub(super) fn prepaint(
    app: &Entity<Luma>,
    overview: bool,
    bounds: Bounds<Pixels>,
    window: &Window,
    cx: &mut App,
) {
    let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
        return;
    };
    let strip = editor.waveform_strip(overview);
    strip.bounds.set(bounds);
    if updates_frozen() && strip.frame.is_some() {
        return;
    }
    let target = Target::TrackEditor {
        track: editor.track_id.clone(),
        venue: editor.venue_id.clone(),
    };
    let identity = editor.gpu_waveform.clone();
    let resource = identity.borrow();
    if resource.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()) {
        drop(resource);
        let app = app.clone();
        cx.defer(move |cx| {
            app.update(cx, |this, cx| {
                this.edit_waveform_tab(&target, cx, |editor| {
                    editor.gpu_waveform = Rc::new(RefCell::new(Resource::default()));
                    editor.timeline_waveform.pending = false;
                    editor.overview_waveform.pending = false;
                    editor.timeline_waveform.frame = None;
                    editor.overview_waveform.frame = None;
                    editor.timeline_waveform.failed = None;
                    editor.overview_waveform.failed = None;
                });
            })
        });
        return;
    }
    let Some(gpu) = resource.gpu.clone() else {
        if resource.loading || resource.error.is_some() || editor.waveform.is_none() {
            return;
        }
        // Headless Linux has no compositor device. It can still exercise the
        // editor's geometry; renderer tests exercise the GPU independently.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let Some(gpui::WgpuDevice {
                device,
                queue,
                adapter,
                lost,
            }) = window.wgpu_device()
            else {
                return;
            };
            DeviceContext::adopt(device, queue, adapter, lost);
        }
        drop(resource);
        identity.borrow_mut().loading = true;
        let request = app.read(cx).library.track_waveform_signal(&editor.track_id);
        let app = app.clone();
        cx.spawn(async move |cx| {
            let result = match request.await {
                Ok(signal) => {
                    cx.background_spawn(async move {
                        let context = DeviceContext::shared().map_err(|e| e.to_string())?;
                        Waveform::new(
                            context,
                            &signal.bands,
                            [signal.gains.low, signal.gains.mid, signal.gains.high],
                            signal.ceilings,
                            signal.sample_rate,
                        )
                        .map(Arc::new)
                        .map_err(|e| e.to_string())
                    })
                    .await
                }
                Err(error) => Err(error.to_string()),
            };
            app.update(cx, |this, cx| {
                this.edit_waveform_tab(&target, cx, |editor| {
                    if !Rc::ptr_eq(&identity, &editor.gpu_waveform) {
                        return;
                    }
                    let mut resource = identity.borrow_mut();
                    resource.loading = false;
                    match result {
                        Ok(gpu) => resource.gpu = Some(gpu),
                        Err(error) => resource.error = Some(format!("Waveform: {error}")),
                    }
                })
            });
        })
        .detach();
        return;
    };
    drop(resource);
    let scale = window.scale_factor();
    let width = (f32::from(bounds.size.width) * scale).ceil().max(1.) as u32;
    let duration = editor.waveform.as_ref().map_or(0., |w| w.duration_seconds);
    if duration <= 0. {
        return;
    }
    let margin = (editor.view.zoom * scale * 0.1).ceil().clamp(64., 2048.) as u32;
    let color = |c: Rgba| [c.r, c.g, c.b, c.a];
    let camera = (!overview && editor.follow && editor.transport.playing)
        .then_some((editor.view, editor.transport.position));
    let desired = GpuView {
        start: if overview {
            0.
        } else if camera.is_some() {
            editor.view.time_at(0.)
        } else {
            // A fixed physical-pixel lattice keeps sampling phase unchanged
            // while following. Margin covers motion during background rendering.
            let pixel = (f64::from(editor.view.scroll) * f64::from(scale)).floor();
            (pixel - f64::from(margin)).max(0.) / (f64::from(editor.view.zoom) * f64::from(scale))
        },
        seconds_per_pixel: if overview {
            duration / f64::from(width)
        } else {
            1. / (f64::from(editor.view.zoom) * f64::from(scale))
        },
        width: if overview || camera.is_some() {
            width
        } else {
            width + 2 * margin
        },
        height: ((if overview { 40. } else { WAVEFORM_HEIGHT }) * scale)
            .round()
            .max(1.) as u32,
        padding: 8. * scale,
        colors: [
            color(ladder::card()),
            color(ladder::waveform_low()),
            color(ladder::waveform_mid()),
            color(ladder::waveform_high()),
        ],
    };
    if strip.pending
        || strip.failed == Some(desired)
        || strip.frame.as_ref().is_some_and(|frame| {
            if overview || camera.is_some() {
                return frame.view == desired;
            }
            let old = frame.view;
            let left = editor.view.time_at(0.);
            let right = left + f64::from(width) * desired.seconds_per_pixel;
            let reserve = f64::from(margin) * 0.25 * desired.seconds_per_pixel;
            old.seconds_per_pixel == desired.seconds_per_pixel
                && old.height == desired.height
                && old.colors == desired.colors
                && old.start <= (left - reserve).max(0.)
                && old.start + f64::from(old.width) * old.seconds_per_pixel >= right + reserve
        })
    {
        return;
    }
    let app = app.clone();
    cx.defer(move |cx| {
        app.update(cx, |this, cx| {
            let Some(Body::TrackEditor(editor)) = this.workspace.body_mut(&target) else {
                return;
            };
            if !Rc::ptr_eq(&identity, &editor.gpu_waveform) {
                return;
            }
            let strip = editor.waveform_strip_mut(overview);
            if strip.pending {
                return;
            }
            strip.pending = true;
            let queued = std::time::Instant::now();
            let work = cx.background_spawn(async move {
                let started = std::time::Instant::now();
                let result = (|| -> Result<(Image, luma_render::waveform::CpuTimings), String> {
                    let frame = gpu.render(desired).map_err(|e| e.to_string())?;
                    let timings = frame.cpu_timings();
                    match frame.surface() {
                        Some(surface) => Ok((Image::Surface(surface), timings)),
                        None => {
                            let mut pixels = frame.pixels().map_err(|e| e.to_string())?;
                            for pixel in pixels.chunks_exact_mut(4) {
                                pixel.swap(0, 2);
                            }
                            let buffer =
                                image::RgbaImage::from_raw(desired.width, desired.height, pixels)
                                    .ok_or("Invalid waveform image")?;
                            Ok((
                                Image::Pixels(Arc::new(gpui::RenderImage::new([
                                    image::Frame::new(buffer),
                                ]))),
                                timings,
                            ))
                        }
                    }
                })();
                (result, started, std::time::Instant::now())
            });
            cx.spawn(async move |this, cx| {
                let (result, started, finished) = work.await;
                trace::waveform(
                    overview,
                    queued,
                    started,
                    finished,
                    result.as_ref().ok().map(|(_, timings)| *timings),
                );
                this.update(cx, |this, cx| {
                    this.edit_waveform_tab(&target, cx, |editor| {
                        if !Rc::ptr_eq(&identity, &editor.gpu_waveform) {
                            return;
                        }
                        let strip = editor.waveform_strip_mut(overview);
                        strip.pending = false;
                        match result {
                            Ok((image, _)) => {
                                strip.frame = Some(Rc::new(Painted {
                                    camera,
                                    view: desired,
                                    image,
                                }));
                                strip.failed = None;
                                strip.error = None;
                            }
                            Err(error) => {
                                strip.failed = Some(desired);
                                strip.error = Some(format!("Waveform: {error}"));
                            }
                        }
                    })
                })
                .ok();
            })
            .detach();
        });
    });
}

pub(super) fn paint(
    bounds: Bounds<Pixels>,
    frame: Option<&Painted>,
    view: Option<View>,
    window: &mut Window,
) {
    window.paint_quad(fill(bounds, ladder::card()));
    let Some(frame) = frame else {
        return;
    };
    let scale = window.scale_factor();
    let mut target = bounds;
    target.size = size(
        px(frame.view.width as f32 / scale),
        px(frame.view.height as f32 / scale),
    );
    // Keep the completed waveform visible while a zoom replacement renders.
    // Transform its time extent into the current view; the replacement returns
    // to native pixel size as soon as it is published.
    let mut native_width = true;
    if let Some(view) = view {
        let step = 1. / (f64::from(view.zoom) * f64::from(scale));
        let ratio = frame.view.seconds_per_pixel / step;
        native_width = (ratio - 1.).abs() <= 0.00001;
        target.origin.x += px(((frame.view.start - view.time_at(0.)) / step) as f32 / scale);
        if !native_width {
            target.size.width = px((f64::from(frame.view.width) * ratio / f64::from(scale)) as f32);
        }
    }
    if frame.camera.is_some() && native_width {
        target.origin.x = px((f32::from(target.origin.x) * scale).round() / scale);
    }
    target.origin.y = px((f32::from(target.origin.y) * scale).round() / scale);
    window.with_content_mask(Some(ContentMask { bounds }), |window| match &frame.image {
        Image::Surface(surface) => window.paint_surface(target, surface.source()),
        Image::Pixels(image) => {
            let _ = window.paint_image(target, bounds, Default::default(), image.clone(), 0, false);
        }
    });
}
