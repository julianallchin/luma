use super::*;
use luma_lib::audio::melspec::Spectrogram;

/// Convert the column-major frequency data to the atlas's BGRA image once.
/// Low frequencies sit at the bottom of the view.
pub(super) fn image(mel: &Spectrogram) -> Arc<RenderImage> {
    let mut pixels = Vec::with_capacity(mel.width * mel.height * 4);
    for row in (0..mel.height).rev() {
        for column in 0..mel.width {
            let [r, g, b] = color(mel.data[column * mel.height + row]);
            pixels.extend_from_slice(&[b, g, r, 255]);
        }
    }
    let image = image::RgbaImage::from_raw(mel.width as u32, mel.height as u32, pixels)
        .expect("spectrogram pixels match their dimensions");
    Arc::new(RenderImage::new([image::Frame::new(image)]))
}

fn color(value: f32) -> [u8; 3] {
    let colors = [
        [0., 0., 0.],
        [36., 18., 77.],
        [128., 38., 89.],
        [225., 91., 54.],
        [252., 253., 191.],
    ];
    let position = value.clamp(0., 1.) * (colors.len() - 1) as f32;
    let index = (position.floor() as usize).min(colors.len() - 2);
    let t = position - index as f32;
    std::array::from_fn(|c| {
        (colors[index][c] + t * (colors[index + 1][c] - colors[index][c])).round() as u8
    })
}

pub(super) fn controls(
    view: &View,
    app: &Entity<Luma>,
    name: &str,
    source: &luma_patterns::AudioInput,
    data: &Inspection,
    time: f32,
    span: (f32, f32),
) -> Div {
    let Some(result) = data.spectrograms.get(name) else {
        return div().child("Preparing spectrogram…");
    };
    let mel = match result {
        Ok(mel) => mel,
        Err(error) => {
            return div().child(
                div()
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error.clone()),
            )
        }
    };
    let Some(image) = view.state.inspection.images.get(name).cloned() else {
        return div().child("Spectrogram unavailable");
    };
    let hide = view.state.inspection.hide_beats;
    let label = if hide { "Show beats" } else { "Hide beats" };
    let target = view.target.clone();
    let toggle = app.clone();
    let filters = source
        .filters()
        .iter()
        .map(|filter| match filter {
            luma_patterns::AudioFilter::Lowpass { cutoff_hz } => format!("Lowpass {cutoff_hz} Hz"),
            luma_patterns::AudioFilter::Highpass { cutoff_hz } => {
                format!("Highpass {cutoff_hz} Hz")
            }
        })
        .collect::<Vec<_>>();
    let caption = format!(
        "{} audio{} · Mel frequency {:.0}–{:.0} Hz",
        source.name(),
        if filters.is_empty() {
            String::new()
        } else {
            format!(" · {}", filters.join(" → "))
        },
        mel.frequencies_hz.first().copied().unwrap_or(0.),
        mel.frequencies_hz.last().copied().unwrap_or(0.)
    );
    let clock = data.clock.clone();
    let left = (mel.span.0 - span.0) / (span.1 - span.0);
    let width = (mel.span.1 - mel.span.0) / (span.1 - span.0);
    div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(
                    luma_ui::button(label, luma_ui::Enabled::Yes)
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            toggle.update(cx, |this, cx| {
                                this.with_preview(&target, |s| {
                                    s.inspection.hide_beats = !s.inspection.hide_beats
                                });
                                cx.notify();
                            });
                        })
                        .agent_node(Role::Button, label),
                )
                .child(div().child(caption.clone()).agent_node(Role::Text, caption)),
        )
        .child(
            div()
                .relative()
                .overflow_hidden()
                .w_full()
                .h(px(128.))
                .bg(ladder::background())
                .child(
                    div()
                        .absolute()
                        .left(relative(left))
                        .top(px(0.))
                        .w(relative(width))
                        .h_full()
                        .child(img(image).object_fit(ObjectFit::Fill).size_full()),
                )
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let x = |time: f32| {
                                bounds.left()
                                    + bounds.size.width * ((time - span.0) / (span.1 - span.0))
                            };
                            if !hide {
                                let beats = clock.timestamps();
                                let a = beats.partition_point(|t| *t < f64::from(span.0));
                                let b = beats.partition_point(|t| *t < f64::from(span.1));
                                let mut grid = PathBuilder::stroke(px(1.));
                                for beat in
                                    beats[a..b].iter().step_by((b - a).div_ceil(4096).max(1))
                                {
                                    grid.move_to(point(x(*beat as f32), bounds.top()));
                                    grid.line_to(point(x(*beat as f32), bounds.bottom()));
                                }
                                if let Ok(path) = grid.build() {
                                    window.paint_path(path, ladder::foreground_alpha(0.25));
                                }
                            }
                            let mut cursor = PathBuilder::stroke(px(1.5));
                            cursor.move_to(point(x(time.clamp(span.0, span.1)), bounds.top()));
                            cursor.line_to(point(x(time.clamp(span.0, span.1)), bounds.bottom()));
                            if let Ok(path) = cursor.build() {
                                window.paint_path(path, ladder::foreground());
                            }
                        },
                    )
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .size_full(),
                )
                .agent_node(Role::Card, "Graph spectrogram"),
        )
}

#[cfg(test)]
mod tests {
    use super::{image, Spectrogram};

    #[test]
    fn spectrogram_pixels_put_low_frequencies_below_high_and_use_atlas_bgra() {
        let mel = Spectrogram {
            width: 2,
            height: 2,
            data: vec![0., 1., 0.5, 0.25],
            span: (0., 1.),
            frequencies_hz: vec![100., 1000.],
        };
        let image = image(&mel);
        assert_eq!(
            image.as_bytes(0).unwrap(),
            &[191, 253, 252, 255, 77, 18, 36, 255, 0, 0, 0, 255, 89, 38, 128, 255]
        );
    }
}
