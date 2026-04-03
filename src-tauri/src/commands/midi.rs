use tauri::{AppHandle, Emitter, State};

use crate::audio::{FftService, StemCache};
use crate::controller_manager::ControllerManager;
use crate::database::local::auth;
use crate::database::local::midi as midi_db;
use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::models::midi::{
    CreateBindingInput, CreateCueInput, CreateModifierInput, Cue, MidiBinding, ModifierDef,
    UpdateBindingInput, UpdateCueInput, UpdateModifierInput,
};
use crate::render_engine::{RenderEngine, ResolvedTarget};

// ============================================================================
// Cue CRUD
// ============================================================================

#[tauri::command]
pub async fn midi_list_cues(db: State<'_, Db>, venue_id: String) -> Result<Vec<Cue>, String> {
    midi_db::list_cues(&db.0, &venue_id).await
}

#[tauri::command]
pub async fn midi_create_cue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    input: CreateCueInput,
) -> Result<Cue, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    midi_db::create_cue(&db.0, input, uid.as_deref()).await
}

#[tauri::command]
pub async fn midi_update_cue(db: State<'_, Db>, input: UpdateCueInput) -> Result<Cue, String> {
    midi_db::update_cue(&db.0, input).await
}

#[tauri::command]
pub async fn midi_delete_cue(
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    id: String,
) -> Result<(), String> {
    midi_db::delete_cue(&db.0, &id).await?;
    render_engine.remove_cue_buffers(&id);
    Ok(())
}

// ============================================================================
// Modifier CRUD
// ============================================================================

#[tauri::command]
pub async fn midi_list_modifiers(
    db: State<'_, Db>,
    venue_id: String,
) -> Result<Vec<ModifierDef>, String> {
    midi_db::list_modifiers(&db.0, &venue_id).await
}

#[tauri::command]
pub async fn midi_create_modifier(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    input: CreateModifierInput,
) -> Result<ModifierDef, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    midi_db::create_modifier(&db.0, input, uid.as_deref()).await
}

#[tauri::command]
pub async fn midi_update_modifier(
    db: State<'_, Db>,
    input: UpdateModifierInput,
) -> Result<ModifierDef, String> {
    midi_db::update_modifier(&db.0, input).await
}

#[tauri::command]
pub async fn midi_delete_modifier(db: State<'_, Db>, id: String) -> Result<(), String> {
    midi_db::delete_modifier(&db.0, &id).await
}

// ============================================================================
// Binding CRUD
// ============================================================================

#[tauri::command]
pub async fn midi_list_bindings(
    db: State<'_, Db>,
    venue_id: String,
) -> Result<Vec<MidiBinding>, String> {
    midi_db::list_bindings(&db.0, &venue_id).await
}

#[tauri::command]
pub async fn midi_create_binding(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    input: CreateBindingInput,
) -> Result<MidiBinding, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    midi_db::create_binding(&db.0, input, uid.as_deref()).await
}

#[tauri::command]
pub async fn midi_update_binding(
    db: State<'_, Db>,
    input: UpdateBindingInput,
) -> Result<MidiBinding, String> {
    midi_db::update_binding(&db.0, input).await
}

#[tauri::command]
pub async fn midi_delete_binding(db: State<'_, Db>, id: String) -> Result<(), String> {
    midi_db::delete_binding(&db.0, &id).await
}

/// Rebuild ControllerMappingSnapshot from DB and recompile cues onto the simulated deck.
/// Call after any CRUD change to cues, bindings, or modifiers, or on venue load.
#[tauri::command]
pub async fn midi_reload_mapping(
    app: AppHandle,
    db: State<'_, Db>,
    controller: State<'_, ControllerManager>,
    render_engine: State<'_, RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    venue_id: String,
) -> Result<(), String> {
    let cues = midi_db::list_cues(&db.0, &venue_id).await?;
    let modifiers = midi_db::list_modifiers(&db.0, &venue_id).await?;
    let bindings = midi_db::list_bindings(&db.0, &venue_id).await?;
    eprintln!(
        "[midi_reload_mapping] venue={venue_id} cues={} modifiers={} bindings={}",
        cues.len(),
        modifiers.len(),
        bindings.len()
    );
    controller.reload_mapping(cues, modifiers, bindings);

    // Refresh group→fixture map so target resolution stays in sync
    let group_map = midi_db::get_group_fixture_map(&db.0, &venue_id).await?;
    eprintln!(
        "[midi_reload_mapping] group_map entries={}",
        group_map.len()
    );
    render_engine.set_group_fixture_map(group_map);

    // Compile cue buffers onto the simulated deck so MIDI pads work without a real DJ deck
    let resource_path = crate::services::fixtures::resolve_fixtures_root(&app).ok();
    eprintln!("[midi_reload_mapping] resource_path={:?}", resource_path);
    match crate::controller_compositor::compile_cues_for_simulated_deck(
        &db.0,
        &stem_cache,
        &fft_service,
        resource_path,
        &render_engine,
        &venue_id,
    )
    .await
    {
        Ok(()) => eprintln!("[midi_reload_mapping] sim deck compile OK"),
        Err(e) => eprintln!("[midi_reload_mapping] sim deck compile error: {e}"),
    }

    // Log what's now in cue_buffers
    {
        let arc = render_engine.inner_arc();
        let guard = arc.lock().expect("poisoned");
        eprintln!(
            "[midi_reload_mapping] cue_buffers after compile: {:?}",
            guard.cue_buffers.keys().collect::<Vec<_>>()
        );
    }

    Ok(())
}

// ============================================================================
// Manual Layer Control (fire without MIDI hardware — for UI test buttons)
// ============================================================================

#[tauri::command]
pub async fn midi_fire_cue(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    cue_id: String,
    target_override: Option<crate::models::midi::Target>,
) -> Result<(), String> {
    let cue = midi_db::get_cue(&db.0, &cue_id).await?;
    eprintln!(
        "[midi_fire_cue] cue_id={cue_id} blend_mode={:?} z_index={}",
        cue.blend_mode, cue.z_index
    );
    let resolved = match target_override.as_ref().unwrap_or(&cue.default_target) {
        crate::models::midi::Target::All => ResolvedTarget::All,
        crate::models::midi::Target::Explicit { groups } => ResolvedTarget::Groups(groups.clone()),
        crate::models::midi::Target::FromModifiers => ResolvedTarget::All, // fallback for UI
    };
    render_engine.latch_cue_on(&cue_id, resolved, cue.z_index as i8);
    {
        let arc = render_engine.inner_arc();
        let guard = arc.lock().expect("poisoned");
        eprintln!(
            "[midi_fire_cue] after latch: has_any_cues={} active_cues={:?} buffers_for_cue={:?}",
            guard.manual_layer.has_any_cues(),
            guard
                .manual_layer
                .global
                .active_cues
                .keys()
                .collect::<Vec<_>>(),
            guard
                .cue_buffers
                .keys()
                .filter(|(_, cid)| cid == &cue_id)
                .collect::<Vec<_>>(),
        );
    }
    let state = render_engine.get_manual_state_snapshot();
    let _ = app.emit("controller_state", &state);
    Ok(())
}

#[tauri::command]
pub fn midi_release_cue(
    app: AppHandle,
    render_engine: State<'_, RenderEngine>,
    cue_id: String,
) -> Result<(), String> {
    render_engine.latch_cue_off(&cue_id);
    render_engine.flash_cue_off(&cue_id);
    let state = render_engine.get_manual_state_snapshot();
    let _ = app.emit("controller_state", &state);
    // If no cues remain active, push a dark universe frame so the visualizer clears.
    {
        let arc = render_engine.inner_arc();
        let guard = arc.lock().expect("poisoned");
        if !guard.manual_layer.has_any_cues() && !guard.manual_layer.active {
            use std::collections::HashMap;
            let _ = app.emit(
                "universe-state-update",
                &crate::models::universe::UniverseState {
                    primitives: HashMap::new(),
                },
            );
        }
    }
    Ok(())
}

/// Compile all cues for a venue onto a deck.
/// Called automatically after render_composite_deck; also callable manually.
#[tauri::command]
pub async fn midi_compile_cues_for_deck(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    deck_id: u8,
    track_id: String,
    venue_id: String,
) -> Result<(), String> {
    let resource_path = crate::services::fixtures::resolve_fixtures_root(&app).ok();

    let group_map = midi_db::get_group_fixture_map(&db.0, &venue_id).await?;
    render_engine.set_group_fixture_map(group_map);

    crate::controller_compositor::compile_cues_for_deck(
        &db.0,
        &stem_cache,
        &fft_service,
        resource_path,
        &render_engine,
        deck_id,
        &track_id,
        &venue_id,
    )
    .await
}
