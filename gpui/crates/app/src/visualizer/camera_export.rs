//! "Export camera": the stage's camera, sun and shadow cascades as JSON, and
//! the frame drawn with them as a PNG beside it.
//!
//! The action only asks. The next prepaint records the frame it submits
//! ([`luma_render::camera_export::CameraExport::capture`]) and writes the
//! JSON; the PNG follows when that frame comes back from the renderer, so the
//! picture is the frame the numbers describe.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{div, px, AnyElement};
use luma_render::camera_export::{CameraExport, Context, Orbit, SceneIdentity, Viewport};
use luma_scene::Camera;
use luma_ui::float;
use luma_ui::node::{Instrument as _, Role};

use super::StageFrame;

/// How long the result stays on the stage.
const NOTICE: Duration = Duration::from_secs(6);

/// What the action knows: which room and score the view is of.
pub(super) struct Request {
    pub venue_id: String,
    pub venue_name: String,
    pub score_id: Option<String>,
}

impl Request {
    /// The export context of the frame the prepaint is about to submit.
    pub(super) fn context(
        self,
        playhead_s: f32,
        camera: Camera,
        size: (u32, u32),
        scale_factor: f32,
    ) -> Context {
        Context {
            exported_at: chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
            scene: SceneIdentity {
                venue_id: self.venue_id,
                venue_name: self.venue_name,
                score_id: self.score_id,
                playhead_s,
            },
            viewport: Viewport {
                width: size.0,
                height: size.1,
                scale_factor,
            },
            orbit: Some(Orbit {
                target: camera.target.to_array(),
                radius: camera.radius,
                azimuth: camera.azimuth,
                polar: camera.polar,
            }),
        }
    }
}

/// The export's progress, kept on the stage between frames.
#[derive(Default)]
pub(super) struct Exports {
    /// Asked for, not yet captured.
    pub request: Option<Request>,
    /// The submission serial whose frame becomes the PNG, and its path.
    pub png: Option<(u64, PathBuf)>,
    /// The last result, and when it was shown.
    pub notice: Option<(String, Instant)>,
}

impl Exports {
    /// Write `export` as `camera-<exported_at>.json` in the debug directory,
    /// and owe the PNG of submission `serial`.
    pub(super) fn written(&mut self, serial: u64, export: &CameraExport) {
        let result = debug_dir().and_then(|dir| {
            let path = dir.join(format!("camera-{}.json", export.exported_at));
            export.write(&path).map_err(|error| error.to_string())?;
            Ok(path)
        });
        match result {
            Ok(path) => {
                eprintln!("exported the stage camera to {}", path.display());
                self.png = Some((serial, path.with_extension("png")));
                self.show(format!("Camera exported to {}", path.display()));
            }
            Err(error) => self.show(format!("Could not export the camera: {error}")),
        }
    }

    /// Write the owed PNG when `serial`'s frame (or a later one) arrives.
    pub(super) fn presented(&mut self, serial: u64, frame: &StageFrame, size: (u32, u32)) {
        let Some((_, path)) = self.png.take_if(|(owed, _)| serial >= *owed) else {
            return;
        };
        let bytes = match frame {
            StageFrame::Image(image) => image.as_bytes(0).map(<[u8]>::to_vec).unwrap_or_default(),
            StageFrame::Shared(surface) => surface.to_bytes(),
        };
        let written = luma_render::image_out::rgba8_from_presented(&bytes, size.0, size.1)
            .and_then(|rgba| luma_render::image_out::write(&path, &rgba, size.0, size.1));
        if let Err(error) = written {
            self.show(format!("Could not save the frame: {error}"));
        }
    }

    fn show(&mut self, message: String) {
        self.notice = Some((message, Instant::now()));
    }

    /// The result, while it is fresh.
    pub(super) fn notice(&self) -> Option<AnyElement> {
        let (message, _) = self
            .notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTICE)?;
        Some(
            div()
                .absolute()
                .top(px(16.))
                .left(px(16.))
                .right(px(16.))
                .flex()
                .justify_center()
                .child(float::frosted_card(
                    float::popover_card()
                        .px(px(12.))
                        .py(px(8.))
                        .occlude()
                        .child(float::label(message.clone()))
                        .agent_node(Role::Text, message.clone()),
                ))
                .into_any_element(),
        )
    }
}

/// `<config>/debug`, created.
fn debug_dir() -> Result<PathBuf, String> {
    let dir = crate::library::config_dir()?.path().join("debug");
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir)
}
