use serde_json::Value;
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::models::midi::{
    CreateBindingInput, CreateCueInput, CreateModifierInput, Cue, CueExecutionMode, MidiBinding,
    ModifierDef, Target, UpdateBindingInput, UpdateCueInput, UpdateModifierInput,
};
use crate::models::node_graph::BlendMode;

// ============================================================================
// JSON helpers
// ============================================================================

fn to_json<T: serde::Serialize>(val: &T) -> Result<String, String> {
    serde_json::to_string(val).map_err(|e| format!("serialize: {}", e))
}

fn from_json<T: for<'de> serde::Deserialize<'de>>(s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(|e| format!("deserialize '{}': {}", s, e))
}

fn blend_mode_from_str(s: &str) -> Result<BlendMode, String> {
    serde_json::from_str(&format!("\"{}\"", s)).map_err(|e| format!("blend_mode '{}': {}", s, e))
}

// ============================================================================
// Row types
// ============================================================================

#[derive(FromRow)]
struct CueRow {
    id: String,
    venue_id: String,
    name: String,
    pattern_id: String,
    args_json: String,
    z_index: i64,
    blend_mode: String,
    default_target_json: String,
    execution_mode_json: String,
    display_x: i64,
    display_y: i64,
    created_at: String,
    updated_at: String,
}

impl CueRow {
    fn into_cue(self) -> Result<Cue, String> {
        Ok(Cue {
            id: self.id,
            venue_id: self.venue_id,
            name: self.name,
            pattern_id: self.pattern_id,
            args: from_json(&self.args_json)?,
            z_index: self.z_index,
            blend_mode: blend_mode_from_str(&self.blend_mode)?,
            default_target: from_json(&self.default_target_json)?,
            execution_mode: from_json(&self.execution_mode_json)?,
            display_x: self.display_x,
            display_y: self.display_y,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow)]
struct ModifierRow {
    id: String,
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
// Cues
// ============================================================================

pub async fn list_cues(pool: &SqlitePool, venue_id: &str) -> Result<Vec<Cue>, String> {
    sqlx::query_as::<_, CueRow>(
        "SELECT id, venue_id, name, pattern_id, args_json, z_index, blend_mode,
                default_target_json, execution_mode_json, display_x, display_y, created_at, updated_at
         FROM cues WHERE venue_id = ? ORDER BY display_y ASC, display_x ASC, name ASC",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("list_cues: {}", e))?
    .into_iter()
    .map(|r| r.into_cue())
    .collect()
}

pub async fn get_cue(pool: &SqlitePool, id: &str) -> Result<Cue, String> {
    sqlx::query_as::<_, CueRow>(
        "SELECT id, venue_id, name, pattern_id, args_json, z_index, blend_mode,
                default_target_json, execution_mode_json, display_x, display_y, created_at, updated_at
         FROM cues WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("get_cue: {}", e))?
    .into_cue()
}

pub async fn create_cue(pool: &SqlitePool, input: CreateCueInput) -> Result<Cue, String> {
    let id = Uuid::new_v4().to_string();
    let args = input.args.unwrap_or(Value::Object(Default::default()));
    let args_json = to_json(&args)?;
    let z_index = input.z_index.unwrap_or(1);
    let blend_mode = input.blend_mode.unwrap_or(BlendMode::Replace);
    let blend_mode_str = to_json(&blend_mode)?.trim_matches('"').to_string();
    let default_target_json = to_json(&input.default_target.unwrap_or(Target::All))?;
    let execution_mode_json = to_json(
        &input
            .execution_mode
            .unwrap_or(CueExecutionMode::Loop { bars: 4 }),
    )?;
    let display_x = input.display_x.unwrap_or(0);
    let display_y = input.display_y.unwrap_or(0);

    sqlx::query(
        "INSERT INTO cues (id, venue_id, name, pattern_id, args_json, z_index, blend_mode,
                           default_target_json, execution_mode_json, display_x, display_y)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.venue_id)
    .bind(&input.name)
    .bind(&input.pattern_id)
    .bind(&args_json)
    .bind(z_index)
    .bind(&blend_mode_str)
    .bind(&default_target_json)
    .bind(&execution_mode_json)
    .bind(display_x)
    .bind(display_y)
    .execute(pool)
    .await
    .map_err(|e| format!("create_cue: {}", e))?;

    get_cue(pool, &id).await
}

pub async fn update_cue(pool: &SqlitePool, input: UpdateCueInput) -> Result<Cue, String> {
    let existing = get_cue(pool, &input.id).await?;
    let args_json = to_json(&input.args.unwrap_or(existing.args))?;
    let z_index = input.z_index.unwrap_or(existing.z_index);
    let blend_mode_str = to_json(&input.blend_mode.unwrap_or(existing.blend_mode))?
        .trim_matches('"')
        .to_string();
    let default_target_json = to_json(&input.default_target.unwrap_or(existing.default_target))?;
    let execution_mode_json = to_json(&input.execution_mode.unwrap_or(existing.execution_mode))?;
    let display_x = input.display_x.unwrap_or(existing.display_x);
    let display_y = input.display_y.unwrap_or(existing.display_y);

    sqlx::query(
        "UPDATE cues SET name = ?, pattern_id = ?, args_json = ?, z_index = ?, blend_mode = ?,
                         default_target_json = ?, execution_mode_json = ?, display_x = ?, display_y = ?,
                         updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&input.name.unwrap_or(existing.name))
    .bind(&input.pattern_id.unwrap_or(existing.pattern_id))
    .bind(&args_json)
    .bind(z_index)
    .bind(&blend_mode_str)
    .bind(&default_target_json)
    .bind(&execution_mode_json)
    .bind(display_x)
    .bind(display_y)
    .bind(&input.id)
    .execute(pool)
    .await
    .map_err(|e| format!("update_cue: {}", e))?;

    get_cue(pool, &input.id).await
}

pub async fn delete_cue(pool: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM cues WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("delete_cue: {}", e))?;
    Ok(())
}

// ============================================================================
// Modifiers
// ============================================================================

pub async fn list_modifiers(pool: &SqlitePool, venue_id: &str) -> Result<Vec<ModifierDef>, String> {
    sqlx::query_as::<_, ModifierRow>(
        "SELECT id, venue_id, name, input_json, groups_json, created_at, updated_at
         FROM midi_modifiers WHERE venue_id = ? ORDER BY name ASC",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("list_modifiers: {}", e))?
    .into_iter()
    .map(|r| r.into_modifier())
    .collect()
}

pub async fn get_modifier(pool: &SqlitePool, id: &str) -> Result<ModifierDef, String> {
    sqlx::query_as::<_, ModifierRow>(
        "SELECT id, venue_id, name, input_json, groups_json, created_at, updated_at
         FROM midi_modifiers WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("get_modifier: {}", e))?
    .into_modifier()
}

pub async fn create_modifier(
    pool: &SqlitePool,
    input: CreateModifierInput,
) -> Result<ModifierDef, String> {
    let id = Uuid::new_v4().to_string();
    let input_json = to_json(&input.input)?;
    let groups_json: Option<String> = input.groups.as_ref().map(to_json).transpose()?;

    sqlx::query(
        "INSERT INTO midi_modifiers (id, venue_id, name, input_json, groups_json)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.venue_id)
    .bind(&input.name)
    .bind(&input_json)
    .bind(&groups_json)
    .execute(pool)
    .await
    .map_err(|e| format!("create_modifier: {}", e))?;

    get_modifier(pool, &id).await
}

pub async fn update_modifier(
    pool: &SqlitePool,
    input: UpdateModifierInput,
) -> Result<ModifierDef, String> {
    let existing = get_modifier(pool, &input.id).await?;
    let midi_input = input.input.unwrap_or(existing.input);
    let input_json = to_json(&midi_input)?;
    let groups = match input.groups {
        Some(g) => g,
        None => existing.groups,
    };
    let groups_json: Option<String> = groups.as_ref().map(to_json).transpose()?;

    sqlx::query(
        "UPDATE midi_modifiers SET name = ?, input_json = ?, groups_json = ?,
                                   updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&input.name.unwrap_or(existing.name))
    .bind(&input_json)
    .bind(&groups_json)
    .bind(&input.id)
    .execute(pool)
    .await
    .map_err(|e| format!("update_modifier: {}", e))?;

    get_modifier(pool, &input.id).await
}

pub async fn delete_modifier(pool: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM midi_modifiers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("delete_modifier: {}", e))?;
    Ok(())
}

// ============================================================================
// Bindings
// ============================================================================

pub async fn list_bindings(pool: &SqlitePool, venue_id: &str) -> Result<Vec<MidiBinding>, String> {
    sqlx::query_as::<_, BindingRow>(
        "SELECT id, venue_id, trigger_json, required_modifiers_json, exclusive, mode_json,
                action_json, target_override_json, display_order, created_at, updated_at
         FROM midi_bindings WHERE venue_id = ? ORDER BY display_order ASC",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("list_bindings: {}", e))?
    .into_iter()
    .map(|r| r.into_binding())
    .collect()
}

pub async fn get_binding(pool: &SqlitePool, id: &str) -> Result<MidiBinding, String> {
    sqlx::query_as::<_, BindingRow>(
        "SELECT id, venue_id, trigger_json, required_modifiers_json, exclusive, mode_json,
                action_json, target_override_json, display_order, created_at, updated_at
         FROM midi_bindings WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("get_binding: {}", e))?
    .into_binding()
}

pub async fn create_binding(
    pool: &SqlitePool,
    input: CreateBindingInput,
) -> Result<MidiBinding, String> {
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
             (id, venue_id, trigger_json, required_modifiers_json, exclusive, mode_json,
              action_json, target_override_json, display_order)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.venue_id)
    .bind(&trigger_json)
    .bind(&required_modifiers_json)
    .bind(exclusive)
    .bind(&mode_json)
    .bind(&action_json)
    .bind(&target_override_json)
    .bind(input.display_order)
    .execute(pool)
    .await
    .map_err(|e| format!("create_binding: {}", e))?;

    get_binding(pool, &id).await
}

pub async fn update_binding(
    pool: &SqlitePool,
    input: UpdateBindingInput,
) -> Result<MidiBinding, String> {
    let existing = get_binding(pool, &input.id).await?;
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
                                  target_override_json = ?, display_order = ?,
                                  updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&trigger_json)
    .bind(&required_modifiers_json)
    .bind(exclusive_i)
    .bind(&mode_json)
    .bind(&action_json)
    .bind(&target_override_json)
    .bind(display_order)
    .bind(&input.id)
    .execute(pool)
    .await
    .map_err(|e| format!("update_binding: {}", e))?;

    get_binding(pool, &input.id).await
}

pub async fn delete_binding(pool: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM midi_bindings WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("delete_binding: {}", e))?;
    Ok(())
}

// ============================================================================
// Group→fixture map for target filtering
// ============================================================================

#[derive(FromRow)]
struct GroupFixturePair {
    group_id: String,
    fixture_id: String,
}

/// Returns HashMap<group_id, Vec<fixture_id>> for all groups in a venue.
pub async fn get_group_fixture_map(
    pool: &SqlitePool,
    venue_id: &str,
) -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    sqlx::query_as::<_, GroupFixturePair>(
        "SELECT g.id as group_id, m.fixture_id
         FROM fixture_groups g
         JOIN fixture_group_members m ON m.group_id = g.id
         WHERE g.venue_id = ?",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("get_group_fixture_map: {}", e))?
    .into_iter()
    .fold(
        Ok(std::collections::HashMap::new()),
        |acc: Result<std::collections::HashMap<String, Vec<String>>, String>, r| {
            let mut map = acc?;
            map.entry(r.group_id).or_default().push(r.fixture_id);
            Ok(map)
        },
    )
}
