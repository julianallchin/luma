//! The app's own output, kept out of the report.
//!
//! The backend writes progress lines straight to the process's stdout and
//! stderr (`eprintln!("[waveform] …")`), and every test's app runs in this
//! process. Left alone, those lines land between the report's, and a reader —
//! often an agent paying for every line — has to sift them out.
//!
//! So file descriptors 1 and 2 go to a log file for the whole run, and the
//! report writes to copies of the originals made before the switch. A failing
//! test shows the tail of what the log gained while it ran; a passing test
//! shows none of it. Tests run side by side, so with `-j` above 1 that tail
//! can hold another test's lines too — the report says so.

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

struct Capture {
    /// The terminal (or pipe) stdout pointed at before the switch.
    out: Mutex<File>,
    log: PathBuf,
}

static CAPTURE: OnceLock<Capture> = OnceLock::new();

#[cfg(unix)]
mod fd {
    extern "C" {
        fn dup(fd: i32) -> i32;
        fn dup2(from: i32, to: i32) -> i32;
    }

    pub fn duplicate(fd: i32) -> Option<i32> {
        // SAFETY: `dup` takes any integer and fails with -1 on a bad one.
        let copy = unsafe { dup(fd) };
        (copy >= 0).then_some(copy)
    }

    pub fn replace(from: i32, to: i32) -> bool {
        // SAFETY: as above; both descriptors are open when this is called.
        unsafe { dup2(from, to) >= 0 }
    }
}

/// Send fds 1 and 2 to a log file of this run's own. Does nothing where that
/// cannot be done; the report then prints to stdout as before.
pub fn start() {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let path = std::env::temp_dir().join(format!("luma-test-{}.log", std::process::id()));
        let Ok(log) = File::create(&path) else {
            return;
        };
        let Some(saved) = fd::duplicate(1) else {
            return;
        };
        // Flush what std has buffered for the old stdout before it moves.
        std::io::stdout().flush().ok();
        if !fd::replace(log.as_raw_fd(), 1) || !fd::replace(log.as_raw_fd(), 2) {
            return;
        }
        // SAFETY: `saved` is a fresh descriptor nothing else owns.
        let out = unsafe { File::from_raw_fd(saved) };
        CAPTURE
            .set(Capture {
                out: Mutex::new(out),
                log: path,
            })
            .ok();
    }
}

/// Write one line of the report.
pub fn say(line: &str) {
    match CAPTURE.get() {
        Some(capture) => {
            let mut out = capture.out.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(out, "{line}");
        }
        None => println!("{line}"),
    }
}

/// How far the log has got, to mark where a test began and ended.
pub fn mark() -> u64 {
    CAPTURE
        .get()
        .and_then(|capture| std::fs::metadata(&capture.log).ok())
        .map_or(0, |meta| meta.len())
}

/// What the log gained between two marks: its last `lines` non-empty lines.
pub fn between(from: u64, to: u64, lines: usize) -> Vec<String> {
    let Some(capture) = CAPTURE.get() else {
        return Vec::new();
    };
    let Ok(mut file) = File::open(&capture.log) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(from)).is_err()
        || file
            .take(to.saturating_sub(from))
            .read_to_end(&mut bytes)
            .is_err()
    {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|line| line.to_string())
        .collect()
}

/// Where the log is, when there is one.
pub fn log_path() -> Option<&'static PathBuf> {
    CAPTURE.get().map(|capture| &capture.log)
}

/// Remove the log after a clean run; a failed run's is evidence.
pub fn finish(failed: bool) {
    let Some(path) = log_path() else { return };
    if failed {
        say(&format!("app log: {}", path.display()));
    } else {
        std::fs::remove_file(path).ok();
    }
}
