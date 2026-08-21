//! The waveform looks the same on both sides of the resolution threshold.
//!
//! `track_editor_waveform.rs` proves the editor *switches* source past the
//! stored envelope's density. This proves the switch does not show. Two shots,
//! one wheel notch apart, either side of the crossing: the palette either side
//! has to be the same three band colours over the same bed, because the seam
//! returns the same three envelopes in the same units whichever source answers
//! and `paint_waveform` has one drawing routine for them.
//!
//! It is a *colour and structure* test, not a golden image. The two shots are
//! at slightly different zooms — the threshold is crossed by zooming, so they
//! cannot be at the same one — and the extra detail the deeper shot resolves is
//! measured in `dispatch::handlers::waveforms`' seam tests, where it is a
//! number rather than a picture. What a picture can settle, and nothing else
//! can, is that the *rendering* did not change: the old peak-and-RMS hull drew
//! a translucent accent outline round a solid accent core, so its palette had
//! none of these colours in it and this test would have failed on the shot
//! above the threshold.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test track_editor_waveform_pixels
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use image::RgbaImage;
use serde_json::Value;
use support::{Clip, Fixture};

/// Three minutes, so `FULL_WAVEFORM_SIZE / 180` is 167 buckets a second and the
/// threshold sits inside the wheel's range — the same track
/// `track_editor_waveform.rs` uses, for the same reason.
const TRACK_SECONDS: u32 = 180;

/// One wheel notch. `View::ZOOM_PER_PIXEL` is 0.002, so this scales the zoom by
/// `exp(0.08)` — about 8%, which is fine enough that the two shots are
/// recognisably the same view and coarse enough to cross in a dozen notches.
const NOTCH: i32 = 40;

/// One clip, only so the track shows up under the venue: a venue lists the
/// tracks its scores reference, and a track with no score on it is not in the
/// venue to be opened. Nothing here reads the clip.
fn harness() -> Harness {
    Fixture::new(
        "track-editor-waveform-pixels",
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 11.5, 12.5)],
    )
    .open(Mode::Pixel)
}

/// Open the track, walk the zoom in one notch at a time, and shoot the waveform
/// strip on the last notch before `FINE` appears and the first notch after.
///
/// The readout is the oracle for where the threshold is: it is computed from
/// the same `drawn_buckets` the canvas asks before choosing its source, so
/// "the notch before FINE" is exactly "the last notch drawn from the stored
/// envelope" and no arithmetic here has to agree with the editor's.
const SCRIPT: &str = r#"
    function fine() {
        return app.snapshot().findAll({ role: "text" })
            .map((n) => n.label)
            .find((label) => label.startsWith("FINE ")) ?? null;
    }

    function waveform() {
        return app.snapshot().find({ role: "card", label: "Waveform" });
    }

    function settle(check, limit) {
        for (let i = 0; i < limit; i++) {
            const value = check();
            if (value) return value;
            app.frames(1, { waitMs: 60 });
        }
        return null;
    }

    function notch() {
        app.scroll(waveform(), { dy: NOTCH, steps: 1, modifiers: ["platform"] });
        app.frames(8, { waitMs: 60 });
    }

    // Pixel mode lays out with real glyph metrics on a real device, so the
    // first frames take long enough that a fixed frame count can click at
    // nothing. Every step waits for what it is about to press.
    const venue = settle(() => app.snapshot().find({ role: "card", label: "Test Venue" }), 200);
    if (venue === null) throw new Error("the venue list never appeared");
    app.click(venue);
    const track = settle(() => app.snapshot().find({ role: "row", label: "Aurora" }), 200);
    if (track === null) throw new Error("the track list never appeared");
    app.click(track);

    // Three minutes of audio to decode an envelope for, on a runtime gpui does
    // not own — waited for by its result rather than by a frame count.
    if (settle(waveform, 200) === null) throw new Error("the editor never drew a waveform");
    app.frames(10, { waitMs: 60 });
    if (fine() !== null) throw new Error("the opening zoom is already past the threshold");

    // The last frame still drawn from the stored envelope, kept as we go: one
    // notch later we may already be over, and stepping back would be a second
    // way of naming the same zoom.
    let below = app.screenshot({ node: waveform() });
    let crossed = false;
    for (let i = 0; i < 40; i++) {
        notch();
        if (settle(fine, 40) !== null) { crossed = true; break; }
        below = app.screenshot({ node: waveform() });
    }
    if (!crossed) throw new Error("the wheel never reached the resolution threshold");

    ({ below, above: app.screenshot({ node: waveform() }), buckets: fine() })
"#;

#[test]
fn crossing_the_resolution_threshold_does_not_change_how_the_waveform_is_drawn() {
    let mut harness = harness();
    let result = harness.exec(
        &SCRIPT.replace("NOTCH", &NOTCH.to_string()),
        Duration::from_secs(600),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let below = keep(&out["below"], "below-threshold");
    let above = keep(&out["above"], "above-threshold");

    // Every colour worth a percent of the strip, either side. The bed and the
    // three band bars are opaque fills, so these are exact quad colours and not
    // blends — which is what makes an equality assertion honest here.
    let (below_palette, below_ink) = palette(&below.1);
    let (above_palette, above_ink) = palette(&above.1);

    for (name, palette) in [("below", &below_palette), ("above", &above_palette)] {
        for (band, color) in bands() {
            let share = palette.get(&color).copied().unwrap_or(0.);
            assert!(
                share > 0.01,
                "{name} the threshold, the {band} band is missing from the waveform \
                 (it covers {share:.4} of the strip)\n  below: {}\n  above: {}",
                below.0.display(),
                above.0.display(),
            );
        }
    }

    let (mut below_colors, mut above_colors): (Vec<_>, Vec<_>) = (
        below_palette.keys().copied().collect(),
        above_palette.keys().copied().collect(),
    );
    below_colors.sort_unstable();
    above_colors.sort_unstable();
    assert_eq!(
        below_colors,
        above_colors,
        "the waveform is drawn in different colours either side of the threshold\n  \
         below: {}\n  above: {}",
        below.0.display(),
        above.0.display(),
    );

    // Same silhouette, not just the same crayons: a rendering that swapped a
    // band stack for a hull would keep none of these proportions. The bound is
    // loose because the two shots are a wheel notch apart and the strip does
    // show slightly different audio.
    let drift = (above_ink - below_ink).abs() / below_ink;
    assert!(
        drift < 0.25,
        "the waveform covers {below_ink:.3} of the strip below the threshold and \
         {above_ink:.3} above it — that is a different picture, not more detail\n  \
         below: {}\n  above: {}",
        below.0.display(),
        above.0.display(),
    );

    // No gaps and no floor-to-ceiling blocks, either side. Both are what a
    // waveform drawn by two systems looks like: a bucket walk that misses a
    // pixel column leaves a bar out, and a hull path that escaped the band
    // stack paints the full height of the strip in one colour.
    for (name, shot) in [("below", &below), ("above", &above)] {
        let structure = structure(&shot.1);
        assert!(
            structure.gaps.is_empty(),
            "{name} the threshold, the waveform has {} missing bar(s) — \
             blank columns inside the drawn range, at x={:?}\n  {}",
            structure.gaps.len(),
            &structure.gaps[..structure.gaps.len().min(12)],
            shot.0.display(),
        );
        assert!(
            structure.tallest < 0.98,
            "{name} the threshold, a column is painted {:.3} of the strip's \
             height — the band stack caps well under that, so this is not the \
             band renderer drawing\n  {}",
            structure.tallest,
            shot.0.display(),
        );
    }

    // The deeper shot is deeper: the readout is the bucket count behind it.
    let buckets = out["buckets"]
        .as_str()
        .and_then(|label| label.strip_prefix("FINE "))
        .and_then(|count| count.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("the crossing shot has no resolution readout: {out:#}"));
    assert!(buckets > 0., "a measured window with no buckets in it");

    eprintln!(
        "waveform threshold shots:\n  below: {}\n  above: {}",
        below.0.display(),
        above.0.display()
    );
}

/// `ladder::waveform_low` / `_mid` / `_high` as the frame stores them.
///
/// Read off the ladder rather than written out again: the whole point of that
/// module is that a colour is spelled in one place.
fn bands() -> [(&'static str, [u8; 4]); 3] {
    [
        ("low", bytes(luma_ui::ladder::waveform_low())),
        ("mid", bytes(luma_ui::ladder::waveform_mid())),
        ("high", bytes(luma_ui::ladder::waveform_high())),
    ]
}

/// A ladder colour as an opaque frame pixel. Exact, not approximate: these are
/// opaque quads over an opaque bed, so nothing blends on the way to the frame.
fn bytes(color: gpui::Rgba) -> [u8; 4] {
    [
        (color.r * 255.).round() as u8,
        (color.g * 255.).round() as u8,
        (color.b * 255.).round() as u8,
        255,
    ]
}

/// What the band stack looks like column by column.
struct Structure {
    /// Columns inside the drawn range with no band colour in them at all — a
    /// missing bar. Empty is the only acceptable value.
    gaps: Vec<u32>,
    /// The tallest run of band colour any column reached, as a fraction of the
    /// strip. The three bands cap at `0.95 * (WAVEFORM_HEIGHT - 8)` of it, so
    /// anything near 1 was painted by something that is not the band stack.
    tallest: f64,
}

/// Walk the strip column by column and report where the band stack is not.
///
/// "Band colour" and not "not the bed": the bed is one colour but the strip
/// also carries a floor line and a playhead, and a test that called those ink
/// would be asserting about chrome. A column drawn in anything other than the
/// three band colours reads here as a column with no waveform in it, which is
/// exactly the complaint.
///
/// The scan runs between the first and last column that has band colour, so
/// the empty margins past the end of the audio are not gaps — there is nothing
/// out there to have drawn. Columns carrying the playhead are skipped: it is
/// painted over the waveform and hides the bars underneath it.
fn structure(image: &RgbaImage) -> Structure {
    let bands: Vec<[u8; 4]> = bands().into_iter().map(|(_, color)| color).collect();
    let playhead = bytes(luma_ui::ladder::playhead());
    let height = image.height();

    let mut extent: Vec<Option<(u32, u32)>> = Vec::with_capacity(image.width() as usize);
    let mut obscured = vec![false; image.width() as usize];
    for x in 0..image.width() {
        let mut run: Option<(u32, u32)> = None;
        for y in 0..height {
            let pixel = image.get_pixel(x, y).0;
            if pixel == playhead {
                obscured[x as usize] = true;
            }
            if !bands.contains(&pixel) {
                continue;
            }
            run = Some(match run {
                Some((top, _)) => (top, y),
                None => (y, y),
            });
        }
        extent.push(run);
    }

    let drawn: Vec<u32> = (0..image.width())
        .filter(|x| extent[*x as usize].is_some())
        .collect();
    let (Some(&first), Some(&last)) = (drawn.first(), drawn.last()) else {
        return Structure {
            gaps: (0..image.width()).collect(),
            tallest: 0.,
        };
    };

    Structure {
        gaps: (first..=last)
            .filter(|x| extent[*x as usize].is_none() && !obscured[*x as usize])
            .collect(),
        tallest: extent
            .iter()
            .flatten()
            .map(|(top, bottom)| f64::from(bottom - top + 1) / f64::from(height))
            .fold(0., f64::max),
    }
}

/// Every colour covering more than a percent of the strip, and the share of it
/// that is not the recessed bed.
///
/// The one-percent floor is what keeps the beat grid's stray antialiased pixels
/// and the playhead's single column out of an equality assertion, without
/// letting a whole band's worth of colour hide under it.
fn palette(image: &RgbaImage) -> (HashMap<[u8; 4], f64>, f64) {
    let mut counts: HashMap<[u8; 4], usize> = HashMap::new();
    for pixel in image.pixels() {
        *counts.entry(pixel.0).or_default() += 1;
    }
    let total = (image.width() * image.height()) as f64;
    let bed = bytes(luma_ui::ladder::muted());
    let ink = total - counts.get(&bed).copied().unwrap_or(0) as f64;
    let palette = counts
        .into_iter()
        .map(|(color, count)| (color, count as f64 / total))
        .filter(|(_, share)| *share > 0.01)
        .collect();
    (palette, ink / total)
}

/// Copy a shot somewhere stable and decode it.
///
/// The harness writes into a temporary directory it owns; a failing assertion
/// that names a path already deleted is a failing assertion nobody can look at.
fn keep(shot: &Value, name: &str) -> (PathBuf, RgbaImage) {
    let source = shot["path"].as_str().expect("a shot has a path");
    let directory = std::env::temp_dir().join("luma-waveform-threshold");
    std::fs::create_dir_all(&directory).expect("failed to create the shot directory");
    let kept = directory.join(format!("{name}.png"));
    std::fs::copy(source, &kept).expect("failed to keep the shot");
    let image = image::open(&kept)
        .expect("the harness wrote a shot that is not an image")
        .to_rgba8();
    (kept, image)
}
