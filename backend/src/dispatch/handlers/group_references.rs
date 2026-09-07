//! Repair selector names through authored history, scoped to one venue.
use crate::database::local::{
    scores,
    venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource, Write},
};
use crate::dispatch::{AppServices, CommandError};
use crate::models::groups::MissingGroup;
use crate::services::{
    graph_scores::{self, ScoreDocument},
    groups,
    track_edits::TrackScope,
};
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
async fn scopes(access: &mut impl AuthorizedVenue) -> Result<Vec<TrackScope>, String> {
    let venue = access.venue_id().to_owned();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, track_id FROM scores WHERE venue_id = ? ORDER BY id")
            .bind(&venue)
            .fetch_all(&mut *access.connection())
            .await
            .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(score_id, track_id)| TrackScope {
            score_id,
            track_id,
            venue_id: venue.clone(),
        })
        .collect())
}
fn document_value(document: &ScoreDocument) -> Result<Value, String> {
    match document {
        ScoreDocument::Graph(doc) => serde_json::to_value(&doc.score),
        ScoreDocument::Legacy(doc) => serde_json::to_value(&doc.clips),
    }
    .map_err(|e| e.to_string())
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
    for scope in scopes(&mut access).await? {
        let doc = graph_scores::read_score_document(&mut access, &scope).await?;
        selectors(&mut document_value(&doc)?, &mut |expression| {
            for name in groups::selection_names(expression)? {
                if !known.contains(&name) {
                    let scores = missing.entry(name).or_default();
                    if !scores.contains(&scope.score_id) {
                        scores.push(scope.score_id.clone());
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
    let owner = access.principal().map(str::to_owned);
    let scopes = scopes(&mut access).await?;
    drop(access);
    for scope in scopes {
        let mut access =
            VenueAccess::<Read>::read(&services.db.0, VenueResource::Score(&scope.score_id))
                .await?;
        let document = graph_scores::read_score_document(&mut access, &scope).await?;
        let mut value = document_value(&document)?;
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
        let operation = uuid::Uuid::new_v4().to_string();
        let subject = format!("Replace group {missing} with {replacement}");
        match document {
            ScoreDocument::Graph(doc) => {
                drop(access);
                let source = serde_json::to_string(&value)
                    .map_err(|e| CommandError::Invalid(e.to_string()))?;
                services
                    .authored
                    .apply_score_source_for_scope(
                        &services.db.0,
                        owner.as_deref(),
                        scope,
                        &operation,
                        &source,
                        &doc.revision,
                        &subject,
                    )
                    .await?;
            }
            ScoreDocument::Legacy(_) => {
                let base = scores::get_clips_of_score(&mut access, &scope.score_id).await?;
                let mut candidate = base.clone();
                for clip in &mut candidate {
                    selectors(&mut clip.args, &mut |expression| {
                        *expression =
                            groups::replace_selection_name(expression, &missing, &replacement)?;
                        Ok(())
                    })?;
                }
                drop(access);
                services
                    .authored
                    .replace_track_scores_for_scope(
                        &services.db.0,
                        owner.as_deref(),
                        scope,
                        &base,
                        &candidate,
                        &operation,
                        &subject,
                    )
                    .await?;
            }
        }
    }
    services.sync.push_notify.notify_one();
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
