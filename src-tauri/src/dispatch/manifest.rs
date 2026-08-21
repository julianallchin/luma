//! The IPC manifest generator.
//!
//! `docs/specs/ipc-manifest.{json,md}` are the contract for everything crossing
//! the host boundary. They are derived from the parent module's `commands!`
//! table rather than written by hand, so the surface cannot drift from the
//! documentation of it: [`check`] rebuilds both files and fails if what is on
//! disk differs, having written the new version first.
//!
//! Two things the table cannot know are carried across by name from the
//! previous file: the per-command prose (`prose`), and the whole `events`
//! block — Tauri events have no registry to read. Everything else is
//! regenerated, including each event's emit/listen sites, which are found by
//! searching the tree for the event's literal name: the Rust core emits, every
//! host listens.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// One argument of a dispatched command, as the table declares it.
pub(super) struct Arg {
    pub name: &'static str,
    pub rust_type: &'static str,
}

/// One row of the command table.
pub(super) struct Command {
    pub name: &'static str,
    pub domain: &'static str,
    pub args: &'static [Arg],
    pub returns: &'static str,
}

/// A `#[tauri::command]` that is not on the seam yet, found by scanning
/// `src/commands/`. Listing them is what keeps "the manifest is the whole host
/// surface" true while the port is unfinished.
struct Unported {
    name: String,
    file: String,
    line: usize,
}

const JSON_PATH: &str = "../docs/specs/ipc-manifest.json";
const MD_PATH: &str = "../docs/specs/ipc-manifest.md";

/// Rebuild both files from `table` and report whether the tree already matched.
///
/// # Panics
///
/// If a table row has no handler function, or the previous manifest is missing
/// or unreadable — both mean the generator's inputs are wrong, not that the
/// manifest is stale.
pub(super) fn check(table: &[Command]) -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let previous: Value = serde_json::from_str(
        &fs::read_to_string(root.join(JSON_PATH)).expect("previous ipc-manifest.json"),
    )
    .expect("ipc-manifest.json is valid JSON");

    let unported = scan_unported(&root);
    let events = regenerate_events(&root, &previous);
    let json = render_json(table, &root, &unported, &previous, &events);
    let markdown = render_markdown(table, &unported, &events);

    let mut stale = Vec::new();
    for (path, contents) in [(JSON_PATH, &json), (MD_PATH, &markdown)] {
        let path = root.join(path);
        if fs::read_to_string(&path).ok().as_ref() != Some(contents) {
            fs::write(&path, contents).expect("write manifest");
            stale.push(path.display().to_string());
        }
    }

    if stale.is_empty() {
        Ok(())
    } else {
        Err(stale.join(", "))
    }
}

// -----------------------------------------------------------------------------
// Derived facts
// -----------------------------------------------------------------------------

/// Wire spelling of an argument: Tauri renames handler parameters
/// `snake_case` → `camelCase`, and the manifest documents the wire.
fn camel(snake: &str) -> String {
    super::to_camel_case(snake)
}

fn handler_file(domain: &str) -> String {
    format!("src-tauri/src/dispatch/handlers/{domain}.rs")
}

/// The 1-indexed line of a handler's `pub async fn`, so a manifest row points
/// at the body rather than at a module.
fn handler_line(root: &Path, domain: &str, name: &str) -> usize {
    let path = root.join(format!("src/dispatch/handlers/{domain}.rs"));
    let source = fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()));
    let signature = format!("pub async fn {name}(");
    source
        .lines()
        .position(|line| line.trim_start().starts_with(&signature))
        .map(|index| index + 1)
        .unwrap_or_else(|| panic!("no `{signature}` in {}", path.display()))
}

fn scan_unported(root: &Path) -> Vec<Unported> {
    let mut found = Vec::new();
    let mut files: Vec<PathBuf> = fs::read_dir(root.join("src/commands"))
        .expect("src/commands")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    for path in files {
        let source = fs::read_to_string(&path).expect("read command module");
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim() != "#[tauri::command]" {
                continue;
            }
            let (offset, signature) = lines[index + 1..]
                .iter()
                .enumerate()
                .find(|(_, line)| line.contains("fn "))
                .expect("a `#[tauri::command]` with no function under it");
            let name = signature
                .split("fn ")
                .nth(1)
                .and_then(|rest| rest.split(['(', '<']).next())
                .expect("function name");
            found.push(Unported {
                name: name.to_owned(),
                file: format!(
                    "src-tauri/src/commands/{}",
                    path.file_name().unwrap().to_string_lossy()
                ),
                line: index + offset + 2,
            });
        }
    }
    found
}

/// Where each known event is emitted and listened for, found by searching the
/// tree for its quoted name. Event *names* stay hand-maintained — there is no
/// registry to enumerate them — but their sites are derived, so the one part
/// that rots on every refactor does not.
fn regenerate_events(root: &Path, previous: &Value) -> Vec<Value> {
    let repo = root.parent().expect("repo root");
    let mut sources = Vec::new();
    for (directory, extensions) in [
        ("src-tauri/src", &["rs"][..]),
        ("src", &["ts", "tsx"][..]),
        ("scripts", &["ts"][..]),
        ("gpui/crates", &["rs"][..]),
    ] {
        collect_sources(&repo.join(directory), extensions, &mut sources);
    }
    sources.sort();

    let indexed: Vec<(String, String)> = sources
        .iter()
        .map(|path| {
            (
                path.strip_prefix(repo)
                    .unwrap_or(path)
                    .display()
                    .to_string()
                    .replace('\\', "/"),
                fs::read_to_string(path).unwrap_or_default(),
            )
        })
        .collect();

    previous["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|event| {
            let name = event["name"].as_str().expect("event name").to_owned();
            let needle = format!("\"{name}\"");
            let mut emitters = Vec::new();
            let mut listeners = Vec::new();
            for (path, source) in &indexed {
                for (index, line) in source.lines().enumerate() {
                    if !line.contains(&needle) {
                        continue;
                    }
                    let site = format!("{path}:{}", index + 1);
                    if path.starts_with("src-tauri/") {
                        emitters.push(site);
                    } else {
                        listeners.push(site);
                    }
                }
            }
            let orphan = emitters.is_empty() || listeners.is_empty();
            let mut out = json!({
                "name": name,
                "emitters": emitters,
                "listeners": listeners,
                "orphan": orphan,
            });
            if let Some(note) = event.get("note").filter(|note| !note.is_null()) {
                out["note"] = note.clone();
            }
            out
        })
        .collect()
}

fn collect_sources(directory: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name == "node_modules" || name == "dist" {
            continue;
        }
        if path.is_dir() {
            collect_sources(&path, extensions, out);
        } else if path
            .extension()
            .is_some_and(|ext| extensions.iter().any(|wanted| ext == *wanted))
        {
            out.push(path);
        }
    }
}

/// Hand-written prose from the previous manifest, keyed by command name. A
/// renamed command loses its prose, which is the intended cost: the note was
/// about the old name.
fn prose_by_command(previous: &Value) -> BTreeMap<String, Value> {
    let mut kept = BTreeMap::new();
    for command in previous["commands"].as_array().into_iter().flatten() {
        let Some(name) = command["name"].as_str() else {
            continue;
        };
        // v2 keeps prose in one object; v1 spread it across four fields.
        let prose = command.get("prose").cloned().unwrap_or_else(|| {
            let mut object = Map::new();
            for key in ["returns", "sideEffects", "callerUsage", "notes"] {
                match command.get(key) {
                    Some(value) if !value.is_null() && value.as_str() != Some("") => {
                        object.insert(key.to_owned(), value.clone());
                    }
                    _ => {}
                }
            }
            Value::Object(object)
        });
        if prose.as_object().is_some_and(|object| !object.is_empty()) {
            kept.insert(name.to_owned(), prose);
        }
    }
    kept
}

// -----------------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------------

fn domains(table: &[Command]) -> Vec<(&'static str, usize)> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for command in table {
        match counts.iter_mut().find(|(name, _)| *name == command.domain) {
            Some((_, count)) => *count += 1,
            None => counts.push((command.domain, 1)),
        }
    }
    counts.sort_unstable();
    counts
}

fn render_json(
    table: &[Command],
    root: &Path,
    unported: &[Unported],
    previous: &Value,
    events: &[Value],
) -> String {
    let prose = prose_by_command(previous);
    let mut commands: Vec<Value> = table
        .iter()
        .map(|command| {
            let args: Vec<Value> = command
                .args
                .iter()
                .map(|arg| json!({ "name": camel(arg.name), "rust": arg.rust_type }))
                .collect();
            let mut row = json!({
                "name": command.name,
                "domain": command.domain,
                "handler": {
                    "file": handler_file(command.domain),
                    "line": handler_line(root, command.domain, command.name),
                },
                "args": args,
                "returns": command.returns,
            });
            if let Some(prose) = prose.get(command.name) {
                row["prose"] = prose.clone();
            }
            row
        })
        .collect();
    commands.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

    let domains: Vec<Value> = domains(table)
        .into_iter()
        .map(|(domain, count)| {
            json!({ "domain": domain, "commandCount": count, "handlers": handler_file(domain) })
        })
        .collect();

    let unported: Vec<Value> = unported
        .iter()
        .map(|command| json!({ "name": command.name, "file": command.file, "line": command.line }))
        .collect();

    let manifest = json!({
        "$schema": "https://luma.dev/schemas/ipc-manifest.v2.json",
        "version": 2,
        "source": "generated from the `commands!` table in src-tauri/src/dispatch/mod.rs by `cargo test -p luma ipc_manifest`",
        "summary": {
            "commandCount": table.len(),
            "domainCount": domains.len(),
            "unportedCount": unported.len(),
            "eventCount": events.len(),
        },
        "domains": domains,
        "commands": commands,
        "unported": unported,
        "events": events,
    });
    let mut out = serde_json::to_string_pretty(&manifest).expect("serialize manifest");
    out.push('\n');
    out
}

fn render_markdown(table: &[Command], unported: &[Unported], events: &[Value]) -> String {
    let domains = domains(table);
    let mut out = String::new();
    out.push_str(concat!(
        "# IPC manifest\n\n",
        "Every command crossing the host boundary, generated from the `commands!` table in\n",
        "`src-tauri/src/dispatch/mod.rs`. **Do not edit this file** — run\n",
        "`cargo test --manifest-path src-tauri/Cargo.toml ipc_manifest` and it rewrites itself.\n",
        "The machine-readable form is [`ipc-manifest.json`](./ipc-manifest.json); the per-command\n",
        "prose and the event names in it are the only hand-written parts and are carried across by\n",
        "name. The 2026-08-19 audit that motivated the dispatch seam — payload conventions, dead\n",
        "commands, known issues — is kept verbatim in [`ipc-audit-2026-08.md`](./ipc-audit-2026-08.md).\n\n",
    ));
    let _ = writeln!(
        out,
        "**{} commands** across **{} domains** · **{} events** · **{} commands not on the seam**\n",
        table.len(),
        domains.len(),
        events.len(),
        unported.len(),
    );

    out.push_str("## Domains\n\n| Domain | Commands | Handlers |\n| --- | ---: | --- |\n");
    for (domain, count) in &domains {
        let _ = writeln!(out, "| `{domain}` | {count} | `{}` |", handler_file(domain));
    }
    let _ = writeln!(out, "| **total** | **{}** | |", table.len());

    out.push_str("\n## Commands\n\nArguments are shown in their wire spelling; types are the Rust types the table declares.\n");
    for (domain, _) in &domains {
        let _ = writeln!(out, "\n### `{domain}`\n");
        out.push_str("| Command | Arguments | Returns |\n| --- | --- | --- |\n");
        for command in table.iter().filter(|command| command.domain == *domain) {
            let args = if command.args.is_empty() {
                "—".to_owned()
            } else {
                command
                    .args
                    .iter()
                    .map(|arg| format!("`{}: {}`", camel(arg.name), arg.rust_type))
                    .collect::<Vec<_>>()
                    .join("<br>")
            };
            let _ = writeln!(
                out,
                "| `{}` | {args} | `{}` |",
                command.name, command.returns
            );
        }
    }

    out.push_str("\n## Not on the seam\n\nStill `#[tauri::command]` in `src-tauri/src/commands/`: the spawned-progress import path, which\nreports through events rather than a return value. See\n[`dispatcher-port-guide.md`](./dispatcher-port-guide.md).\n\n");
    out.push_str("| Command | Source |\n| --- | --- |\n");
    for command in unported {
        let _ = writeln!(
            out,
            "| `{}` | `{}:{}` |",
            command.name, command.file, command.line
        );
    }

    out.push_str("\n## Events\n\nTauri events are the only push channel; there is no subscription command. Every long-running\ncommand reports progress here, not in its return value. The event *names* are hand-maintained —\nnothing enumerates them — but the sites below are found by searching the tree for each name, so a\nmoved emitter cannot leave a stale row. An event with no emitter or no listener is an orphan.\n\n");
    out.push_str("| Event | Emitters | Listeners | Note |\n| --- | ---: | ---: | --- |\n");
    for event in events {
        let count = |key: &str| event[key].as_array().map_or(0, Vec::len);
        let name = event["name"].as_str().unwrap_or_default();
        let mut note = String::new();
        if event["orphan"] == json!(true) {
            note.push_str("**orphan**");
        }
        if let Some(text) = event["note"].as_str().filter(|text| !text.is_empty()) {
            if !note.is_empty() {
                note.push_str(" — ");
            }
            note.push_str(text);
        }
        let _ = writeln!(
            out,
            "| `{name}` | {} | {} | {note} |",
            count("emitters"),
            count("listeners"),
        );
    }
    out
}
