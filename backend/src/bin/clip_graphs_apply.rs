//! Apply the clip graphs migration to the app database (spec 8.6 step 4).
//!
//! Opens the database the way the app does (`open_app_db_at`), so the SQLite
//! migrations run first and every connection carries the change log and the
//! PowerSync upload queue. Then runs each `--sql` file written by
//! `backend/scripts/migrate_clip_graphs.py` (`clips.sql`, `drafts.sql`) in
//! one transaction. Each file starts with `-- expected: N` and holds one
//! guarded `UPDATE` per line; the transaction rolls back unless every file
//! changes exactly its expected number of rows. Close the app first.
//!
//! Usage:
//!     cargo run --release --bin clip_graphs_apply -- --app-dir DIR --sql clips.sql [--sql drafts.sql]
use std::path::PathBuf;

use luma_lib::database::local::database::open_app_db_at;
use sqlx::{Connection, Row, SqliteConnection};

struct Plan {
    path: PathBuf,
    expected: u64,
    statements: Vec<String>,
}

fn read_plan(path: PathBuf) -> Result<Plan, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    let expected = lines
        .next()
        .and_then(|line| line.strip_prefix("-- expected: "))
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| {
            format!(
                "{}: the first line must be `-- expected: N`",
                path.display()
            )
        })?;
    let statements: Vec<String> = lines
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(bad) = statements.iter().find(|s| !s.starts_with("UPDATE ")) {
        return Err(format!(
            "{}: expected one UPDATE per line; got {}",
            path.display(),
            &bad[..bad.len().min(80)]
        ));
    }
    if statements.len() as u64 != expected {
        return Err(format!(
            "{}: expected {expected} statements; got {}",
            path.display(),
            statements.len()
        ));
    }
    Ok(Plan {
        path,
        expected,
        statements,
    })
}

async fn counts(connection: &mut SqliteConnection, when: &str) -> Result<(), String> {
    for (label, sql) in [
        ("ps_crud", "SELECT count(*) FROM ps_crud"),
        ("clips", "SELECT count(*) FROM clips"),
        (
            "clips with a graph",
            "SELECT count(*) FROM clips WHERE graph_json <> '{}'",
        ),
        (
            "clips with a name",
            "SELECT count(*) FROM clips WHERE name <> ''",
        ),
    ] {
        let n: i64 = sqlx::query(sql)
            .fetch_one(&mut *connection)
            .await
            .map_err(|e| format!("{label}: {e}"))?
            .get(0);
        println!("{when}: {label} {n}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut app_dir = None;
    let mut plans = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app-dir" => app_dir = args.next().map(PathBuf::from),
            "--sql" => plans.push(read_plan(
                args.next().map(PathBuf::from).ok_or("--sql needs a file")?,
            )?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let app_dir = app_dir.ok_or("--app-dir is required")?;
    if plans.is_empty() {
        return Err("--sql is required".into());
    }

    let (db, _connections) = open_app_db_at(&app_dir).await?;
    let mut connection = db.0.acquire().await.map_err(|e| e.to_string())?;
    counts(&mut connection, "before").await?;

    let mut transaction = connection.begin().await.map_err(|e| e.to_string())?;
    let mut mismatch = None;
    for plan in &plans {
        let mut changed = 0;
        for statement in &plan.statements {
            changed += sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
                .execute(&mut *transaction)
                .await
                .map_err(|e| format!("{}: {e}", plan.path.display()))?
                .rows_affected();
        }
        println!(
            "{}: changed {changed} of {}",
            plan.path.display(),
            plan.expected
        );
        if changed != plan.expected && mismatch.is_none() {
            mismatch = Some(format!(
                "{}: changed {changed} rows, expected {}; rolled back (a row changed since the dry run)",
                plan.path.display(),
                plan.expected
            ));
        }
    }
    if let Some(why) = mismatch {
        transaction.rollback().await.map_err(|e| e.to_string())?;
        return Err(why);
    }
    transaction.commit().await.map_err(|e| e.to_string())?;

    counts(&mut connection, "after").await?;
    let integrity: String = sqlx::query("PRAGMA integrity_check")
        .fetch_one(&mut *connection)
        .await
        .map_err(|e| e.to_string())?
        .get(0);
    println!("integrity_check: {integrity}");
    if integrity != "ok" {
        return Err(format!("integrity_check: {integrity}"));
    }
    Ok(())
}
