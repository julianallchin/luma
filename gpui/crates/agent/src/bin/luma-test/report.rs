//! What a run prints: one line per test, a few more under a failure, one
//! summary line — or the same as JSON lines.

use std::time::Duration;

use serde_json::json;

use crate::run::{Outcome, Status};

/// Lines of detail under a failure, at most.
const DETAIL_LINES: usize = 15;
/// Characters of one detail line, at most.
const LINE_WIDTH: usize = 200;

pub struct Reporter {
    json: bool,
    timed: Vec<(String, Duration)>,
    passed: usize,
    failed: usize,
    skipped: usize,
}

impl Reporter {
    pub fn new(json: bool) -> Self {
        Self {
            json,
            timed: Vec::new(),
            passed: 0,
            failed: 0,
            skipped: 0,
        }
    }

    pub fn record(&mut self, outcome: Outcome) {
        let name = format!("{} › {}", outcome.file, outcome.test);
        match outcome.status {
            Status::Pass => self.passed += 1,
            Status::Fail => self.failed += 1,
            Status::Skip => self.skipped += 1,
        }
        if outcome.status != Status::Skip {
            self.timed.push((name.clone(), outcome.duration));
        }
        if self.json {
            println!("{}", as_json(&outcome));
            return;
        }
        let status = match outcome.status {
            Status::Pass => "ok",
            Status::Fail => "FAIL",
            Status::Skip => "skip",
        };
        match outcome.status {
            Status::Skip => match &outcome.error {
                Some(reason) => println!("{status:<4} {name}  ({reason})"),
                None => println!("{status:<4} {name}"),
            },
            _ => println!(
                "{status:<4} {name}  {:.1}s (setup {:.1}s)",
                outcome.duration.as_secs_f64(),
                outcome.setup.as_secs_f64()
            ),
        }
        if outcome.status == Status::Fail {
            for line in details(&outcome).into_iter().take(DETAIL_LINES) {
                println!("     {line}");
            }
        }
    }

    /// Print the summary; true if anything failed.
    pub fn finish(&mut self, elapsed: Duration, slowest: usize) -> bool {
        if self.json {
            println!(
                "{}",
                json!({ "summary": {
                    "passed": self.passed,
                    "failed": self.failed,
                    "skipped": self.skipped,
                    "ms": elapsed.as_millis() as u64,
                }})
            );
        } else {
            println!(
                "{} passed, {} failed, {} skipped in {:.1}s",
                self.passed,
                self.failed,
                self.skipped,
                elapsed.as_secs_f64()
            );
            if slowest > 0 {
                self.timed.sort_by(|a, b| b.1.cmp(&a.1));
                println!("slowest:");
                for (name, duration) in self.timed.iter().take(slowest) {
                    println!("  {:>6.1}s  {name}", duration.as_secs_f64());
                }
            }
        }
        self.failed > 0
    }
}

fn clip(line: &str) -> String {
    wrap(line, 1).remove(0)
}

/// `line` in pieces of [`LINE_WIDTH`], at most `pieces` of them. A failed
/// wait's message carries the frame it gave up on, and that list is the
/// point of the message.
fn wrap(line: &str, pieces: usize) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<String> = chars
        .chunks(LINE_WIDTH)
        .take(pieces)
        .map(|chunk| chunk.iter().collect())
        .collect();
    if out.is_empty() {
        out.push(String::new());
    }
    if chars.len() > LINE_WIDTH * pieces {
        out.last_mut().expect("one piece at least").push('…');
    }
    out
}

/// The message, where it was, and what the app looked like.
fn details(outcome: &Outcome) -> Vec<String> {
    let mut lines = Vec::new();
    let error = outcome.error.as_deref().unwrap_or_default();
    // The message is what is not a stack frame; the frame that matters is
    // already in `at`, with its source line.
    let message: Vec<&str> = error
        .lines()
        .filter(|line| !line.trim_start().starts_with("at "))
        .collect();
    for line in message.iter().take(6) {
        lines.extend(wrap(line, 4));
    }
    if let Some(at) = &outcome.at {
        lines.push(clip(&format!("at {at}")));
    }
    // `until` already names the frame it gave up on.
    if let Some(frame) = &outcome.frame {
        if !error.contains("last frame") {
            lines.push(clip(&format!("frame: {}", frame.join(", "))));
        }
    }
    if let Some(shot) = &outcome.shot {
        lines.push(format!("shot: {shot}"));
    }
    if let Some(library) = &outcome.library {
        lines.push(format!("library: {library}"));
    }
    let console: Vec<&str> = outcome.console.lines().filter(|l| !l.is_empty()).collect();
    if !console.is_empty() {
        lines.push("console:".into());
        let skip = console.len().saturating_sub(4);
        for line in &console[skip..] {
            lines.extend(wrap(&format!("  {line}"), 2));
        }
    }
    lines
}

fn as_json(outcome: &Outcome) -> serde_json::Value {
    json!({
        "file": outcome.file,
        "test": outcome.test,
        "status": match outcome.status {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Skip => "skip",
        },
        "ms": outcome.duration.as_millis() as u64,
        "setupMs": outcome.setup.as_millis() as u64,
        "error": outcome.error,
        "at": outcome.at,
        "frame": outcome.frame,
        "shot": outcome.shot,
        "console": outcome.console,
        "library": outcome.library,
    })
}
