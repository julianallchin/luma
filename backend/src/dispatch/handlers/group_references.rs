//! Repair selector names across a venue's scores.
use crate::database::local::{
    scores::rows,
    venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource, Write},
};
use crate::dispatch::{AppServices, CommandError};
use crate::models::groups::MissingGroup;
use crate::services::groups;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

fn selectors(
    value: &mut Value,
    visit: &mut impl FnMut(&mut String) -> Result<(), String>,
) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            // Only selection objects carry this field; labels and graph names
            // are not selectors and must never be rewritten by a rename.
            if let Some(Value::String(expression)) = object.get_mut("expression") {
                visit(expression)?;
            }
            for (key, child) in object {
                if key != "expression" {
                    selectors(child, visit)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                selectors(item, visit)?;
            }
        }
        _ => {}
    }
    Ok(())
}
/// Every score of this venue, with its owner — the uid a rewrite writes under.
async fn score_ids(access: &mut impl AuthorizedVenue) -> Result<Vec<(String, String)>, String> {
    let venue = access.venue_id().to_owned();
    sqlx::query_as("SELECT id, COALESCE(uid, '') FROM scores WHERE venue_id = ? ORDER BY id")
        .bind(&venue)
        .fetch_all(&mut *access.connection())
        .await
        .map_err(|error| error.to_string())
}
pub async fn missing_venue_groups(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<MissingGroup>, CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let known: HashSet<String> = groups::GroupSources::read(&services.fixtures_root, &mut access)
        .await?
        .tree()
        .into_iter()
        .map(|g| g.name)
        .collect();
    let mut missing = BTreeMap::<String, Vec<String>>::new();
    for (score_id, _) in score_ids(&mut access).await? {
        let score = rows::load_score(access.connection(), &score_id).await?;
        let mut value = serde_json::to_value(&score).map_err(|e| e.to_string())?;
        selectors(&mut value, &mut |expression| {
            for name in groups::selection_names(expression)? {
                if !known.contains(&name) {
                    let scores = missing.entry(name).or_default();
                    if !scores.contains(&score_id) {
                        scores.push(score_id.clone());
                    }
                }
            }
            Ok(())
        })?;
    }
    Ok(missing
        .into_iter()
        .map(|(name, scores)| MissingGroup { name, scores })
        .collect())
}

pub async fn resolve_venue_group(
    services: &AppServices,
    venue_id: String,
    missing: String,
    replacement: Option<String>,
    fixtures: Vec<String>,
) -> Result<(), CommandError> {
    crate::models::groups::validate_group_name(&missing).map_err(CommandError::Invalid)?;
    if replacement.is_none() {
        // Recreating an empty set is explicit: the user can assign fixtures next.
        return super::groups::save_venue_group(
            services,
            venue_id,
            None,
            missing,
            fixtures,
            vec![],
        )
        .await;
    }
    let replacement = replacement.unwrap();
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let known = groups::GroupSources::read(&services.fixtures_root, &mut access)
        .await?
        .tree();
    if !known.iter().any(|g| g.name == replacement) {
        return Err(CommandError::Invalid(
            "The replacement group no longer exists".into(),
        ));
    }
    let scores = score_ids(&mut access).await?;
    drop(access);
    for (score_id, uid) in scores {
        let mut access =
            VenueAccess::<Write>::write(&services.db.0, VenueResource::Score(&score_id)).await?;
        let score = rows::load_score(access.connection(), &score_id).await?;
        let mut value = serde_json::to_value(&score).map_err(|e| e.to_string())?;
        let mut changed = false;
        selectors(&mut value, &mut |expression| {
            if groups::selection_names(expression)?
                .iter()
                .any(|name| name == &missing)
            {
                *expression = groups::replace_selection_name(expression, &missing, &replacement)?;
                changed = true;
            }
            Ok(())
        })?;
        if !changed {
            continue;
        }
        let candidate: luma_patterns::Score =
            serde_json::from_value(value).map_err(|e| CommandError::Invalid(e.to_string()))?;
        rows::save_score(access.connection(), &score_id, &uid, &candidate).await?;
        access.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_selector_repair_preserves_labels_and_operator_structure() {
        let mut value = serde_json::json!({"name":"wash", "selection":{"expression":"wash & ~wash_back > ALL"}});
        selectors(&mut value, &mut |e| {
            *e = groups::replace_selection_name(e, "wash", "front")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(value["name"], "wash");
        assert_eq!(value["selection"]["expression"], "front & ~wash_back > ALL");
        assert_eq!(
            groups::selection_names("front & ~wash_back > ALL").unwrap(),
            vec!["front", "wash_back"]
        );
    }
}
