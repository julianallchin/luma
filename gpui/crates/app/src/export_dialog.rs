//! The "Export show" dialog: choose a size and a file, then watch the score
//! render. The rendering is [`crate::visualizer::show_export`]'s; this is
//! only the card over it.

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{div, px, relative, AnyElement, Context, Entity, SharedString};

use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{float, glass, ladder};

use crate::shell::{Body, Overlay};
use crate::visualizer::show_export::{self, Export, Job};
use crate::Luma;

/// The frame an export fits the viewport's shape into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    Hd,
    Qhd,
    Uhd,
}

impl Resolution {
    const ALL: [Self; 3] = [Self::Hd, Self::Qhd, Self::Uhd];

    fn label(self) -> &'static str {
        match self {
            Self::Hd => "1080p",
            Self::Qhd => "1440p",
            Self::Uhd => "4K",
        }
    }

    fn frame(self) -> (u32, u32) {
        match self {
            Self::Hd => (1920, 1080),
            Self::Qhd => (2560, 1440),
            Self::Uhd => (3840, 2160),
        }
    }
}

enum Phase {
    Setup { error: Option<String> },
    Running(Export),
    Done { path: PathBuf },
    Failed(String),
}

/// The dialog's state, in [`Overlay::ShowExport`].
pub(crate) struct ExportDialog {
    track_name: String,
    /// The score's length, in seconds.
    duration: f32,
    audio: PathBuf,
    resolution: Resolution,
    output: PathBuf,
    /// The viewport's width over its height, when the dialog opened. Start
    /// reads it again.
    aspect: f32,
    phase: Phase,
}

impl ExportDialog {
    /// Whether a render is in flight. Closing the dialog would cancel it, so
    /// only its own Cancel button may.
    pub(crate) fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Running(_))
    }

    fn size(&self) -> (u32, u32) {
        show_export::fit(self.resolution.frame(), self.aspect)
    }
}

/// `~/Videos/<name>.mp4` (`~/Movies` on macOS), with the characters a file
/// name cannot hold replaced.
fn default_output(name: &str) -> PathBuf {
    let folder = if cfg!(target_os = "macos") {
        "Movies"
    } else {
        "Videos"
    };
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim();
    let stem = if stem.is_empty() { "Show" } else { stem };
    std::env::home_dir()
        .unwrap_or_default()
        .join(folder)
        .join(format!("{stem}.mp4"))
}

impl Luma {
    /// Whether a show export is rendering, so the stage should stand aside.
    pub(crate) fn exporting_show(&self) -> bool {
        matches!(self.overlay.as_open(), Some(Overlay::ShowExport(dialog)) if dialog.is_running())
    }

    /// Raise the dialog for the score open in the active tab.
    pub(crate) fn open_show_export(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        let Some((track_name, duration, audio)) = editor.export_source() else {
            return;
        };
        let aspect = self
            .visualizer
            .as_ref()
            .and_then(|stage| stage.shot())
            .map_or(16.0 / 9.0, |shot| shot.aspect);
        self.overlay
            .open(Overlay::ShowExport(Box::new(ExportDialog {
                output: default_output(&track_name),
                track_name,
                duration,
                audio,
                resolution: Resolution::Uhd,
                aspect,
                phase: Phase::Setup { error: None },
            })));
        cx.notify();
    }

    fn show_export_mut(&mut self) -> Option<&mut ExportDialog> {
        match self.overlay.open_mut() {
            Some(Overlay::ShowExport(dialog)) => Some(dialog),
            _ => None,
        }
    }

    fn set_export_resolution(&mut self, resolution: Resolution, cx: &mut Context<Self>) {
        if let Some(dialog) = self.show_export_mut() {
            dialog.resolution = resolution;
            cx.notify();
        }
    }

    /// The platform's save dialog, starting where the file would go now.
    fn choose_export_file(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.show_export_mut() else {
            return;
        };
        let folder = dialog
            .output
            .parent()
            .filter(|folder| folder.is_dir())
            .map_or_else(
                || std::env::home_dir().unwrap_or_default(),
                Path::to_path_buf,
            );
        let name = dialog
            .output
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let chosen = cx.prompt_for_new_path(&folder, name.as_deref());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some(dialog) = this.show_export_mut() {
                    dialog.output = if path.extension().is_some() {
                        path
                    } else {
                        path.with_extension("mp4")
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Snapshot the stage and start rendering.
    fn start_show_export(&mut self, cx: &mut Context<Self>) {
        let shot = self
            .visualizer
            .as_ref()
            .filter(|stage| stage.is_lit())
            .and_then(|stage| stage.shot());
        let sample = self.library.score_sampler();
        let Some(dialog) = self.show_export_mut() else {
            return;
        };
        if dialog.is_running() {
            return;
        }
        let Some(shot) = shot else {
            dialog.phase = Phase::Setup {
                error: Some("The stage has not lit this score yet. Try again in a moment.".into()),
            };
            cx.notify();
            return;
        };
        if let Some(folder) = dialog.output.parent() {
            if let Err(error) = std::fs::create_dir_all(folder) {
                dialog.phase = Phase::Setup {
                    error: Some(format!("Could not create {}: {error}", folder.display())),
                };
                cx.notify();
                return;
            }
        }
        dialog.aspect = shot.aspect;
        let job = Job {
            size: dialog.size(),
            shot,
            duration: dialog.duration,
            sample: Box::new(sample),
            audio: Some(dialog.audio.clone()).filter(|audio| audio.is_file()),
            output: dialog.output.clone(),
        };
        let export = Export::start(job);
        let mut updates = export.updates();
        dialog.phase = Phase::Running(export);
        cx.notify();
        cx.spawn(async move |this, cx| {
            // Ends when the export finishes, or when the dialog lets it go
            // and the thread drops its sender.
            while updates.changed().await.is_ok() {
                let running = this.update(cx, |this, cx| {
                    let Some(dialog) = this.show_export_mut() else {
                        return false;
                    };
                    let Phase::Running(export) = &dialog.phase else {
                        return false;
                    };
                    match export.progress().outcome {
                        None => {}
                        Some(Ok(())) => {
                            dialog.phase = Phase::Done {
                                path: dialog.output.clone(),
                            }
                        }
                        Some(Err(error)) => dialog.phase = Phase::Failed(error),
                    }
                    cx.notify();
                    dialog.is_running()
                });
                if !matches!(running, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    /// Stop the render. Dropping the export kills ffmpeg and deletes the
    /// partial file.
    fn cancel_show_export(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.show_export_mut() {
            dialog.phase = Phase::Setup { error: None };
            cx.notify();
        }
    }
}

/// `h:mm:ss`, or `m:ss` under an hour.
fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn text(content: impl Into<SharedString>, size: f32, alpha: f32) -> impl IntoElement {
    let content = content.into();
    div()
        .text_size(px(size))
        .text_color(glass::ink(alpha))
        .child(content.clone())
        .agent_node(Role::Text, content)
}

fn button(
    label: &'static str,
    id: &'static str,
    primary: bool,
    app: &Entity<Luma>,
    act: fn(&mut Luma, &mut Context<Luma>),
) -> AnyElement {
    let app = app.clone();
    let shape = if primary {
        float::btn_primary(label)
    } else {
        float::btn(label, id)
    };
    shape
        .id(id)
        .on_click(move |_, _, cx| app.update(cx, |this, cx| act(this, cx)))
        .agent_node(Role::Button, label)
        .into_any_element()
}

fn actions(buttons: Vec<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap(px(8.))
        .children(buttons)
}

/// The card.
pub(crate) fn render(state: &ExportDialog, app: &Entity<Luma>) -> AnyElement {
    let heading = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(text("Export show", 15., 0.95))
        .child(text(state.track_name.clone(), 13., 0.6));
    let content = match &state.phase {
        Phase::Setup { error } => setup(state, error.as_deref(), app),
        Phase::Running(export) => running(export, app),
        Phase::Done { path } => {
            let reveal = app.clone();
            let path = path.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(16.))
                .child(text(format!("Saved to {}", path.display()), 13., 0.8))
                .child(actions(vec![
                    float::btn("Show in folder", "show-export-reveal")
                        .id("show-export-reveal")
                        .on_click(move |_, _, cx| {
                            reveal.update(cx, |_, cx| cx.reveal_path(&path));
                        })
                        .agent_node(Role::Button, "Show in folder")
                        .into_any_element(),
                    button("Close", "show-export-close", true, app, Luma::close_overlay),
                ]))
        }
        Phase::Failed(error) => div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error.clone()),
            )
            .child(actions(vec![button(
                "Close",
                "show-export-close",
                true,
                app,
                Luma::close_overlay,
            )])),
    };
    let body = div()
        .size_full()
        .flex()
        .flex_col()
        .justify_between()
        .p(px(20.))
        .gap(px(16.))
        .child(heading)
        .child(content);
    morph::fixed_card(
        "Export show dialog",
        MorphSize::new(WIDTH, HEIGHT),
        body.into_any_element(),
    )
}

fn setup(state: &ExportDialog, error: Option<&str>, app: &Entity<Luma>) -> gpui::Div {
    let mut sizes = float::segmented();
    for resolution in Resolution::ALL {
        let label = resolution.label();
        let chosen = resolution == state.resolution;
        let app = app.clone();
        sizes = sizes.child(
            float::segment(label, chosen, label)
                .id(label)
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| this.set_export_resolution(resolution, cx));
                })
                .agent_node(Role::Toggle, label)
                .agent_focused(chosen),
        );
    }
    let (width, height) = state.size();
    let seconds = show_export::frame_count(state.duration) as f64 / f64::from(show_export::FPS);
    let file = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .w_full()
        .child(
            float::field()
                .flex_1()
                .min_w_0()
                .child(div().truncate().child(state.output.display().to_string()))
                .agent_node(Role::Text, state.output.display().to_string()),
        )
        .child(button(
            "Choose…",
            "show-export-choose",
            false,
            app,
            Luma::choose_export_file,
        ));
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(float::field_row(
            "Resolution",
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .child(sizes)
                .child(luma_ui::caption(format!("{width} × {height}"))),
        ))
        .child(float::field_row("File", file).items_stretch())
        .child(luma_ui::caption(format!(
            "{} at {} fps, from the stage's current view",
            clock(seconds),
            show_export::FPS
        )))
        .when_some(error, |el, error| {
            el.child(float::error_row(error.to_string()))
        })
        .child(actions(vec![
            button(
                "Cancel",
                "show-export-dismiss",
                false,
                app,
                Luma::close_overlay,
            ),
            button(
                "Start",
                "show-export-start",
                true,
                app,
                Luma::start_show_export,
            ),
        ]))
}

fn running(export: &Export, app: &Entity<Luma>) -> gpui::Div {
    let progress = export.progress();
    let elapsed = progress.started.elapsed().as_secs_f64();
    let fraction = if progress.total == 0 {
        0.0
    } else {
        progress.done as f32 / progress.total as f32
    };
    let fps = progress.done as f64 / elapsed.max(1e-3);
    let left = if progress.done == 0 {
        "—".to_string()
    } else {
        clock((progress.total - progress.done) as f64 / fps)
    };
    let bar = div()
        .w_full()
        .h(px(6.))
        .rounded(px(3.))
        .bg(glass::ink(0.08))
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded(px(3.))
                .bg(ladder::foreground()),
        );
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(bar)
        .child(text(
            format!("{} / {} frames", progress.done, progress.total),
            13.,
            0.8,
        ))
        .child(luma_ui::caption(format!(
            "{} elapsed · {fps:.0} fps · {left} left",
            clock(elapsed)
        )))
        .child(actions(vec![button(
            "Cancel",
            "show-export-cancel",
            false,
            app,
            Luma::cancel_show_export,
        )]))
}

const WIDTH: f32 = 460.0;
const HEIGHT: f32 = 320.0;
