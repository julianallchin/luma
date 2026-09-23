//! Apply the clip forms change set to the app database.
//!
//! Opens the database the way the app does (`open_app_db_at`), so every
//! connection carries the change log and the PowerSync upload queue, and runs
//! the dry run's `proposed_changes.sql` on one connection. Close the app first.
//!
//! Usage:
//!     cargo run --release --bin clip_forms_apply -- --app-dir DIR --sql FILE

use std::path::PathBuf;

use luma_lib::database::local::database::open_app_db_at;
use sqlx::Row;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut app_dir = None;
    let mut sql = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app-dir" => app_dir = args.next().map(PathBuf::from),
            "--sql" => sql = args.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let app_dir = app_dir.ok_or("--app-dir is required")?;
    let sql = sql.ok_or("--sql is required")?;
    let script = std::fs::read_to_string(&sql).map_err(|e| format!("{}: {e}", sql.display()))?;

    let (db, _connections) = open_app_db_at(&app_dir).await?;
    let mut connection = db.0.acquire().await.map_err(|e| e.to_string())?;
    let count = |table: &'static str| format!("SELECT count(*) FROM {table}");
    for table in ["ps_crud", "clips", "score_definitions"] {
        let n: i64 = sqlx::query(sqlx::AssertSqlSafe(count(table)))
            .fetch_one(&mut *connection)
            .await
            .map_err(|e| e.to_string())?
            .get(0);
        println!("before: {table} {n}");
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(script))
        .execute(&mut *connection)
        .await
        .map_err(|e| format!("apply failed: {e}"))?;
    for table in ["ps_crud", "clips", "score_definitions"] {
        let n: i64 = sqlx::query(sqlx::AssertSqlSafe(count(table)))
            .fetch_one(&mut *connection)
            .await
            .map_err(|e| e.to_string())?
            .get(0);
        println!("after: {table} {n}");
    }
    Ok(())
}
