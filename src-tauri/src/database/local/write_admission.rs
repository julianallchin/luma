use sqlx::SqliteConnection;

/// Enter the narrow capability used by a remote pull transaction. Provenance
/// columns are data; only this app-owned admission transition authorizes rows
/// belonging to another principal.
pub(crate) async fn enter_remote_writes(connection: &mut SqliteConnection) -> Result<(), String> {
    let changed = sqlx::query(
        "UPDATE auth_write_admission SET remote_writes = 1
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND remote_writes = 0",
    )
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to enter remote-write admission: {error}"))?
    .rows_affected();
    if changed != 1 {
        return Err("Remote writes require an active authenticated admission".into());
    }
    Ok(())
}

/// Leave remote-write mode before committing the transaction. A failure keeps
/// the transaction uncommitted, so the capability can never leak globally.
pub(crate) async fn leave_remote_writes(connection: &mut SqliteConnection) -> Result<(), String> {
    let changed = sqlx::query(
        "UPDATE auth_write_admission SET remote_writes = 0
         WHERE singleton = 1 AND remote_writes = 1",
    )
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to leave remote-write admission: {error}"))?
    .rows_affected();
    if changed != 1 {
        return Err("Remote-write admission escaped its transaction".into());
    }
    Ok(())
}

/// Temporarily close ordinary admission and enter the narrow transaction-local
/// mode used for an already-authorized aggregate cascade. The IMMEDIATE
/// transaction held by the caller prevents an identity switch from crossing
/// this transition.
pub(crate) async fn enter_maintenance_writes(
    connection: &mut SqliteConnection,
    principal: Option<&str>,
) -> Result<(), String> {
    let changed = sqlx::query(
        "UPDATE auth_write_admission
         SET accepting = 0, maintenance = 1
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND remote_writes = 0 AND active_uid IS ?",
    )
    .bind(principal)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to enter maintenance-write admission: {error}"))?
    .rows_affected();
    if changed != 1 {
        return Err("Maintenance writes require the currently admitted principal".into());
    }
    Ok(())
}

/// Restore the ordinary admission mode before the maintenance transaction is
/// allowed to commit. A failure leaves the whole transaction uncommitted.
pub(crate) async fn leave_maintenance_writes(
    connection: &mut SqliteConnection,
    principal: Option<&str>,
) -> Result<(), String> {
    let changed = sqlx::query(
        "UPDATE auth_write_admission
         SET accepting = 1, maintenance = 0
         WHERE singleton = 1 AND armed = 1 AND accepting = 0
           AND maintenance = 1 AND remote_writes = 0 AND active_uid IS ?",
    )
    .bind(principal)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to leave maintenance-write admission: {error}"))?
    .rows_affected();
    if changed != 1 {
        return Err("Maintenance-write admission escaped its transaction".into());
    }
    Ok(())
}
