//! `luma-record <score-id> <output.mp4>` — one score in, one mp4 out.
//!
//! A thin caller. Everything that makes a recording a recording lives in
//! `luma_lib::recording`; this binary parses flags, boots the same headless host
//! `agent_harness` and `luma-mcp` boot, and prints progress.
//!
//! ```text
//! luma-record <score-id> <out.mp4>
//!     [--view front|audience|overhead|quarter-left|quarter-right|dj]
//!     [--width 1280] [--height 720] [--fps 30] [--span 30:90]
//!     [--config-dir DIR] [--fixtures-root DIR] [--cache-dir DIR]
//! ```

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use luma_lib::headless_host::{boot, HostConfig};
use luma_lib::recording::{record, Recording};
use luma_scene::View;

const USAGE: &str = "usage: luma-record <score-id> <output.mp4> \
[--view front] [--width 1280] [--height 720] [--fps 30] [--span start:end] \
[--config-dir DIR] [--fixtures-root DIR] [--cache-dir DIR]";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("luma-record: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let (spec, host) = parse(std::env::args().skip(1))?;
    let services = boot(&host).await?;

    let cancel: Arc<AtomicBool> = Arc::default();
    // Ctrl-C stops at the next frame boundary; the encoder is then closed the
    // ordinary way, so a half-recording is still a playable file's worth of
    // frames rather than a truncated container.
    let stop = Arc::clone(&cancel);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nstopping at the next frame…");
            stop.store(true, Ordering::Relaxed);
        }
    });

    let bar = ProgressBar::new(0).with_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40} {pos}/{len} frames · {per_sec} · eta {eta}",
        )
        .expect("the template is a literal")
        .progress_chars("##-"),
    );
    bar.set_draw_target(ProgressDrawTarget::stderr());
    // `recording` reports; the bar draws. The module never learns what a
    // terminal is.
    let ticker = bar.clone();
    let progress = move |p: luma_lib::recording::Progress| {
        if ticker.length() != Some(p.total) {
            ticker.set_length(p.total);
        }
        ticker.set_position(p.frame);
    };

    let started = Instant::now();
    let out = record(
        &services.db().0,
        services.storage(),
        services.fixtures_root(),
        spec,
        cancel,
        progress,
    )
    .await
    .map_err(|error| {
        bar.abandon();
        error.to_string()
    })?;
    bar.finish_and_clear();

    let wall = started.elapsed().as_secs_f64();
    let frames = out.frames.max(1) as f64;
    eprintln!(
        "{} — {} frames, {:.1}s of video in {:.1}s wall · {:.2}x realtime · \
render {:.0} ms/frame · encode {:.1} ms/frame",
        out.path.display(),
        out.frames,
        out.duration,
        wall,
        f64::from(out.duration) / wall,
        out.render.as_secs_f64() * 1e3 / frames,
        out.encode.as_secs_f64() * 1e3 / frames,
    );
    println!("{}", out.path.display());
    Ok(())
}

/// Split the command line into a [`Recording`] and the shared headless flags.
///
/// Unknown flags are forwarded to [`HostConfig::parse_args`] rather than
/// rejected here, so there is one list of host flags, not two.
fn parse(args: impl Iterator<Item = String>) -> Result<(Recording, HostConfig), String> {
    let mut positional = Vec::new();
    let mut shared = Vec::new();
    let mut view = View::Front;
    let mut size = (1280u32, 720u32);
    let mut fps = 30u32;
    let mut span = None;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.into()),
            "--view" => {
                let name = args.next().ok_or("--view needs a value")?;
                view = View::from_str(&name).map_err(|error| error.to_string())?;
            }
            "--width" => size.0 = number(args.next(), "--width")?,
            "--height" => size.1 = number(args.next(), "--height")?,
            "--fps" => fps = number(args.next(), "--fps")?,
            "--span" => span = Some(parse_span(&args.next().ok_or("--span needs a value")?)?),
            other if other.starts_with("--") => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{other} needs a value"))?;
                shared.push(arg);
                shared.push(value);
            }
            _ => positional.push(arg),
        }
    }

    let [score_id, output] = <[String; 2]>::try_from(positional).map_err(|_| USAGE.to_string())?;
    Ok((
        Recording {
            score_id,
            view,
            span,
            size,
            fps,
            output: PathBuf::from(output),
        },
        HostConfig::parse_args(shared.into_iter())?,
    ))
}

fn number(value: Option<String>, flag: &str) -> Result<u32, String> {
    value
        .ok_or_else(|| format!("{flag} needs a value"))?
        .parse()
        .map_err(|_| format!("{flag} takes a whole number"))
}

/// `start:end`, in seconds. Either side may be empty for "the track's own".
fn parse_span(text: &str) -> Result<(f32, f32), String> {
    let (start, end) = text
        .split_once(':')
        .ok_or_else(|| format!("--span takes start:end in seconds, not {text:?}"))?;
    let side = |s: &str, fallback: f32| -> Result<f32, String> {
        if s.is_empty() {
            return Ok(fallback);
        }
        s.parse()
            .map_err(|_| format!("--span takes seconds, not {s:?}"))
    };
    Ok((side(start, 0.0)?, side(end, f32::MAX)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(args: &[&str]) -> Recording {
        parse(args.iter().map(|s| (*s).to_string()))
            .expect("parses")
            .0
    }

    #[test]
    fn the_defaults_are_720p30_front() {
        let out = spec(&["score-1", "out.mp4"]);
        assert_eq!(out.score_id, "score-1");
        assert_eq!(out.output, PathBuf::from("out.mp4"));
        assert_eq!((out.size, out.fps), ((1280, 720), 30));
        assert!(matches!(out.view, View::Front));
        assert!(out.span.is_none());
    }

    #[test]
    fn a_span_is_two_seconds_counts() {
        let out = spec(&["s", "o.mp4", "--span", "30:90.5"]);
        assert_eq!(out.span, Some((30.0, 90.5)));
    }

    #[test]
    fn an_open_ended_span_leans_on_the_clamp() {
        assert_eq!(
            spec(&["s", "o.mp4", "--span", "30:"]).span,
            Some((30.0, f32::MAX))
        );
        assert_eq!(
            spec(&["s", "o.mp4", "--span", ":30"]).span,
            Some((0.0, 30.0))
        );
    }

    #[test]
    fn host_flags_pass_through_untouched() {
        let (out, _) = parse(
            ["s", "o.mp4", "--fps", "60", "--config-dir", "/tmp/x"]
                .iter()
                .map(|s| (*s).to_string()),
        )
        .expect("forwards the host flag");
        assert_eq!(out.fps, 60);
    }

    #[test]
    fn a_missing_output_is_a_usage_error() {
        assert!(parse(["only-a-score".to_string()].into_iter()).is_err());
    }
}
