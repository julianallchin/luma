//! Cached graph outputs beside the clip transport. Inspecting and paging never
//! edits the score or reevaluates its graph on a playback frame.
use super::*;
use luma_lib::eval::lighting::Inspection;
use luma_patterns::{Channels, Events, Signal, Value};
mod spectrogram;

#[derive(Clone, Default)]
pub(super) struct State {
    open: bool,
    menu: bool,
    output: Option<String>,
    head: usize,
    channel: usize,
    hide_beats: bool,
    images: HashMap<String, Arc<RenderImage>>,
}

impl State {
    pub(super) fn prepare(&mut self, preview: &ClipPreview) {
        self.images.clear();
        if let Ok(Some(data)) = &preview.inspection {
            for (name, result) in &data.spectrograms {
                if let Ok(mel) = result {
                    self.images.insert(name.clone(), spectrogram::image(mel));
                }
            }
        }
    }
}

pub(super) fn controls(view: &View, app: &Entity<Luma>, library: &crate::library::Library) -> Div {
    let Some(scene) = &view.state.scene else {
        return div();
    };
    if matches!(scene.inspection, Ok(None)) {
        return div();
    }
    let target = view.target.clone();
    let toggle = app.clone();
    let state = &view.state.inspection;
    let label = if state.open {
        "Close inspection"
    } else {
        "Inspect signals"
    };
    let mut out = div().flex().flex_col().gap(px(5.)).child(
        luma_ui::button(label, luma_ui::Enabled::Yes)
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                toggle.update(cx, |this, cx| {
                    this.with_preview(&target, |s| {
                        s.inspection.open = !s.inspection.open;
                        s.inspection.menu = false;
                    });
                    cx.notify();
                });
            })
            .agent_node(Role::Button, label),
    );
    if !state.open {
        return out;
    }
    let inspection = match &scene.inspection {
        Ok(Some(data)) => data,
        Err(error) => {
            return out.child(
                div()
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error.clone()),
            )
        }
        Ok(None) => return out,
    };
    let names: Vec<_> = inspection.values.keys().cloned().collect();
    let selected = state
        .output
        .as_ref()
        .filter(|key| inspection.values.contains_key(*key))
        .unwrap_or(&names[0]);
    let labels: Vec<_> = names
        .iter()
        .map(|name| name.strip_prefix("view/").unwrap_or(name))
        .collect();
    let target = view.target.clone();
    let toggle = app.clone();
    let pick_target = target.clone();
    let pick = app.clone();
    let options = names.clone();
    out = out.child(luma_ui::arg::select::luma_arg_select(
        "Graph inspection output",
        selected.strip_prefix("view/").unwrap_or(selected),
        &labels,
        state.menu,
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_preview(&target, |s| s.inspection.menu = !s.inspection.menu);
                cx.notify();
            });
        },
        move |index, _, cx| {
            pick.update(cx, |this, cx| {
                this.with_preview(&pick_target, |s| {
                    s.inspection.output = options.get(index).cloned();
                    s.inspection.menu = false;
                    s.inspection.head = 0;
                    s.inspection.channel = 0;
                });
                cx.notify();
            });
        },
    ));
    let value = &inspection.values[selected];
    let time = view.time(library);
    if let Some(signal) = value.signal() {
        let (heads, _, channels) = signal.values().dim();
        if heads == 0 {
            return out.child("Empty selection");
        }
        let head = state.head.min(heads - 1);
        let channel = state.channel.min(channels - 1);
        let mut row = div().flex().flex_wrap().items_center().gap(px(6.));
        if let Some(ids) = signal.fixtures() {
            row = row.child(
                div()
                    .child(ids[head].clone())
                    .agent_node(Role::Text, format!("Inspect head: {}", ids[head])),
            );
            if heads > 1 {
                row = row
                    .child(page(view, app, "Previous head", false, heads, true))
                    .child(page(view, app, "Next head", true, heads, true));
            }
        } else {
            row = row.child("All heads");
        }
        if channels > 8 {
            row = row
                .child(format!(
                    "Channels {}–{} of {channels}",
                    channel + 1,
                    (channel + 8).min(channels)
                ))
                .child(page(view, app, "Previous channels", false, channels, false))
                .child(page(view, app, "Next channels", true, channels, false));
        }
        let (lines, range) = traces(signal, head, channel, inspection.times.len());
        let legend = (channel..(channel + 8).min(channels)).enumerate().fold(
            div().flex().flex_wrap().gap(px(8.)),
            |row, (index, c)| {
                row.child(
                    div()
                        .text_color(ladder::plot_trace(index))
                        .child(channel_name(signal.channels(), c)),
                )
            },
        );
        let summary = format!(
            "{} samples · {:?} · {:.3} to {:.3}",
            inspection.times.len(),
            signal.unit(),
            range[0],
            range[1]
        );
        out.child(row)
            .child(plot(lines, time, scene.span, PlotKind::Signal))
            .child(legend)
            .child(div().child(summary.clone()).agent_node(Role::Text, summary))
    } else if let Ok(Value::AudioSource(source)) = value.sample(0) {
        out.child(spectrogram::controls(
            view, app, selected, &source, inspection, time, scene.span,
        ))
    } else if let Ok(Value::Events(events)) = value.sample(0) {
        let (lines, caption) = event_lines(&events, inspection, scene.span);
        out.child(plot(lines, time, scene.span, PlotKind::Events))
            .child(div().child(caption.clone()).agent_node(Role::Text, caption))
    } else {
        out.child("No samples")
    }
}

fn page(
    view: &View,
    app: &Entity<Luma>,
    label: &'static str,
    forward: bool,
    count: usize,
    head: bool,
) -> impl IntoElement {
    let target = view.target.clone();
    let app = app.clone();
    luma_ui::button(label, luma_ui::Enabled::Yes)
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            app.update(cx, |this, cx| {
                this.with_preview(&target, |s| {
                    let index = if head {
                        &mut s.inspection.head
                    } else {
                        &mut s.inspection.channel
                    };
                    let step = if head { 1 } else { 8 };
                    let pages = count.div_ceil(step);
                    let page = (*index / step).min(pages - 1);
                    *index = (if forward {
                        (page + 1) % pages
                    } else {
                        (page + pages - 1) % pages
                    }) * step;
                });
                cx.notify();
            });
        })
        .agent_node(Role::Button, label)
}

type Lines = Vec<Vec<[f32; 2]>>;
fn traces(signal: &Signal, head: usize, channel: usize, samples: usize) -> (Lines, [f64; 2]) {
    let values = signal.values();
    let channels = channel..(channel + 8).min(values.dim().2);
    let range = channels
        .clone()
        .flat_map(|c| (0..values.dim().1).map(move |t| values[[head, t, c]]))
        .fold([f64::INFINITY, f64::NEG_INFINITY], |[lo, hi], v| {
            [lo.min(v), hi.max(v)]
        });
    // Valid signals may span the full finite range; normalize before subtracting
    // so plotting opposite large values cannot overflow into NaN coordinates.
    let scale = range[0].abs().max(range[1].abs()).max(f64::MIN_POSITIVE);
    let low = range[0] / scale;
    let height = range[1] / scale - low;
    let lines = channels
        .map(|c| {
            (0..samples)
                .map(|t| {
                    let v = values[[head, if values.dim().1 == 1 { 0 } else { t }, c]];
                    [
                        t as f32 / (samples - 1).max(1) as f32,
                        if range[0] == range[1] {
                            0.5
                        } else {
                            ((v / scale - low) / height) as f32
                        },
                    ]
                })
                .collect()
        })
        .collect();
    (lines, range)
}

fn channel_name(channels: &Channels, channel: usize) -> String {
    match channels {
        Channels::Rgb => ["Red", "Green", "Blue"][channel].into(),
        Channels::PanTilt => ["Pan", "Tilt"][channel].into(),
        Channels::Value => "Value".into(),
        _ => format!("Channel {}", channel + 1),
    }
}

fn event_lines(events: &Events, data: &Inspection, span: (f32, f32)) -> (Lines, String) {
    let mut source = events;
    let mut heads = None;
    while let Events::Targeted { events, targets } = source {
        heads.get_or_insert(targets.fixtures().len());
        source = events;
    }
    let (Ok(start), Ok(end)) = (
        data.clock.beat_at(f64::from(span.0)),
        data.clock.beat_at(f64::from(span.1)),
    ) else {
        return (vec![], "Invalid event timeline".into());
    };
    const LIMIT: usize = 4096;
    let (beats, count): (Vec<_>, _) = match source {
        Events::Beats { times } => {
            let times = times.as_slice();
            let a = times.partition_point(|t| *t < start);
            let b = times.partition_point(|t| *t < end);
            (times[a..b].iter().take(LIMIT).copied().collect(), b - a)
        }
        Events::Periodic {
            repeat,
            grid_aligned,
            delay,
        } => {
            let origin = if *grid_aligned { 0. } else { data.clip_start } + delay;
            let first = ((start - origin) / repeat).ceil();
            let count = (((end - origin) / repeat).ceil() - first).max(0.) as usize;
            (
                (0..count.min(LIMIT))
                    .map(|i| origin + (first + i as f64) * repeat)
                    .collect(),
                count,
            )
        }
        _ => {
            return (
                vec![],
                "Automatic events resolve at the consuming node".into(),
            )
        }
    };
    let lines = beats
        .into_iter()
        .filter_map(|beat| {
            let time = data.clock.seconds_at(beat).ok()? as f32;
            let x = (time - span.0) / (span.1 - span.0);
            Some(vec![[x, 0.15], [x, 0.85]])
        })
        .collect();
    let domain = heads.map_or_else(
        || "global schedule".into(),
        |n| format!("schedule targeting {n} heads"),
    );
    let suffix = if count > LIMIT {
        format!(" · first {LIMIT} shown")
    } else {
        String::new()
    };
    (lines, format!("{count} events · {domain}{suffix}"))
}

enum PlotKind {
    Signal,
    Events,
}

fn plot(lines: Lines, time: f32, span: (f32, f32), kind: PlotKind) -> impl IntoElement {
    let (label, events) = match kind {
        PlotKind::Signal => ("Graph signal plot", false),
        PlotKind::Events => ("Graph event plot", true),
    };
    div()
        .h(px(100.))
        .w_full()
        .bg(ladder::background())
        .rounded(px(4.))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let at = |p: [f32; 2]| {
                        point(
                            bounds.left() + bounds.size.width * p[0],
                            bounds.bottom() - bounds.size.height * (0.05 + 0.9 * p[1]),
                        )
                    };
                    for (index, line) in lines.iter().enumerate() {
                        let Some((first, rest)) = line.split_first() else {
                            continue;
                        };
                        let mut path = PathBuilder::stroke(px(1.5));
                        path.move_to(at(*first));
                        for point in rest {
                            path.line_to(at(*point));
                        }
                        if let Ok(path) = path.build() {
                            window.paint_path(
                                path,
                                ladder::plot_trace(if events { 0 } else { index }),
                            );
                        }
                    }
                    let x = ((time - span.0) / (span.1 - span.0)).clamp(0., 1.);
                    let mut cursor = PathBuilder::stroke(px(1.));
                    cursor.move_to(at([x, 0.]));
                    cursor.line_to(at([x, 1.]));
                    if let Ok(path) = cursor.build() {
                        window.paint_path(path, ladder::foreground_alpha(0.6));
                    }
                },
            )
            .size_full(),
        )
        .agent_node(Role::Card, label)
}

#[cfg(test)]
mod tests {
    use super::{event_lines, traces, Events, Inspection, Signal};

    #[test]
    fn graph_inspection_normalizes_large_and_constant_values_without_invalid_coordinates() {
        let signal =
            Signal::vector(vec![-f64::MAX, f64::MAX], luma_patterns::Unit::Number).unwrap();
        let (lines, range) = traces(&signal, 0, 0, 128);
        assert_eq!(range, [-f64::MAX, f64::MAX]);
        assert!(lines[0].iter().all(|point| point[1] == 0.));
        assert!(lines[1].iter().all(|point| point[1] == 1.));
        let (lines, _) = traces(
            &Signal::scalar(0.25, luma_patterns::Unit::Proportion).unwrap(),
            0,
            0,
            128,
        );
        assert!(lines[0].iter().all(|point| point[1] == 0.5));
    }

    #[test]
    fn graph_inspection_event_ticks_follow_tempo_clip_origin_and_half_open_bounds() {
        let data = Inspection {
            times: vec![],
            clock: luma_patterns::BeatTimeline::new(vec![0., 0.5, 1., 1.5, 2., 3., 4., 5., 6.], 0.)
                .unwrap(),
            clip_start: 4.,
            values: Default::default(),
            spectrograms: Default::default(),
        };
        let events = Events::Beats {
            times: luma_patterns::EventTimes::new(vec![3.99, 4., 5., 7., 8.]).unwrap(),
        };
        let (lines, label) = event_lines(&events, &data, (2., 6.));
        assert_eq!(label, "3 events · global schedule");
        assert_eq!(
            lines.iter().map(|line| line[0][0]).collect::<Vec<_>>(),
            vec![0., 0.25, 0.75]
        );
        let periodic = Events::Periodic {
            repeat: 1.,
            grid_aligned: false,
            delay: 0.5,
        };
        let (lines, _) = event_lines(&periodic, &data, (2., 6.));
        assert_eq!(
            lines.iter().map(|line| line[0][0]).collect::<Vec<_>>(),
            vec![0.125, 0.375, 0.625, 0.875]
        );
        let periodic = Events::Periodic {
            repeat: 3.,
            grid_aligned: true,
            delay: 0.5,
        };
        let (lines, _) = event_lines(&periodic, &data, (2., 6.));
        assert_eq!(
            lines.iter().map(|line| line[0][0]).collect::<Vec<_>>(),
            vec![0.625]
        );
    }
}
