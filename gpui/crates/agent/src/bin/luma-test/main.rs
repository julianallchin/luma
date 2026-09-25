//! `luma-test` — runs `.test.js` files against the real app.
//!
//! ```text
//! luma-test [PATHS|GLOBS...] [-j N] [--pixel] [--filter SUBSTR] [--json]
//!           [--slowest N] [--watch]
//! ```
//!
//! A test is a script, so adding or editing one needs no Rust compile. Each
//! test gets its own seeded library and its own app; `-j` of them run at once.
//! See `runner.js` for what a file is written against.

mod discover;
mod report;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use report::Reporter;

pub struct Options {
    paths: Vec<String>,
    jobs: Option<usize>,
    pixel: bool,
    filter: Option<String>,
    json: bool,
    watch: bool,
    slowest: usize,
}

const USAGE: &str = "usage: luma-test [PATHS|GLOBS...] [-j N] [--pixel] [--filter SUBSTR] \
                     [--json] [--slowest N] [--watch]";

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if options.pixel && !cfg!(feature = "pixel") {
        eprintln!("--pixel needs the pixel build: run `gpui/test --pixel`");
        return ExitCode::from(2);
    }
    run::quiet_panics();

    let jobs = options.jobs.unwrap_or_else(|| {
        let cores = std::thread::available_parallelism().map_or(4, usize::from);
        if options.pixel {
            2
        } else {
            (cores / 2).max(1)
        }
    });
    gpui_agent::set_harness_concurrency(jobs);

    let files = match discover::files(&options.paths, options.pixel) {
        Ok(files) if files.is_empty() => {
            eprintln!("no .test.js files matched");
            return ExitCode::from(2);
        }
        Ok(files) => files,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let failed = run_files(&files, &options, jobs);
    if !options.watch {
        return if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    watch(&files, &options, jobs)
}

/// Register and run `files`; true if anything failed.
fn run_files(files: &[PathBuf], options: &Options, jobs: usize) -> bool {
    let started = Instant::now();
    let mut reporter = Reporter::new(options.json);
    let mut jobs_list = Vec::new();
    for file in files {
        match run::register(file, options) {
            Ok((jobs, skipped)) => {
                jobs_list.extend(jobs);
                skipped
                    .into_iter()
                    .for_each(|outcome| reporter.record(outcome));
            }
            Err(outcome) => reporter.record(outcome),
        }
    }
    run::all(jobs_list, jobs, |outcome| reporter.record(outcome));
    reporter.finish(started.elapsed(), options.slowest)
}

/// Re-run a file when it changes; re-run everything when a helper does.
///
/// Only scripts are watched. Helpers are read from the source tree at run
/// time, so they need no rebuild either; Rust changes do, and a rebuilt
/// binary is a new process.
fn watch(files: &[PathBuf], options: &Options, jobs: usize) -> ExitCode {
    let helpers = run::helper_paths();
    let stamp = |path: &PathBuf| -> Option<SystemTime> {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    };
    let mut seen: Vec<_> = files.iter().chain(&helpers).map(stamp).collect();
    eprintln!("watching {} test files; ctrl-c to stop", files.len());
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let now: Vec<_> = files.iter().chain(&helpers).map(stamp).collect();
        if now == seen {
            continue;
        }
        let helper_changed = now[files.len()..] != seen[files.len()..];
        let changed: Vec<PathBuf> = if helper_changed {
            files.to_vec()
        } else {
            files
                .iter()
                .zip(now.iter().zip(&seen))
                .filter(|(_, (now, seen))| now != seen)
                .map(|(file, _)| file.clone())
                .collect()
        };
        seen = now;
        eprintln!("\n--- change: re-running {} file(s)", changed.len());
        run_files(&changed, options, jobs);
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        paths: Vec::new(),
        jobs: None,
        pixel: false,
        filter: None,
        json: false,
        watch: false,
        slowest: 0,
    };
    let number = |flag: &str, value: Option<String>| -> Result<usize, String> {
        let value = value.ok_or_else(|| format!("{flag} needs a number"))?;
        value
            .parse()
            .map_err(|_| format!("{flag}: not a number: {value}"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-j" | "--jobs" => options.jobs = Some(number("-j", args.next())?.max(1)),
            "--pixel" => options.pixel = true,
            "--filter" => {
                options.filter = Some(args.next().ok_or("--filter needs a substring")?);
            }
            "--json" => options.json = true,
            "--watch" => options.watch = true,
            "--slowest" => options.slowest = number("--slowest", args.next())?,
            "-h" | "--help" => return Err(String::new()),
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            _ => options.paths.push(arg),
        }
    }
    Ok(options)
}
