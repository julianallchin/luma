//! Which files a run means.
//!
//! An argument is a file, a directory, a glob (`tests/js/**/set*.test.js`, for
//! when the shell did not expand it), or a bare name (`settings`), which finds
//! `<name>.test.js` anywhere under the suite root.

use std::path::{Path, PathBuf};

/// Where the suite lives.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js")
}

const SUFFIX: &str = ".test.js";

pub fn files(args: &[String], pixel: bool) -> Result<Vec<PathBuf>, String> {
    let root = root();
    let mut found = Vec::new();
    if args.is_empty() {
        // A headless run does not even register the pixel directory.
        walk(&root, &mut |path| {
            if pixel || !under_pixel(path) {
                found.push(path.to_path_buf());
            }
        });
    }
    for arg in args {
        let path = PathBuf::from(arg);
        if arg.contains(['*', '?']) {
            let before = found.len();
            glob(arg, &mut found);
            if found.len() == before {
                return Err(format!("{arg}: no files match"));
            }
        } else if path.is_dir() {
            walk(&path, &mut |path| found.push(path.to_path_buf()));
        } else if path.is_file() {
            found.push(path);
        } else {
            let name = format!("{}{SUFFIX}", arg.trim_end_matches(SUFFIX));
            let before = found.len();
            walk(&root, &mut |path| {
                if path.file_name().is_some_and(|file| *file == *name) {
                    found.push(path.to_path_buf());
                }
            });
            if found.len() == before {
                return Err(format!(
                    "{arg}: no such file, and no {name} under {}",
                    root.display()
                ));
            }
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// Files under `tests/js/pixel/` run only in pixel mode.
pub fn under_pixel(path: &Path) -> bool {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = std::fs::canonicalize(root()).unwrap_or_else(|_| root());
    absolute
        .strip_prefix(&root)
        .is_ok_and(|rest| rest.starts_with("pixel"))
}

fn walk(dir: &Path, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, visit);
        } else if path.to_string_lossy().ends_with(SUFFIX) {
            visit(&path);
        }
    }
}

/// Walk from the glob's literal prefix and match the rest segment by segment.
fn glob(pattern: &str, found: &mut Vec<PathBuf>) {
    let segments: Vec<&str> = pattern.split('/').collect();
    let literal = segments
        .iter()
        .take_while(|segment| !segment.contains(['*', '?']))
        .count();
    let base: PathBuf = if literal == 0 {
        PathBuf::from(".")
    } else {
        segments[..literal].iter().collect()
    };
    let rest = &segments[literal..];
    let mut candidates = Vec::new();
    collect(&base, &mut candidates);
    for path in candidates {
        let Ok(relative) = path.strip_prefix(&base) else {
            continue;
        };
        let parts: Vec<String> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        if matches_path(rest, &parts) {
            found.push(path);
        }
    }
}

/// Every file under `dir`, test or not: the pattern decides.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            collect(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn matches_path(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            matches_path(&pattern[1..], path)
                || (!path.is_empty() && matches_path(pattern, &path[1..]))
        }
        (Some(segment), Some(part)) => {
            matches_segment(segment.as_bytes(), part.as_bytes())
                && matches_path(&pattern[1..], &path[1..])
        }
        _ => false,
    }
}

fn matches_segment(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            matches_segment(&pattern[1..], text)
                || (!text.is_empty() && matches_segment(pattern, &text[1..]))
        }
        (Some(b'?'), Some(_)) => matches_segment(&pattern[1..], &text[1..]),
        (Some(a), Some(b)) if a == b => matches_segment(&pattern[1..], &text[1..]),
        _ => false,
    }
}
