//! `luma.perform` — a venue's Perform set, for the Perform tab's chats: its
//! patterns, the top of the set first, its layout and the controls they play
//! under, as of the cell's start. `luma.perform`'s verbs answer it as it
//! stands after every change.

use super::{inline, unavailable, ProviderCtx};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::agent_execution::bindings::manifest::AgentKind;
use crate::agent_execution::spoken_groups::SpokenGroups;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::services::perform_set;

/// Why another thread has no Perform set.
const NOT_PERFORM: &str = "the Perform set is in scope only in the Perform tab's chats";

pub async fn provide(b: &mut BindingBuilder, ctx: &ProviderCtx<'_>) -> Result<(), String> {
    let venue_id = match (ctx.scope.agent_kind()?, ctx.scope.venue_id.as_deref()) {
        (AgentKind::PerformSet, Some(venue_id)) => venue_id,
        _ => return unavailable(b, "perform", NOT_PERFORM),
    };
    let read = async {
        let mut access =
            VenueAccess::<Read>::read(ctx.pool, VenueResource::Venue(venue_id)).await?;
        let set = perform_set::read(&mut access).await?;
        let groups = SpokenGroups::read(ctx.resource_root, &mut access).await?;
        Ok::<_, String>(groups.spoken(set.described()))
    };
    match read.await {
        Ok(set) => {
            inline(b, "perform.venue_id", venue_id)?;
            inline(b, "perform.set", set)
        }
        Err(error) => unavailable(
            b,
            "perform",
            format!("the Perform set is not available: {error}"),
        ),
    }
}
