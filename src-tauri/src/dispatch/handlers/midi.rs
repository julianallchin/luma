use std::collections::HashMap;

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::universe::UniverseState;

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
