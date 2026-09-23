use sqlx::FromRow;
use uuid::Uuid;

use crate::database::local::deletes;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::midi::{
    CreateBindingInput, CreateModifierInput, MidiBinding, ModifierDef, UpdateBindingInput,
    UpdateModifierInput,
};

// ============================================================================
// JSON helpers
// ============================================================================

fn to_json<T: serde::Serialize>(val: &T) -> Result<String, String> {
    serde_json::to_string(val).map_err(|e| format!("serialize: {}", e))
}

fn from_json<T: for<'de> serde::Deserialize<'de>>(s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(|e| format!("deserialize '{}': {}", s, e))
}

// ============================================================================
// Row types
// ============================================================================

#[derive(FromRow)]
struct ModifierRow {
    id: String,
    uid: Option<String>,
    venue_id: String,
    name: String,
    input_json: String,
    groups_json: Option<String>,
    created_at: String,
    updated_at: String,
}

impl ModifierRow {
    fn into_modifier(self) -> Result<ModifierDef, String> {
        Ok(ModifierDef {
            id: self.id,
            uid: self.uid,
            venue_id: self.venue_id,
            name: self.name,
            input: from_json(&self.input_json)?,
            groups: self.groups_json.as_deref().map(from_json).transpose()?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow)]
struct BindingRow {
    id: String,
    uid: Option<String>,
    venue_id: String,
    trigger_json: String,
    required_modifiers_json: String,
    exclusive: i64,
    mode_json: String,
    action_json: String,
    target_override_json: Option<String>,
    display_order: i64,
    created_at: String,
    updated_at: String,
}

impl BindingRow {
    fn into_binding(self) -> Result<MidiBinding, String> {
        Ok(MidiBinding {
            id: self.id,
            uid: self.uid,
            venue_id: self.venue_id,
            trigger: from_json(&self.trigger_json)?,
            required_modifiers: from_json(&self.required_modifiers_json)?,
            exclusive: self.exclusive != 0,
            mode: from_json(&self.mode_json)?,
            action: from_json(&self.action_json)?,
            target_override: self
                .target_override_json
                .as_deref()
                .map(from_json)
                .transpose()?,
            display_order: self.display_order,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

// ============================================================================
// Modifiers
// ============================================================================

pub async fn list_modifiers(access: &mut impl AuthorizedVenue) -> Result<Vec<ModifierDef>, String> {
    sqlx::query_as::<_, ModifierRow>(
        "SELECT id, uid, venue_id, name, input_json, groups_json, created_at, updated_at
         FROM midi_modifiers WHERE venue_id = ? ORDER BY name ASC",
    )
    .bind(access.venue_id().to_owned())
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("list_modifiers: {}", e))?
    .into_iter()
    .map(|r| r.into_modifier())
    .collect()
}

pub async fn get_modifier(
    access: &mut impl AuthorizedVenue,
    id: &str,
) -> Result<ModifierDef, String> {
    sqlx::query_as::<_, ModifierRow>(
        "SELECT id, uid, venue_id, name, input_json, groups_json, created_at, updated_at
         FROM midi_modifiers WHERE id = ? AND venue_id = ?",
    )
    .bind(id)
    .bind(access.venue_id().to_owned())
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|e| format!("get_modifier: {}", e))?
    .into_modifier()
}

pub async fn create_modifier(
    access: &mut VenueAccess<'_, Write>,
    input: CreateModifierInput,
) -> Result<ModifierDef, String> {
    access.require_venue(&input.venue_id)?;
    let id = Uuid::new_v4().to_string();
    let input_json = to_json(&input.input)?;
    let groups_json: Option<String> = input.groups.as_ref().map(to_json).transpose()?;

    sqlx::query(
        "INSERT INTO midi_modifiers (id, uid, venue_id, name, input_json, groups_json)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(access.principal().map(str::to_owned))
    .bind(access.venue_id().to_owned())
    .bind(&input.name)
    .bind(&input_json)
    .bind(&groups_json)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("create_modifier: {}", e))?;

    get_modifier(access, &id).await
}

pub async fn update_modifier(
    access: &mut VenueAccess<'_, Write>,
    input: UpdateModifierInput,
) -> Result<ModifierDef, String> {
    let existing = get_modifier(access, &input.id).await?;
    let midi_input = input.input.unwrap_or(existing.input);
    let input_json = to_json(&midi_input)?;
    let groups = match input.groups {
        Some(g) => g,
        None => existing.groups,
    };
    let groups_json: Option<String> = groups.as_ref().map(to_json).transpose()?;

    sqlx::query(
        "UPDATE midi_modifiers SET name = ?, input_json = ?, groups_json = ?
         WHERE id = ? AND venue_id = ?",
    )
    .bind(input.name.unwrap_or(existing.name))
    .bind(&input_json)
    .bind(&groups_json)
    .bind(&input.id)
    .bind(access.venue_id().to_owned())
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("update_modifier: {}", e))?;

    get_modifier(access, &input.id).await
}

pub async fn delete_modifier(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let deleted = deletes::delete_where(
        access.connection(),
        "midi_modifiers",
        "id = ? AND venue_id = ?",
        &[id, &venue_id],
    )
    .await
    .map_err(|e| format!("delete_modifier: {}", e))?;
    Ok(deleted as u64)
}

// ============================================================================
// Bindings
// ============================================================================

pub async fn list_bindings(access: &mut impl AuthorizedVenue) -> Result<Vec<MidiBinding>, String> {
    sqlx::query_as::<_, BindingRow>(
        "SELECT id, uid, venue_id, trigger_json, required_modifiers_json, exclusive, mode_json,
                action_json, target_override_json, display_order, created_at, updated_at
         FROM midi_bindings WHERE venue_id = ? ORDER BY display_order ASC",
    )
    .bind(access.venue_id().to_owned())
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("list_bindings: {}", e))?
    .into_iter()
    .map(|r| r.into_binding())
    .collect()
}

pub async fn get_binding(
    access: &mut impl AuthorizedVenue,
    id: &str,
) -> Result<MidiBinding, String> {
    sqlx::query_as::<_, BindingRow>(
        "SELECT id, uid, venue_id, trigger_json, required_modifiers_json, exclusive, mode_json,
                action_json, target_override_json, display_order, created_at, updated_at
         FROM midi_bindings WHERE id = ? AND venue_id = ?",
    )
    .bind(id)
    .bind(access.venue_id().to_owned())
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|e| format!("get_binding: {}", e))?
    .into_binding()
}

pub async fn create_binding(
    access: &mut VenueAccess<'_, Write>,
    input: CreateBindingInput,
) -> Result<MidiBinding, String> {
    access.require_venue(&input.venue_id)?;
    let id = Uuid::new_v4().to_string();
    let trigger_json = to_json(&input.trigger)?;
    let required_modifiers_json = to_json(&input.required_modifiers)?;
    let exclusive: i64 = if input.exclusive { 1 } else { 0 };
    let mode_json = to_json(&input.mode.unwrap_or_default())?;
    let action_json = to_json(&input.action)?;
    let target_override_json: Option<String> =
        input.target_override.as_ref().map(to_json).transpose()?;

    sqlx::query(
        "INSERT INTO midi_bindings
             (id, uid, venue_id, trigger_json, required_modifiers_json, exclusive, mode_json,
              action_json, target_override_json, display_order)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(access.principal().map(str::to_owned))
    .bind(access.venue_id().to_owned())
    .bind(&trigger_json)
    .bind(&required_modifiers_json)
    .bind(exclusive)
    .bind(&mode_json)
    .bind(&action_json)
    .bind(&target_override_json)
    .bind(input.display_order)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("create_binding: {}", e))?;

    get_binding(access, &id).await
}

pub async fn update_binding(
    access: &mut VenueAccess<'_, Write>,
    input: UpdateBindingInput,
) -> Result<MidiBinding, String> {
    let existing = get_binding(access, &input.id).await?;
    let trigger_json = to_json(&input.trigger.unwrap_or(existing.trigger))?;
    let required_modifiers_json = to_json(
        &input
            .required_modifiers
            .unwrap_or(existing.required_modifiers),
    )?;
    let exclusive_i: i64 = if input.exclusive.unwrap_or(existing.exclusive) {
        1
    } else {
        0
    };
    let mode_json = to_json(&input.mode.unwrap_or(existing.mode))?;
    let action_json = to_json(&input.action.unwrap_or(existing.action))?;
    let target_override = match input.target_override {
        Some(t) => t,
        None => existing.target_override,
    };
    let target_override_json: Option<String> = target_override.as_ref().map(to_json).transpose()?;
    let display_order = input.display_order.unwrap_or(existing.display_order);

    sqlx::query(
        "UPDATE midi_bindings SET trigger_json = ?, required_modifiers_json = ?,
                                  exclusive = ?, mode_json = ?, action_json = ?,
                                  target_override_json = ?, display_order = ?
         WHERE id = ? AND venue_id = ?",
    )
    .bind(&trigger_json)
    .bind(&required_modifiers_json)
    .bind(exclusive_i)
    .bind(&mode_json)
    .bind(&action_json)
    .bind(&target_override_json)
    .bind(display_order)
    .bind(&input.id)
    .bind(access.venue_id().to_owned())
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("update_binding: {}", e))?;

    get_binding(access, &input.id).await
}

pub async fn delete_binding(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let deleted = deletes::delete_where(
        access.connection(),
        "midi_bindings",
        "id = ? AND venue_id = ?",
        &[id, &venue_id],
    )
    .await
    .map_err(|e| format!("delete_binding: {}", e))?;
    Ok(deleted as u64)
}
