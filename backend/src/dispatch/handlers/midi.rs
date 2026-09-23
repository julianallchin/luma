use crate::database::local::midi as midi_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::{AppServices, CommandError};
use crate::models::midi::{
    CreateBindingInput, CreateModifierInput, MidiBinding, ModifierDef, UpdateBindingInput,
};
use crate::services::groups::GroupSources;

// ============================================================================
// Modifier CRUD
// ============================================================================

pub async fn midi_list_modifiers(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<ModifierDef>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(midi_db::list_modifiers(&mut access).await?)
}

pub async fn midi_create_modifier(
    services: &AppServices,
    input: CreateModifierInput,
) -> Result<ModifierDef, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&input.venue_id)).await?;
    let modifier = midi_db::create_modifier(&mut access, input).await?;
    access.commit().await?;
    Ok(modifier)
}

pub async fn midi_delete_modifier(services: &AppServices, id: String) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::MidiModifier(&id)).await?;
    require_changed(midi_db::delete_modifier(&mut access, &id).await?)?;
    Ok(access.commit().await?)
}

// ============================================================================
// Binding CRUD
// ============================================================================

pub async fn midi_list_bindings(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<MidiBinding>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(midi_db::list_bindings(&mut access).await?)
}

pub async fn midi_create_binding(
    services: &AppServices,
    input: CreateBindingInput,
) -> Result<MidiBinding, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&input.venue_id)).await?;
    let binding = midi_db::create_binding(&mut access, input).await?;
    access.commit().await?;
    Ok(binding)
}

pub async fn midi_update_binding(
    services: &AppServices,
    input: UpdateBindingInput,
) -> Result<MidiBinding, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::MidiBinding(&input.id)).await?;
    let binding = midi_db::update_binding(&mut access, input).await?;
    access.commit().await?;
    Ok(binding)
}

pub async fn midi_delete_binding(services: &AppServices, id: String) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::MidiBinding(&id)).await?;
    require_changed(midi_db::delete_binding(&mut access, &id).await?)?;
    Ok(access.commit().await?)
}

// ============================================================================
// Mapping reload
// ============================================================================

/// Rebuild `ControllerMappingSnapshot` from the database. Call after any CRUD
/// change to bindings or modifiers, or on venue load.
pub async fn midi_reload_mapping(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    // The group map is the merged tree, which needs the venue's graph.
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let modifiers = midi_db::list_modifiers(&mut access).await?;
    let bindings = midi_db::list_bindings(&mut access).await?;
    services.controller.reload_mapping(modifiers, bindings);

    // Refresh the group→fixture map that per-group intensity dims through. The
    // merged read, so a binding can name a derived set as well as an authored row.
    let group_map = GroupSources::read(&services.fixtures_root, &mut access)
        .await?
        .member_keys();
    drop(access);
    services.render_engine.set_group_fixture_map(group_map);
    Ok(())
}

/// A venue-scoped write that matched no row is a missing resource, not a
/// silent no-op: a delete fails loudly.
fn require_changed(rows_affected: u64) -> Result<(), CommandError> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(CommandError::NotFound("Venue resource not found".into()))
    }
}
