use std::collections::HashMap;

use crate::database::local::midi as midi_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::{AppServices, CommandError};
use crate::models::midi::{
    CreateBindingInput, CreateCueInput, CreateModifierInput, Cue, MidiBinding, ModifierDef, Target,
    UpdateBindingInput, UpdateCueInput,
};
use crate::models::universe::UniverseState;
use crate::render_engine::ResolvedTarget;
use crate::services::groups::GroupSources;

// ============================================================================
// Cue CRUD
// ============================================================================

pub async fn midi_list_cues(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<Cue>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(midi_db::list_cues(&mut access).await?)
}

pub async fn midi_create_cue(
    services: &AppServices,
    input: CreateCueInput,
) -> Result<Cue, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&input.venue_id)).await?;
    let cue = midi_db::create_cue(&mut access, input).await?;
    access.commit().await?;
    Ok(cue)
}

pub async fn midi_update_cue(
    services: &AppServices,
    input: UpdateCueInput,
) -> Result<Cue, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Cue(&input.id)).await?;
    let cue = midi_db::update_cue(&mut access, input).await?;
    access.commit().await?;
    Ok(cue)
}

pub async fn midi_delete_cue(services: &AppServices, id: String) -> Result<(), CommandError> {
    let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Cue(&id)).await?;
    require_changed(midi_db::delete_cue(&mut access, &id).await?)?;
    access.commit().await?;
    services.render_engine.remove_cue_buffers(&id);
    Ok(())
}

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
// Mapping reload / cue compilation
// ============================================================================

/// Rebuild `ControllerMappingSnapshot` from the database and recompile cues
/// onto the simulated deck. Call after any CRUD change to cues, bindings, or
/// modifiers, or on venue load.
pub async fn midi_reload_mapping(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    // The group map is the merged tree, which needs the venue's graph.
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let cues = midi_db::list_cues(&mut access).await?;
    let modifiers = midi_db::list_modifiers(&mut access).await?;
    let bindings = midi_db::list_bindings(&mut access).await?;
    services
        .controller
        .reload_mapping(cues, modifiers, bindings);

    // Refresh group→fixture map so target resolution stays in sync. The merged
    // read, so a binding can name a derived set as well as an authored row.
    let group_map = GroupSources::read(&services.fixtures_root, &mut access)
        .await?
        .member_keys();
    drop(access);
    services.render_engine.set_group_fixture_map(group_map);

    // Compile cue buffers onto the simulated deck so MIDI pads work without a
    // real DJ deck. A compile failure is diagnostic, not fatal: the mapping
    // itself is already live.
    if let Err(error) = crate::controller_compositor::compile_cues_for_simulated_deck(
        &services.db.0,
        &services.storage,
        Some(services.fixtures_root.clone()),
        &services.render_engine,
        &venue_id,
    )
    .await
    {
        log::warn!("[midi] simulated-deck cue compile for venue {venue_id}: {error}");
    }

    Ok(())
}

/// Compile all cues for a venue onto a deck. Called by the Perform page right
/// after the deck's composite is rendered.
pub async fn midi_compile_cues_for_deck(
    services: &AppServices,
    deck_id: u8,
    track_id: String,
    venue_id: String,
) -> Result<(), CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let group_map = GroupSources::read(&services.fixtures_root, &mut access)
        .await?
        .member_keys();
    drop(access);
    services.render_engine.set_group_fixture_map(group_map);

    Ok(crate::controller_compositor::compile_cues_for_deck(
        &services.db.0,
        &services.storage,
        Some(services.fixtures_root.clone()),
        &services.render_engine,
        deck_id,
        &track_id,
        &venue_id,
    )
    .await?)
}

// ============================================================================
// Manual layer control (fire/release without MIDI hardware)
// ============================================================================

pub async fn midi_fire_cue(
    services: &AppServices,
    cue_id: String,
    target_override: Option<Target>,
) -> Result<(), CommandError> {
    let mut access = VenueAccess::<Read>::read(&services.db.0, VenueResource::Cue(&cue_id)).await?;
    let cue = midi_db::get_cue(&mut access, &cue_id).await?;
    drop(access);
    let resolved = match target_override.as_ref().unwrap_or(&cue.default_target) {
        Target::All => ResolvedTarget::All,
        Target::Explicit { groups } => ResolvedTarget::Groups(groups.clone()),
        // The UI holds no modifiers, so a modifier-driven target has nothing to
        // resolve against; fall back to everything rather than to nothing.
        Target::FromModifiers => ResolvedTarget::All,
    };
    services
        .render_engine
        .latch_cue_on(&cue_id, resolved, cue.z_index as i8);
    let state = services.render_engine.get_manual_state_snapshot();
    services.events.emit("controller_state", &state);
    Ok(())
}

pub async fn midi_release_cue(services: &AppServices, cue_id: String) -> Result<(), CommandError> {
    let _access = VenueAccess::<Read>::read(&services.db.0, VenueResource::Cue(&cue_id)).await?;
    services.render_engine.latch_cue_off(&cue_id);
    services.render_engine.flash_cue_off(&cue_id);
    let state = services.render_engine.get_manual_state_snapshot();
    services.events.emit("controller_state", &state);
    // If no cues remain active, push a dark universe frame so the visualizer clears.
    let cleared = {
        let arc = services.render_engine.inner_arc();
        let guard = arc.lock().expect("poisoned");
        !guard.manual_layer.has_any_cues() && !guard.manual_layer.active
    };
    if cleared {
        services.events.emit(
            "universe-state-update",
            UniverseState {
                primitives: HashMap::new(),
            },
        );
    }
    Ok(())
}

/// A venue-scoped write that matched no row is a missing resource, not a
/// silent no-op — the frontend relies on delete failing loudly.
fn require_changed(rows_affected: u64) -> Result<(), CommandError> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(CommandError::NotFound("Venue resource not found".into()))
    }
}
