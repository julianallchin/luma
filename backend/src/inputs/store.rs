//! `control_bindings`: this machine's bindings, per venue. Local only.

use sqlx::{Row, SqlitePool};

use super::{Address, Binding, Mode};

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Fader => "fader",
        Mode::Momentary => "momentary",
        Mode::Toggle => "toggle",
    }
}

fn mode_of(name: &str) -> Mode {
    match name {
        "momentary" => Mode::Momentary,
        "toggle" => Mode::Toggle,
        _ => Mode::Fader,
    }
}

pub(super) async fn list(pool: &SqlitePool, venue_id: &str) -> Result<Vec<Binding>, String> {
    let rows = sqlx::query(
        "SELECT id, control, address_json, mode, invert FROM control_bindings
         WHERE venue_id = ? ORDER BY created_at, id",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("Failed to read control bindings: {error}"))?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let address: Address = serde_json::from_str(row.get("address_json")).ok()?;
            Some(Binding {
                id: row.get("id"),
                control: row.get("control"),
                address,
                mode: mode_of(row.get("mode")),
                invert: row.get::<i64, _>("invert") != 0,
            })
        })
        .collect())
}

pub(super) async fn insert(
    pool: &SqlitePool,
    venue_id: &str,
    binding: &Binding,
) -> Result<(), String> {
    let address = serde_json::to_string(&binding.address).map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO control_bindings (id, venue_id, control, address_json, mode, invert)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&binding.id)
    .bind(venue_id)
    .bind(&binding.control)
    .bind(address)
    .bind(mode_name(binding.mode))
    .bind(i64::from(binding.invert))
    .execute(pool)
    .await
    .map_err(|error| format!("Failed to save the binding: {error}"))?;
    Ok(())
}

pub(super) async fn update(
    pool: &SqlitePool,
    venue_id: &str,
    id: &str,
    mode: Mode,
    invert: bool,
) -> Result<(), String> {
    let done = sqlx::query(
        "UPDATE control_bindings SET mode = ?, invert = ? WHERE id = ? AND venue_id = ?",
    )
    .bind(mode_name(mode))
    .bind(i64::from(invert))
    .bind(id)
    .bind(venue_id)
    .execute(pool)
    .await
    .map_err(|error| format!("Failed to change the binding: {error}"))?;
    if done.rows_affected() == 0 {
        return Err("No such binding".into());
    }
    Ok(())
}

pub(super) async fn delete(pool: &SqlitePool, venue_id: &str, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM control_bindings WHERE id = ? AND venue_id = ?")
        .bind(id)
        .bind(venue_id)
        .execute(pool)
        .await
        .map_err(|error| format!("Failed to remove the binding: {error}"))?;
    Ok(())
}
