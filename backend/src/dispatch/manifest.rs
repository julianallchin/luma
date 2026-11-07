//! The IPC manifest generator.
//!
//! `docs/specs/ipc-manifest.{json,md}` are the contract for everything crossing
//! the host boundary. They are derived from the parent module's `commands!`
//! table rather than written by hand, so the surface cannot drift from the
//! documentation of it: [`check`] rebuilds both files and fails if what is on
//! disk differs.
//!
//! The one thing the table cannot know, the per-command prose (`prose`), is
//! carried across by name from the previous file. Host events are not listed:
//! they are the `events::Event` enum, and every host matches it exhaustively.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

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

const JSON_PATH: &str = "../docs/specs/ipc-manifest.json";
const MD_PATH: &str = "../docs/specs/ipc-manifest.md";

/// Rebuild both files from `table` and check them against the tree, as
/// goldens: `LUMA_UPDATE_GOLDENS` rewrites a stale one.
///
/// # Panics
///
/// If either file is stale, a table row has no handler function, or the
/// previous manifest is missing or unreadable.
pub(super) fn check(table: &[Command]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let previous: Value = serde_json::from_str(
        &fs::read_to_string(root.join(JSON_PATH)).expect("previous ipc-manifest.json"),
    )
    .expect("ipc-manifest.json is valid JSON");

    let json = render_json(table, &root, &previous);
    let markdown = render_markdown(table);
    luma_scene::golden::assert_golden(&root.join(JSON_PATH), &json);
    luma_scene::golden::assert_golden(&root.join(MD_PATH), &markdown);
}

// -----------------------------------------------------------------------------
// Derived facts
// -----------------------------------------------------------------------------

/// Wire spelling of an argument: The dispatcher maps handler parameters
/// `snake_case` → `camelCase`, and the manifest documents the wire.
fn camel(snake: &str) -> String {
    super::to_camel_case(snake)
}

fn handler_file(domain: &str) -> String {
    format!("backend/src/dispatch/handlers/{domain}.rs")
}

/// A handler's file, after checking that it defines the command's
/// `pub async fn`. Files only, not lines: a line number changes with every
/// edit above it, and the manifest would go stale for no change in the surface.
fn handler_file_checked(root: &Path, domain: &str, name: &str) -> String {
    let path = root.join(format!("src/dispatch/handlers/{domain}.rs"));
    let source = fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()));
    let signature = format!("pub async fn {name}(");
    assert!(
        source
            .lines()
            .any(|line| line.trim_start().starts_with(&signature)),
        "no `{signature}` in {}",
        path.display()
    );
    handler_file(domain)
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
        let prose = command.get("prose").cloned().unwrap_or(Value::Null);
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

fn render_json(table: &[Command], root: &Path, previous: &Value) -> String {
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
                    "file": handler_file_checked(root, command.domain, command.name),
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

    let manifest = json!({
        "$schema": "https://luma.dev/schemas/ipc-manifest.v2.json",
        "version": 2,
        "source": "generated from the `commands!` table in backend/src/dispatch/mod.rs by `cargo test -p luma ipc_manifest`",
        "summary": {
            "commandCount": table.len(),
            "domainCount": domains.len(),
        },
        "domains": domains,
        "commands": commands,
    });
    let mut out = serde_json::to_string_pretty(&manifest).expect("serialize manifest");
    out.push('\n');
    out
}

fn render_markdown(table: &[Command]) -> String {
    let domains = domains(table);
    let mut out = String::new();
    out.push_str(concat!(
        "# IPC manifest\n\n",
        "Every command crossing the host boundary, generated from the `commands!` table in\n",
        "`backend/src/dispatch/mod.rs`. **Do not edit this file** — run\n",
        "`cargo test --manifest-path backend/Cargo.toml ipc_manifest` and it rewrites itself.\n",
        "The machine-readable form is [`ipc-manifest.json`](./ipc-manifest.json); the per-command\n",
        "prose in it is the only hand-written part and is carried across by name.\n\n",
    ));
    let _ = writeln!(
        out,
        "**{} commands** across **{} domains**\n",
        table.len(),
        domains.len(),
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

    out.push_str(concat!(
        "\n## Events\n\n",
        "Host events are the only push channel; there is no subscription command. They are the\n",
        "`Event` enum in `backend/src/dispatch/services.rs`, which every host matches exhaustively.\n",
    ));
    out
}
