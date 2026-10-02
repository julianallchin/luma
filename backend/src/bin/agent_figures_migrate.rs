//! Move python figures out of the synced `agent_thread_messages` rows.
//!
//! One-time. For the signed-in user, reads every Supabase row whose
//! `parts_json` still holds a `base64Png`, uploads each PNG to
//! `agent-figures/<uid>/<sha256>.png`, and rewrites the row with `path` in
//! place of `base64Png` (see `luma_lib::agent::figures`). Rows go a few at a
//! time, and each is logged. Idempotent: uploads overwrite the same
//! content-addressed object, and a rewritten row no longer matches.
//!
//! Uses the app's stored session through the app's own refresh, which writes
//! the single-use refresh token's successor and its host proof back.
//!
//! Usage:
//!     cargo run --bin agent_figures_migrate -- [--app-dir DIR] [--dry-run | --apply]
//!
//! `--dry-run` is the default: it reads and counts, and writes nothing.
use std::path::PathBuf;

use base64::Engine as _;
use luma_lib::agent::figures;
use luma_lib::database::local::auth::get_current_auth;
use luma_lib::database::local::state::init_state_db_at;
use luma_lib::database::remote::common::SupabaseClient;
use luma_lib::storage::StorageRoot;
use serde_json::Value;

/// Rows fetched per request. A row can hold several megabytes of figures.
const BATCH: usize = 5;

/// Ids searched for figure bytes per request.
const SCAN: usize = 50;

#[derive(Default)]
struct Tally {
    rows: usize,
    figures: usize,
    base64_bytes: usize,
    png_bytes: usize,
}

/// Replace every `base64Png` in `value` with the figure's `path`, handing each
/// PNG to `found` first. Returns how many it replaced.
fn replace_figures(
    value: &mut Value,
    found: &mut dyn FnMut(&[u8], usize) -> Result<String, String>,
) -> Result<usize, String> {
    match value {
        Value::Array(items) => items.iter_mut().try_fold(0, |n, item| {
            replace_figures(item, found).map(|more| n + more)
        }),
        Value::Object(map) => {
            if let Some(Value::String(data)) = map.get("base64Png") {
                let png = base64::engine::general_purpose::STANDARD
                    .decode(data.as_bytes())
                    .map_err(|error| format!("unreadable figure: {error}"))?;
                let path = found(&png, data.len())?;
                map.remove("base64Png");
                map.insert("path".into(), Value::String(path));
                return Ok(1);
            }
            map.values_mut().try_fold(0, |n, item| {
                replace_figures(item, found).map(|more| n + more)
            })
        }
        _ => Ok(0),
    }
}

struct Rest {
    http: reqwest::Client,
    base: String,
    anon: String,
    token: String,
}

impl Rest {
    fn request(&self, method: reqwest::Method, query: &str) -> reqwest::RequestBuilder {
        self.http
            .request(
                method,
                format!("{}/agent_thread_messages?{query}", self.base),
            )
            .header("apikey", &self.anon)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("x-luma-actor", "migration:agent-figures")
    }

    async fn rows(&self, query: &str) -> Result<Vec<Value>, String> {
        let response = self
            .request(reqwest::Method::GET, query)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let body = response.text().await.map_err(|error| error.to_string())?;
        if !status.is_success() {
            return Err(format!("GET {query}: {status} {body}"));
        }
        serde_json::from_str(&body).map_err(|error| error.to_string())
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut app_dir = None;
    let mut apply = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app-dir" => app_dir = args.next().map(PathBuf::from),
            "--dry-run" => apply = false,
            "--apply" => apply = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let storage = match app_dir {
        Some(dir) => StorageRoot::from_path(dir),
        None => StorageRoot::from_env_default()?,
    };
    let state = init_state_db_at(storage.path()).await?;
    let auth = get_current_auth(&state.0)
        .await
        .map_err(|error| error.to_string())?
        .ok_or("not signed in: sign in to Luma first")?;
    let uid = auth.principal.user_id;
    let rest = Rest {
        http: reqwest::Client::new(),
        base: luma_lib::config::postgrest_url(),
        anon: luma_lib::config::supabase_anon_key(),
        token: auth.access_token.clone(),
    };
    let bucket = SupabaseClient::new(
        luma_lib::config::SUPABASE_URL.to_owned(),
        luma_lib::config::supabase_anon_key(),
    );
    println!("{} for {uid}", if apply { "apply" } else { "dry run" });

    // A `like` over every row's text runs past the statement timeout, so the
    // scan goes a page of ids at a time.
    let ids = |rows: Vec<Value>| -> Vec<String> {
        rows.into_iter()
            .filter_map(|row| row["id"].as_str().map(str::to_owned))
            .collect()
    };
    let assistant = ids(rest
        .rows(&format!(
            "select=id&uid=eq.{uid}&role=eq.assistant&order=id"
        ))
        .await?);
    let mut ids_with_figures = Vec::new();
    for page in assistant.chunks(SCAN) {
        ids_with_figures.extend(ids(rest
            .rows(&format!(
                "select=id&uid=eq.{uid}&id=in.({})&parts_json=like.*base64Png*&order=id",
                page.join(",")
            ))
            .await?));
    }
    let ids = ids_with_figures;
    println!(
        "{} of {} assistant rows hold figure bytes",
        ids.len(),
        assistant.len()
    );

    let mut tally = Tally::default();
    for batch in ids.chunks(BATCH) {
        let rows = rest
            .rows(&format!(
                "select=id,parts_json&uid=eq.{uid}&id=in.({})",
                batch.join(",")
            ))
            .await?;
        for row in rows {
            let id = row["id"].as_str().ok_or("a row without an id")?.to_owned();
            let text = row["parts_json"].as_str().ok_or("parts_json is not text")?;
            let mut parts: Value =
                serde_json::from_str(text).map_err(|error| format!("{id}: {error}"))?;
            let mut pngs = Vec::new();
            let count = replace_figures(&mut parts, &mut |png, encoded| {
                tally.base64_bytes += encoded;
                tally.png_bytes += png.len();
                // Applying also caches the PNG here, so this device need not
                // download what it already had.
                let sha = if apply {
                    figures::cache(&storage, png)?
                } else {
                    figures::sha(png)
                };
                pngs.push((sha.clone(), png.to_vec()));
                Ok(figures::path(Some(&uid), &sha))
            })
            .map_err(|error| format!("{id}: {error}"))?;
            let rewritten = serde_json::to_string(&parts).map_err(|error| error.to_string())?;
            tally.rows += 1;
            tally.figures += count;
            println!(
                "{id}: {count} figures, {} -> {} bytes",
                text.len(),
                rewritten.len()
            );
            if !apply {
                continue;
            }
            for (sha, png) in pngs {
                bucket
                    .upload_file(
                        figures::BUCKET,
                        &format!("{uid}/{sha}.png"),
                        png,
                        "image/png",
                        &rest.token,
                    )
                    .await
                    .map_err(|error| format!("{id}: upload {sha}: {error}"))?;
            }
            let response = rest
                .request(reqwest::Method::PATCH, &format!("id=eq.{id}&uid=eq.{uid}"))
                .header("Prefer", "return=minimal")
                .json(&serde_json::json!({
                    "parts_json": rewritten,
                    "updated_at": chrono::Utc::now().to_rfc3339(),
                }))
                .send()
                .await
                .map_err(|error| format!("{id}: {error}"))?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(format!("{id}: PATCH {status} {body}"));
            }
            println!("{id}: rewritten");
        }
    }
    println!(
        "{} rows, {} figures, {} base64 bytes ({} PNG bytes){}",
        tally.rows,
        tally.figures,
        tally.base64_bytes,
        tally.png_bytes,
        if apply { "" } else { "; nothing written" }
    );
    Ok(())
}
