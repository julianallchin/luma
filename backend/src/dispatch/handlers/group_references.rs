//! Where a venue's groups are used: its scores' clips and its Perform set.
use crate::database::local::{
    perform_set,
    venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource},
};
use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::groups::GroupUsage;
use crate::models::perform::CellContent;
use crate::models::selection::Selection;
use std::collections::BTreeMap;

/// Every group of the venue that something selects, with what selects it.
///
/// A selection names groups by id, so this is a count of ids and reads no
/// graph. A group nothing uses is not listed.
pub async fn venue_group_usage(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<GroupUsage>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(usage(&mut access).await?)
}

pub(crate) async fn usage(access: &mut impl AuthorizedVenue) -> Result<Vec<GroupUsage>, String> {
    let mut used = BTreeMap::<String, GroupUsage>::new();

    let venue = access.venue_id().to_owned();
    let clips: Vec<(String, String)> = sqlx::query_as(
        "SELECT clips.score_id, clips.selection_json FROM clips
         JOIN scores ON scores.id = clips.score_id
         WHERE scores.venue_id = ? ORDER BY clips.score_id",
    )
    .bind(&venue)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|error| format!("failed to read the venue's clips: {error}"))?;
    for (score, selection) in clips {
        let selection: Selection = serde_json::from_str(&selection)
            .map_err(|error| format!("a clip of score {score} has no selection: {error}"))?;
        for group in selection.group_ids() {
            let usage = entry(&mut used, group);
            usage.clips += 1;
            if !usage.scores.contains(&score) {
                usage.scores.push(score.clone());
            }
        }
    }

    for pattern in perform_set::list(access).await? {
        for group in pattern.selection.group_ids() {
            entry(&mut used, group).perform_patterns.push(pattern.id.clone());
        }
    }
    if let Some(layout) = perform_set::layout(access).await? {
        for cell in layout.pages.iter().flat_map(|page| &page.cells) {
            if let CellContent::Group { group, .. } = &cell.content {
                if group != crate::controls::ALL {
                    entry(&mut used, group).perform_cells += 1;
                }
            }
        }
    }
    Ok(used.into_values().collect())
}

fn entry<'a>(used: &'a mut BTreeMap<String, GroupUsage>, group: &str) -> &'a mut GroupUsage {
    used.entry(group.to_owned()).or_insert_with(|| GroupUsage {
        group_id: group.to_owned(),
        clips: 0,
        scores: Vec::new(),
        perform_patterns: Vec::new(),
        perform_cells: 0,
    })
}
