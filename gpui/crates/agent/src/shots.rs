//! `image.*` in a script: the few reads pixel tests make of a screenshot.
//!
//! Three reads cover what the pixel suites assert: how bright a region is,
//! how much of it changed between two shots, and how much of it is one hue
//! (a refusal's red). `keep` copies a shot somewhere a person can find it. Rects are logical pixels, like
//! node bounds, so a script can pass `node.bounds` straight in; the shot's
//! `scale` turns them into device pixels.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::HarnessError;

/// Per-channel delta below which a pixel is noise, not change: antialiasing,
/// compositor rounding, GPU dithering. The same default the cargo suites use.
const CHANNEL_NOISE: u8 = 3;

#[derive(Deserialize)]
struct Shot {
    path: String,
    #[serde(default = "one")]
    scale: f32,
}

fn one() -> f32 {
    1.0
}

#[derive(Deserialize, Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stats {
    shot: Shot,
    rect: Option<Rect>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Diff {
    a: Shot,
    b: Shot,
    threshold: Option<u8>,
    rect: Option<Rect>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tint {
    shot: Shot,
    channel: Channel,
    margin: Option<u8>,
    rect: Option<Rect>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Channel {
    Red,
    Green,
    Blue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keep {
    shot: Shot,
    name: String,
}

pub(crate) fn call(op: &str, args: &str) -> Result<String, HarnessError> {
    let bad = |error: serde_json::Error| HarnessError::BadCall(format!("image.{op}: {error}"));
    let value = match op {
        "stats" => stats(serde_json::from_str(args).map_err(bad)?)?,
        "diff" => diff(serde_json::from_str(args).map_err(bad)?)?,
        "tint" => tint(serde_json::from_str(args).map_err(bad)?)?,
        "keep" => keep(serde_json::from_str(args).map_err(bad)?)?,
        other => return Err(HarnessError::BadCall(format!("no image.{other}"))),
    };
    Ok(value.to_string())
}

fn open(shot: &Shot) -> Result<image::RgbaImage, HarnessError> {
    image::open(&shot.path)
        .map(|image| image.to_rgba8())
        .map_err(|error| HarnessError::BadCall(format!("could not read {}: {error}", shot.path)))
}

/// The device-pixel box `rect` covers, clipped to the image. Empty is an
/// error: a mean over nothing is NaN, and NaN passes no comparison — so a
/// wrong rect would fail every assertion for a reason nobody reads.
fn region(
    image: &image::RgbaImage,
    scale: f32,
    rect: Option<Rect>,
) -> Result<(u32, u32, u32, u32), HarnessError> {
    let (width, height) = image.dimensions();
    let Some(rect) = rect else {
        return Ok((0, 0, width, height));
    };
    let at = |value: f32| (value * scale).round().max(0.) as u32;
    let (x0, y0) = (at(rect.x).min(width), at(rect.y).min(height));
    let (x1, y1) = (
        at(rect.x + rect.width).min(width),
        at(rect.y + rect.height).min(height),
    );
    if x1 <= x0 || y1 <= y0 {
        return Err(HarnessError::BadCall(format!(
            "rect {{x: {}, y: {}, width: {}, height: {}}} is outside the {}×{} shot (scale {scale})",
            rect.x, rect.y, rect.width, rect.height, width, height
        )));
    }
    Ok((x0, y0, x1 - x0, y1 - y0))
}

fn luma(pixel: &image::Rgba<u8>) -> f64 {
    0.299 * f64::from(pixel[0]) + 0.587 * f64::from(pixel[1]) + 0.114 * f64::from(pixel[2])
}

fn stats(args: Stats) -> Result<Value, HarnessError> {
    let image = open(&args.shot)?;
    let (x, y, width, height) = region(&image, args.shot.scale, args.rect)?;
    let view = image::imageops::crop_imm(&image, x, y, width, height);
    let mut sum = [0f64; 4];
    let (mut total, mut min, mut max) = (0f64, f64::MAX, f64::MIN);
    for (_, _, pixel) in image::GenericImageView::pixels(&*view) {
        for (channel, sum) in sum.iter_mut().enumerate() {
            *sum += f64::from(pixel[channel]);
        }
        let value = luma(&pixel);
        total += value;
        min = min.min(value);
        max = max.max(value);
    }
    let count = f64::from(width * height);
    Ok(json!({
        "meanLuma": total / count,
        "min": min,
        "max": max,
        "mean": sum.map(|sum| sum / count),
        "width": width,
        "height": height,
    }))
}

fn diff(args: Diff) -> Result<Value, HarnessError> {
    let (a, b) = (open(&args.a)?, open(&args.b)?);
    if a.dimensions() != b.dimensions() {
        return Err(HarnessError::BadCall(format!(
            "image.diff: {:?} and {:?} differ in size",
            a.dimensions(),
            b.dimensions()
        )));
    }
    let threshold = args.threshold.unwrap_or(CHANNEL_NOISE);
    let (x, y, width, height) = region(&a, args.a.scale, args.rect)?;
    let (a, b) = (
        image::imageops::crop_imm(&a, x, y, width, height),
        image::imageops::crop_imm(&b, x, y, width, height),
    );
    let changed = image::GenericImageView::pixels(&*a)
        .zip(image::GenericImageView::pixels(&*b))
        .filter(|((_, _, left), (_, _, right))| {
            (0..3).any(|c| left[c].abs_diff(right[c]) >= threshold)
        })
        .count();
    Ok(json!(changed as f64 / f64::from(width * height)))
}

/// The fraction of pixels in which `channel` exceeds both others by at least
/// `margin` (default 40): how much of a region reads as that hue rather than
/// as one of the greys an interface is mostly made of.
fn tint(args: Tint) -> Result<Value, HarnessError> {
    let image = open(&args.shot)?;
    let (x, y, width, height) = region(&image, args.shot.scale, args.rect)?;
    let view = image::imageops::crop_imm(&image, x, y, width, height);
    let margin = i16::from(args.margin.unwrap_or(40));
    let index = args.channel as usize;
    let tinted = image::GenericImageView::pixels(&*view)
        .filter(|(_, _, pixel)| {
            let value = i16::from(pixel[index]);
            (0..3)
                .filter(|&other| other != index)
                .all(|other| value >= i16::from(pixel[other]) + margin)
        })
        .count();
    Ok(json!(tinted as f64 / f64::from(width * height)))
}

/// Copy a shot out of the harness's own directory, which goes with the
/// process, to `$LUMA_SHOTS/<name>.png` (default `<temp>/luma-shots`), and
/// say where. `name` may carry a drawer (`"sidebar/push-01"`) but may not
/// climb out of the root.
fn keep(args: Keep) -> Result<Value, HarnessError> {
    let relative = std::path::Path::new(&args.name);
    if args.name.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(HarnessError::BadCall(format!(
            "image.keep: {:?} is not a relative name inside the shots directory",
            args.name
        )));
    }
    let root = std::env::var_os("LUMA_SHOTS").map_or_else(
        || std::env::temp_dir().join("luma-shots"),
        std::path::PathBuf::from,
    );
    let kept = root.join(format!("{}.png", args.name));
    let failed = |error: std::io::Error| {
        HarnessError::BadCall(format!("image.keep: {}: {error}", kept.display()))
    };
    std::fs::create_dir_all(kept.parent().unwrap_or(&root)).map_err(failed)?;
    std::fs::copy(&args.shot.path, &kept).map_err(failed)?;
    Ok(json!(kept.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Left half black, right half white, 20×10; `mark` paints the top-left
    /// 5×5 red.
    fn shot(name: &str, mark: bool) -> String {
        let image = image::RgbaImage::from_fn(20, 10, |x, y| match (x, y) {
            (0..5, 0..5) if mark => image::Rgba([255, 0, 0, 255]),
            (10.., _) => image::Rgba([255, 255, 255, 255]),
            _ => image::Rgba([0, 0, 0, 255]),
        });
        let path = std::env::temp_dir().join(format!(
            "gpui-agent-shots-{name}-{}.png",
            std::process::id()
        ));
        image.save(&path).unwrap();
        path.display().to_string()
    }

    fn run(op: &str, args: Value) -> Result<Value, HarnessError> {
        call(op, &args.to_string()).map(|out| serde_json::from_str(&out).unwrap())
    }

    #[test]
    fn stats_and_diff_read_logical_rects() {
        let (a, b) = (shot("a", false), shot("b", true));
        let whole = run("stats", json!({ "shot": { "path": a } })).unwrap();
        assert_eq!(whole["meanLuma"], 127.5);
        // At scale 2, logical x 5..10 is device x 10..20: the white half.
        let right = run(
            "stats",
            json!({ "shot": { "path": a, "scale": 2 }, "rect": { "x": 5, "y": 0, "width": 5, "height": 5 } }),
        )
        .unwrap();
        assert_eq!(right["min"], 255.0);
        assert_eq!(
            run("diff", json!({ "a": { "path": a }, "b": { "path": b } })).unwrap(),
            0.125
        );
        let outside =
            json!({ "shot": { "path": a }, "rect": { "x": 30, "y": 0, "width": 5, "height": 5 } });
        assert!(run("stats", outside).is_err());
        // 25 of 200 pixels are pure red; none are green.
        assert_eq!(
            run("tint", json!({ "shot": { "path": b }, "channel": "red" })).unwrap(),
            0.125
        );
        assert_eq!(
            run("tint", json!({ "shot": { "path": b }, "channel": "green" })).unwrap(),
            0.0
        );
        assert!(run("keep", json!({ "shot": { "path": a }, "name": "../out" })).is_err());
    }
}
