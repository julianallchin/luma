//! A venue's controllers (`perform_controllers`) and the bindings each
//! holds (`control_bindings`). Both sync, like the rest of the venue.

use sqlx::{Row, SqlitePool};

use super::profile::Profile;
use super::{Address, Binding, Controller, Mode};
use crate::database::local::deletes;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};

/// What a controller is called before anyone names it.
pub(super) const UNNAMED: &str = "Controller";

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

fn failed(what: &'static str) -> impl Fn(sqlx::Error) -> String {
    move |error| format!("Failed to {what}: {error}")
}

async fn write<'a>(
    pool: &'a SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
) -> Result<VenueAccess<'a, Write>, String> {
    VenueAccess::<Write>::write_as(pool, VenueResource::Venue(venue_id), actor).await
}

/// The venue's controllers and their bindings, oldest first.
pub(super) async fn read(
    pool: &SqlitePool,
    venue_id: &str,
) -> Result<(Vec<Controller>, Vec<Binding>), String> {
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id)).await?;
    let rows = sqlx::query(
        "SELECT id, name, port, profile_json FROM perform_controllers
         WHERE venue_id = ? ORDER BY created_at, id",
    )
    .bind(venue_id)
    .fetch_all(access.connection())
    .await
    .map_err(failed("read the controllers"))?;
    let controllers = rows
        .into_iter()
        .map(|row| {
            let id: String = row.get("id");
            let profile = Profile::parse(row.get("profile_json")).unwrap_or_else(|error| {
                log::warn!("controller {id}'s profile is not read: {error}");
                Profile::default()
            });
            Controller {
                id,
                name: row.get("name"),
                port: row.get("port"),
                profile,
            }
        })
        .collect();
    let rows = sqlx::query(
        "SELECT id, controller_id, control, address_json, mode, invert FROM control_bindings
         WHERE venue_id = ? ORDER BY created_at, id",
    )
    .bind(venue_id)
    .fetch_all(access.connection())
    .await
    .map_err(failed("read the control bindings"))?;
    let bindings = rows
        .into_iter()
        .filter_map(|row| {
            let address: Address = serde_json::from_str(row.get("address_json")).ok()?;
            Some(Binding {
                id: row.get("id"),
                controller_id: row.get("controller_id"),
                control: row.get("control"),
                address,
                mode: mode_of(row.get("mode")),
                invert: row.get::<i64, _>("invert") != 0,
            })
        })
        .collect();
    Ok((controllers, bindings))
}

async fn insert_controller(
    access: &mut VenueAccess<'_, Write>,
    name: &str,
    port: &str,
) -> Result<Controller, String> {
    let controller = Controller {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_owned(),
        port: port.to_owned(),
        profile: Profile::default(),
    };
    let (uid, venue_id) = (
        access.principal().map(str::to_owned),
        access.venue_id().to_owned(),
    );
    sqlx::query(
        "INSERT INTO perform_controllers (id, uid, venue_id, name, port, profile_json)
         VALUES (?, ?, ?, ?, ?, '{}')",
    )
    .bind(&controller.id)
    .bind(uid)
    .bind(venue_id)
    .bind(name)
    .bind(port)
    .execute(access.connection())
    .await
    .map_err(failed("add the controller"))?;
    Ok(controller)
}

/// Save a binding of `controller_id`, or, when it already binds `control`
/// to `address`, change that one to `mode` and `invert`. Answers the
/// binding as saved.
async fn upsert(
    access: &mut VenueAccess<'_, Write>,
    controller_id: &str,
    control: &str,
    address: &Address,
    mode: Mode,
    invert: bool,
) -> Result<Binding, String> {
    let json = serde_json::to_string(address).map_err(|error| error.to_string())?;
    let (uid, venue_id) = (
        access.principal().map(str::to_owned),
        access.venue_id().to_owned(),
    );
    let row = sqlx::query(
        "INSERT INTO control_bindings
             (id, uid, venue_id, controller_id, control, address_json, mode, invert)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (controller_id, control, address_json)
         DO UPDATE SET mode = excluded.mode, invert = excluded.invert
         RETURNING id",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(uid)
    .bind(venue_id)
    .bind(controller_id)
    .bind(control)
    .bind(json)
    .bind(mode_name(mode))
    .bind(i64::from(invert))
    .fetch_one(access.connection())
    .await
    .map_err(failed("save the binding"))?;
    Ok(Binding {
        id: row.get("id"),
        controller_id: controller_id.to_owned(),
        control: control.to_owned(),
        address: address.clone(),
        mode,
        invert,
    })
}

/// Bind `control` to `address` as `mode`, heard on `port` (`None` for
/// OSC), on `controller`: none makes one, named after the port. A
/// controller that had no port takes the one heard.
#[allow(clippy::too_many_arguments)]
pub(super) async fn learned(
    pool: &SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
    controller: Option<&Controller>,
    port: Option<&str>,
    control: &str,
    address: &Address,
    mode: Mode,
) -> Result<Binding, String> {
    let mut access = write(pool, venue_id, actor).await?;
    let controller_id = match controller {
        Some(controller) => {
            if let Some(port) = port.filter(|_| controller.port.is_empty()) {
                sqlx::query(
                    "UPDATE perform_controllers SET port = ? WHERE id = ? AND venue_id = ?",
                )
                .bind(port)
                .bind(&controller.id)
                .bind(venue_id)
                .execute(access.connection())
                .await
                .map_err(failed("change the controller"))?;
            }
            controller.id.clone()
        }
        None => {
            insert_controller(
                &mut access,
                port.unwrap_or(UNNAMED),
                port.unwrap_or_default(),
            )
            .await?
            .id
        }
    };
    let binding = upsert(&mut access, &controller_id, control, address, mode, false).await?;
    access.commit().await?;
    Ok(binding)
}

pub(super) async fn update_binding(
    pool: &SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
    id: &str,
    mode: Mode,
    invert: bool,
) -> Result<(), String> {
    let mut access = write(pool, venue_id, actor).await?;
    let done = sqlx::query(
        "UPDATE control_bindings SET mode = ?, invert = ? WHERE id = ? AND venue_id = ?",
    )
    .bind(mode_name(mode))
    .bind(i64::from(invert))
    .bind(id)
    .bind(venue_id)
    .execute(access.connection())
    .await
    .map_err(failed("change the binding"))?;
    if done.rows_affected() == 0 {
        return Err("No such binding".into());
    }
    access.commit().await
}

pub(super) async fn unbind(
    pool: &SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
    id: &str,
) -> Result<(), String> {
    let mut access = write(pool, venue_id, actor).await?;
    deletes::delete_where(
        access.connection(),
        "control_bindings",
        "id = ? AND venue_id = ?",
        &[id, venue_id],
    )
    .await?;
    access.commit().await
}

pub(super) async fn add_controller(
    pool: &SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
    name: &str,
    port: &str,
) -> Result<Controller, String> {
    let mut access = write(pool, venue_id, actor).await?;
    let controller = insert_controller(&mut access, name, port).await?;
    access.commit().await?;
    Ok(controller)
}

/// Store `controller` as it now is: its name, port and profile.
pub(super) async fn save_controller(
    pool: &SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
    controller: &Controller,
) -> Result<(), String> {
    let profile = serde_json::to_string(&controller.profile).map_err(|error| error.to_string())?;
    let mut access = write(pool, venue_id, actor).await?;
    let done = sqlx::query(
        "UPDATE perform_controllers SET name = ?, port = ?, profile_json = ?
         WHERE id = ? AND venue_id = ?",
    )
    .bind(&controller.name)
    .bind(&controller.port)
    .bind(profile)
    .bind(&controller.id)
    .bind(venue_id)
    .execute(access.connection())
    .await
    .map_err(failed("change the controller"))?;
    if done.rows_affected() == 0 {
        return Err("No such controller".into());
    }
    access.commit().await
}

/// Remove controller `id` and every binding it holds.
pub(super) async fn remove_controller(
    pool: &SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
    id: &str,
) -> Result<(), String> {
    let mut access = write(pool, venue_id, actor).await?;
    let removed = deletes::delete_where(
        access.connection(),
        "perform_controllers",
        "id = ? AND venue_id = ?",
        &[id, venue_id],
    )
    .await?;
    if removed == 0 {
        return Err("No such controller".into());
    }
    access.commit().await
}

/// Put `bound` on controller `id` in place of every binding it held at
/// `addresses`, in one change. Answers what it bound.
pub(super) async fn remap(
    pool: &SqlitePool,
    venue_id: &str,
    actor: Option<&str>,
    id: &str,
    addresses: &[Address],
    bound: &[(String, Address, Mode)],
) -> Result<Vec<Binding>, String> {
    let mut access = write(pool, venue_id, actor).await?;
    for address in addresses {
        let json = serde_json::to_string(address).map_err(|error| error.to_string())?;
        deletes::delete_where(
            access.connection(),
            "control_bindings",
            "controller_id = ? AND address_json = ? AND venue_id = ?",
            &[id, &json, venue_id],
        )
        .await?;
    }
    let mut bindings = Vec::new();
    for (control, address, mode) in bound {
        bindings.push(upsert(&mut access, id, control, address, *mode, false).await?);
    }
    access.commit().await?;
    Ok(bindings)
}

/// Move the bindings this machine kept before controllers synced
/// (`control_bindings_unsynced`) into a controller of `venue_id`, one
/// change that uploads like any other. They never knew their port, so the
/// controller is the unnamed one and takes a port when it is next learned
/// on. Nothing to move, nothing changes.
pub(super) async fn adopt(pool: &SqlitePool, venue_id: &str) -> Result<(), String> {
    let kept: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM control_bindings_unsynced WHERE venue_id = ?)",
    )
    .bind(venue_id)
    .fetch_one(pool)
    .await
    .map_err(failed("read the bindings kept here"))?;
    if !kept {
        return Ok(());
    }
    let mut access = write(pool, venue_id, None).await?;
    let rows = sqlx::query(
        "SELECT control, address_json, mode, invert FROM control_bindings_unsynced
         WHERE venue_id = ? ORDER BY created_at, id",
    )
    .bind(venue_id)
    .fetch_all(access.connection())
    .await
    .map_err(failed("read the bindings kept here"))?;
    if rows.is_empty() {
        return Ok(());
    }
    let controller = insert_controller(&mut access, UNNAMED, "").await?;
    for row in rows {
        let Ok(address) = serde_json::from_str::<Address>(row.get("address_json")) else {
            continue;
        };
        upsert(
            &mut access,
            &controller.id,
            row.get("control"),
            &address,
            mode_of(row.get("mode")),
            row.get::<i64, _>("invert") != 0,
        )
        .await?;
    }
    sqlx::query("DELETE FROM control_bindings_unsynced WHERE venue_id = ?")
        .bind(venue_id)
        .execute(access.connection())
        .await
        .map_err(failed("forget the bindings kept here"))?;
    access.commit().await
}
