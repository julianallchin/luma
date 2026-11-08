//! The corner FPS readout and the frame-stats panel it opens, read from the
//! hitch ring and the renderer's retired-frame rate.
use super::*;

/// One 60 Hz frame — the graph's hairline, and the bound a clean frame sits
/// under.
const FRAME_BUDGET_MS: f32 = 1_000.0 / 60.0;

/// Bars in the frame-time graph — about two seconds at 60 Hz.
const GRAPH_BARS: usize = 120;

/// The window the headline rate is averaged over.
const FPS_WINDOW_MS: f32 = 1_000.0;

/// What the presented-frame stream has been doing lately, read from the hitch
/// ring — the one place several seconds of frames already live, so the readout
/// costs no second record.
struct FpsReading {
    /// Delivered rate over the trailing second, when anything arrived.
    fps: Option<f32>,
    /// The rate the renderer worker finished frames at over the same second,
    /// shown or dropped as stale. Above `fps`, the renderer makes frames the
    /// screen never shows; level with it, the pace is set before the renderer.
    rendered: Option<f32>,
    /// The worst delivered interval in the ring's whole window. The rate alone
    /// averages away exactly the frames the eye catches; this is the dip.
    low_ms: Option<f32>,
    /// Every delivered interval in the window, oldest first, for the graph.
    intervals: Vec<f32>,
}

fn fps_reading(stage: &Stage) -> FpsReading {
    let intervals: Vec<f32> = stage
        .hitches
        .recent()
        .filter(|sample| sample.delivered && sample.interval_ms > 0.0)
        .map(|sample| sample.interval_ms)
        .collect();
    let low_ms = intervals.iter().copied().reduce(f32::max);
    let mut sum = 0.0;
    let mut frames = 0u32;
    for ms in intervals.iter().rev() {
        sum += ms;
        frames += 1;
        if sum >= FPS_WINDOW_MS {
            break;
        }
    }
    FpsReading {
        fps: (sum > 0.0).then(|| frames as f32 * 1_000.0 / sum),
        rendered: stage.rendered.rate(),
        low_ms,
        intervals,
    }
}

impl Visualizer {
    /// Show or hide the corner frame rate (the `stage_frame_rate` setting).
    /// Whether that changed anything.
    pub(crate) fn show_frame_rate(&mut self, shown: bool) -> bool {
        let changed = self.render_controls.frame_rate_shown != shown;
        self.render_controls.frame_rate_shown = shown;
        changed
    }
}

/// The renderer worker's retired-frame count, sampled at each submission over
/// the readout's trailing window.
///
/// [`luma_render::AsyncViewport::finished`] counts every frame the worker
/// finished, whether it reached the screen or was dropped as stale. The hitch
/// ring records that count but no clock, so the rate needs this pairing of the
/// two. A few dozen entries at most: one per prepaint for one second.
#[derive(Default)]
pub(super) struct RenderedRate {
    samples: std::collections::VecDeque<(Instant, u64)>,
}

impl RenderedRate {
    pub(super) fn record(&mut self, at: Instant, finished: u64) {
        // A new viewport counts from zero again.
        if self
            .samples
            .back()
            .is_some_and(|&(_, last)| finished < last)
        {
            self.samples.clear();
        }
        self.samples.push_back((at, finished));
        // Keep the newest sample at or before the window's start, so that once
        // a window's worth has passed the span covers all of it.
        let window = Duration::from_secs_f32(FPS_WINDOW_MS / 1_000.0);
        while self
            .samples
            .get(1)
            .is_some_and(|&(second, _)| at.duration_since(second) >= window)
        {
            self.samples.pop_front();
        }
    }

    /// Frames finished per second across the samples held.
    fn rate(&self) -> Option<f32> {
        let (&(from, first), &(to, last)) = (self.samples.front()?, self.samples.back()?);
        let span = to.duration_since(from).as_secs_f32();
        (span > 0.0).then(|| (last - first) as f32 / span)
    }
}

/// A reading on a floating surface: the value bright, its unit or name after
/// it in the label's darker ink.
fn stat_value(value: impl Into<gpui::SharedString>) -> Div {
    div()
        .flex_none()
        .text_color(ladder::foreground())
        .child(value.into())
}

/// The corner FPS readout, and the frame-stats panel it opens.
///
/// This is the stats' one home: two surfaces for one reading would drift
/// apart. The readout is the delivered rate beside the rendered one; the panel
/// adds the worst recent frame, the frame-time graph and the per-phase numbers
/// the hitch ring already records. Each panel line publishes the label the
/// measuring scripts have always read (`Draw `, `UI `, `Present `, `CPU `).
///
/// It is part of the stage's own element, so it is rebuilt with every stage
/// frame while the cached panes around the stage are not.
pub(super) fn fps_overlay(state: &Visualizer, app: &Entity<Luma>) -> Option<AnyElement> {
    if !matches!(state.status, Status::Live) || !state.render_controls.frame_rate_shown {
        return None;
    }
    let open = state.fps_panel.is_open();
    let closing = state.fps_panel.exit();
    let (resting, reading) = {
        let stage = state.stage.borrow();
        (stage.resting, fps_reading(&stage))
    };
    let rate = |value: Option<f32>| value.map_or_else(|| "—".to_string(), |v| format!("{v:.0}"));
    // A resting stage is not rendering slowly, it is not rendering at all —
    // a number here, stale or zero, would read as one or the other.
    let summary: Vec<AnyElement> = if resting {
        vec![stat_value("Idle")
            .agent_node(Role::Text, "FPS IDLE")
            .into_any_element()]
    } else {
        let fps = rate(reading.fps);
        vec![
            stat_value(fps.clone())
                .agent_node(Role::Text, format!("FPS {fps}"))
                .into_any_element(),
            luma_ui::float::label("fps ·").into_any_element(),
            stat_value(rate(reading.rendered)).into_any_element(),
            luma_ui::float::label("rendered").into_any_element(),
        ]
    };
    let panel = (open || closing.is_some()).then(|| {
        let stage = state.stage.borrow();
        stats_panel(&stage, reading)
    });
    let toggle = {
        let app = app.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.fps_panel.toggle();
                }
                this.notify_stage(cx);
            });
        }
    };
    let trigger = luma_ui::button("", luma_ui::Enabled::Yes)
        .id("fps-overlay")
        .h(CORNER_CONTROL)
        .gap(px(4.))
        // Opaque over the viewport, so the pointer plane it covers is its own
        // (see [`listen`]).
        .occlude()
        .children(summary)
        .on_click(toggle)
        .agent_node(Role::Toggle, "Frame stats");
    let id = "frame-stats";
    let trigger_height = f32::from(CORNER_CONTROL);
    Some(
        div()
            .absolute()
            .top(CORNER_INSET)
            .left(CORNER_INSET)
            .child(
                div()
                    .relative()
                    .child(trigger)
                    .children(panel.map(|content| match closing {
                        Some(t) => {
                            luma_ui::float::anchored_below_closing(id, trigger_height, content, t)
                        }
                        // The readout's own toggle closes it: a stats panel is
                        // watched while the camera moves, so a press on the
                        // stage must not dismiss it.
                        None => luma_ui::float::anchored_below(
                            id,
                            trigger_height,
                            luma_ui::float::Dismiss::Never,
                            content,
                        ),
                    })),
            )
            .into_any_element(),
    )
}

/// The frame-stats panel: the frame-time graph over one line per reading.
fn stats_panel(stage: &Stage, reading: FpsReading) -> AnyElement {
    let work = stage.last_work;
    let ms = |value: Option<f32>, digits: usize| {
        value.map_or_else(|| "—".to_string(), |v| format!("{v:.digits$} ms"))
    };
    let line = |name: &'static str, value: Div, detail: Option<String>| {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .child(
                div()
                    .w(px(STAT_NAME_WIDTH))
                    .child(luma_ui::float::label(name)),
            )
            .child(value)
            .children(detail.map(luma_ui::float::label))
    };
    let dipped = reading.low_ms.is_some_and(|ms| ms >= HITCH_MS);
    let low = reading
        .low_ms
        .map(|ms| format!("{:.0} fps", 1_000.0 / ms.max(1.0)));
    let rendered = reading.rendered.map(|fps| format!("{fps:.0} fps"));
    let present = stage
        .last_present
        .map(|present| format!("{:.1} / {:.1} ms", present.p50_ms, present.p95_ms));
    let timings = match (stage.last_cpu_ms, stage.last_gpu_ms, stage.last_cluster_ms) {
        (Some(cpu), Some(gpu), Some(cluster)) => {
            format!("CPU {cpu:.2} · GPU {gpu:.2} · cluster {cluster:.2} ms")
        }
        _ => "CPU/GPU timing unavailable".to_string(),
    };
    let shadows = stage.last_shadow_maps.unwrap_or(0);
    luma_ui::float::popover_card()
        .w(px(240.))
        .p(px(12.))
        .gap(px(6.))
        .child(frame_graph(reading.intervals))
        .child(luma_ui::float::divider())
        .child(
            line(
                "Low",
                stat_value(low.clone().unwrap_or_else(|| "—".to_string()))
                    .when(dipped, |value| value.text_color(ladder::status_bad())),
                None,
            )
            .agent_node(
                Role::Text,
                format!("Low {}", low.unwrap_or_else(|| "—".to_string())),
            ),
        )
        .child(
            line(
                "Rendered",
                stat_value(rendered.clone().unwrap_or_else(|| "—".to_string())),
                None,
            )
            .agent_node(
                Role::Text,
                format!("Rendered {}", rendered.unwrap_or_else(|| "—".to_string())),
            ),
        )
        .child(
            line("Draw", stat_value(ms(stage.last_draw_ms, 1)), None).agent_node(
                Role::Text,
                stage
                    .last_draw_ms
                    .map_or_else(|| "Draw —".to_string(), |ms| format!("Draw {ms:.1} ms")),
            ),
        )
        .child(
            line(
                "UI",
                stat_value(ms(Some(work.total_ms()), 1)),
                Some(format!(
                    "sample {:.1} · build {:.1} · pick {:.1}",
                    work.sample_ms, work.build_ms, work.pick_ms
                )),
            )
            .agent_node(
                Role::Text,
                format!(
                    "UI {:.1} (sample {:.1} build {:.1} pick {:.1}) ms",
                    work.total_ms(),
                    work.sample_ms,
                    work.build_ms,
                    work.pick_ms
                ),
            ),
        )
        .child(
            line(
                "Present",
                stat_value(present.clone().unwrap_or_else(|| "—".to_string())),
                present.is_some().then(|| "p50 / p95".to_string()),
            )
            .agent_node(
                Role::Text,
                stage.last_present.map_or_else(
                    || "Present —".to_string(),
                    |present| format!("Present {:.1}/{:.1} ms", present.p50_ms, present.p95_ms),
                ),
            ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(line("CPU", stat_value(ms(stage.last_cpu_ms, 2)), None))
                .child(line("GPU", stat_value(ms(stage.last_gpu_ms, 2)), None))
                .child(line(
                    "Cluster",
                    stat_value(ms(stage.last_cluster_ms, 2)),
                    None,
                ))
                .agent_node(Role::Text, timings),
        )
        .child(
            line(
                "Shadows",
                stat_value(shadows.to_string()),
                Some("redrawn".to_string()),
            )
            .agent_node(Role::Text, format!("Shadows {shadows} redrawn")),
        )
        .into_any_element()
}

/// The width of the name column in the frame-stats panel, so the values line
/// up down the panel.
const STAT_NAME_WIDTH: f32 = 60.0;

/// Delivered frame intervals as bars, newest at the right.
///
/// The y scale is pinned to the hitch threshold rather than the data's own
/// maximum, so a dip reads at the same height in every capture and a graph
/// with no dips does not stretch its jitter to fill the box. A frame over one
/// missed vsync warns, over the hitch threshold it is the failure colour —
/// hue for meaning, as the ladder allows.
fn frame_graph(intervals: Vec<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            window.paint_quad(
                gpui::fill(bounds, luma_ui::glass::wash(luma_ui::glass::WASH_REST))
                    .corner_radii(px(luma_ui::radius::CONTROL)),
            );
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let budget_y = height * (1.0 - FRAME_BUDGET_MS / HITCH_MS);
            window.paint_quad(gpui::fill(
                Bounds {
                    origin: bounds.origin + Point::new(px(0.), px(budget_y)),
                    size: gpui::Size {
                        width: bounds.size.width,
                        height: px(1.),
                    },
                },
                ladder::foreground_alpha(0.15),
            ));
            let bar = width / GRAPH_BARS as f32;
            for (slot, ms) in intervals.iter().rev().take(GRAPH_BARS).enumerate() {
                let fraction = (ms / HITCH_MS).clamp(0.04, 1.0);
                let color: gpui::Hsla = if *ms >= HITCH_MS {
                    ladder::status_bad().into()
                } else if *ms > FRAME_BUDGET_MS * 1.5 {
                    ladder::status_warn().into()
                } else {
                    ladder::foreground_alpha(0.35)
                };
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: bounds.origin
                            + Point::new(
                                px(width - (slot + 1) as f32 * bar),
                                px(height * (1.0 - fraction)),
                            ),
                        size: gpui::Size {
                            width: px((bar - 1.0).max(1.0)),
                            height: px(height * fraction),
                        },
                    },
                    color,
                ));
            }
        },
    )
    .w_full()
    .h(px(36.))
}

#[cfg(test)]
mod rendered_rate_tests {
    use super::RenderedRate;
    use std::time::{Duration, Instant};

    /// Record `frames` retirements `every` apart, continuing from `count`.
    fn run(rate: &mut RenderedRate, at: &mut Instant, count: &mut u64, frames: u64, every: u64) {
        for _ in 0..frames {
            *at += Duration::from_millis(every);
            *count += 1;
            rate.record(*at, *count);
        }
    }

    /// The rate is the trailing window's, so a slowdown shows once it has
    /// lasted a window, rather than being averaged into everything before it.
    #[test]
    fn the_rate_follows_the_trailing_window() {
        let (mut rate, mut at, mut count) = (RenderedRate::default(), Instant::now(), 0);
        rate.record(at, count);
        run(&mut rate, &mut at, &mut count, 200, 5);
        let fast = rate.rate().unwrap();
        run(&mut rate, &mut at, &mut count, 75, 20);
        let slow = rate.rate().unwrap();
        assert!(slow * 2.0 < fast, "fast {fast}, then slow {slow}");
        // Held samples span the window and not much more.
        let (first, last) = (
            rate.samples.front().unwrap().0,
            rate.samples.back().unwrap().0,
        );
        assert!(last.duration_since(first) < Duration::from_millis(1_100));
    }

    /// A new viewport counts from zero. That is a restart, not a frame count
    /// going backwards.
    #[test]
    fn a_restarted_count_starts_a_new_window() {
        let (mut rate, mut at, mut count) = (RenderedRate::default(), Instant::now(), 0);
        run(&mut rate, &mut at, &mut count, 50, 10);
        let before = rate.rate().unwrap();
        count = 0;
        rate.record(at, count);
        assert_eq!(rate.rate(), None);
        run(&mut rate, &mut at, &mut count, 50, 10);
        let after = rate.rate().unwrap();
        assert!(
            (after - before).abs() < before * 0.05,
            "{before} then {after}"
        );
    }
}
