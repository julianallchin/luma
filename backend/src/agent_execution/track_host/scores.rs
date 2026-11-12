//! `luma.scores`: the user's other scores, read-only.
//!
//! A track thread edits one score. To adapt a show to another rig, or to
//! learn from one, it also needs to read the others: this finds them and
//! hands one over with its groups named and its venue's rig summarized.
//! Nothing here writes, and a score that belongs to someone else is refused
//! even in a venue the user may open.
use super::*;
use crate::agent_execution::spoken_groups::SpokenGroups;
use crate::database::local::scores::rows;
use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};

/// A search that matches everything still answers in one screen.
const FIND_LIMIT: i64 = 50;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FindRequest {
    /// Words to find in the score, track, artist or venue name.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    track_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    id: String,
}

type FoundRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    i64,
    String,
);

fn forbidden(error: String) -> HostCallError {
    HostCallError::new("forbidden", error)
}

fn internal(error: sqlx::Error) -> HostCallError {
    HostCallError::new("internal", error.to_string())
}

impl TrackHost {
    pub(super) async fn scores_call(
        &self,
        method: &str,
        payload: Value,
    ) -> Result<Value, HostCallError> {
        match method {
            "track.scores_find" => self.scores_find(decode(payload)?).await,
            _ => self.scores_open(decode(payload)?).await,
        }
    }

    /// The user's own scores in venues they can open, this track's first and
    /// then the newest. The score this thread edits is left out.
    async fn scores_find(&self, request: FindRequest) -> Result<Value, HostCallError> {
        let principal = {
            let access =
                VenueAccess::<Read>::read(&self.pool, VenueResource::Score(&self.scope.score_id))
                    .await
                    .map_err(forbidden)?;
            access.principal().to_owned()
        };
        let pattern = request
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| format!("%{}%", text.to_lowercase()));
        let rows: Vec<FoundRow> = sqlx::query_as(
            "SELECT score.id, score.name, score.track_id, track.title, track.artist,
                    score.venue_id, venue.name,
                    (SELECT COUNT(*) FROM clips WHERE clips.score_id = score.id),
                    score.updated_at
             FROM scores score
             JOIN tracks track ON track.id = score.track_id
             JOIN venues venue ON venue.id = score.venue_id
             JOIN auth_venue_access access ON access.venue_id = score.venue_id
             WHERE score.uid = ?1 AND score.id != ?2
               AND (?3 IS NULL OR score.track_id = ?3)
               AND (?4 IS NULL OR lower(coalesce(score.name, '') || ' ' || coalesce(track.title, '')
                    || ' ' || coalesce(track.artist, '') || ' ' || coalesce(venue.name, '')) LIKE ?4)
             ORDER BY (score.track_id = ?5) DESC, score.updated_at DESC
             LIMIT ?6",
        )
        .bind(&principal)
        .bind(&self.scope.score_id)
        .bind(&request.track_id)
        .bind(&pattern)
        .bind(&self.scope.track_id)
        .bind(FIND_LIMIT)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        let found: Vec<Value> = rows
            .into_iter()
            .map(
                |(id, name, track_id, title, artist, venue_id, venue, clips, updated_at)| {
                    json!({
                        "id": id, "name": name,
                        "track_id": track_id, "track_title": title, "track_artist": artist,
                        "venue_id": venue_id, "venue_name": venue,
                        "clips": clips, "updated_at": updated_at,
                    })
                },
            )
            .collect();
        Ok(json!({ "scores": found }))
    }

    /// One of the user's scores: its document with groups named, and what
    /// each group of its venue is made of.
    async fn scores_open(&self, request: OpenRequest) -> Result<Value, HostCallError> {
        let mut access = VenueAccess::<Read>::read(&self.pool, VenueResource::Score(&request.id))
            .await
            .map_err(forbidden)?;
        let principal = access.principal().to_owned();
        let venue_id = access.venue_id().to_owned();
        let row: Option<(
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT score.uid, score.track_id, score.name, track.title, venue.name
                 FROM scores score
                 JOIN tracks track ON track.id = score.track_id
                 LEFT JOIN venues venue ON venue.id = score.venue_id
                 WHERE score.id = ?",
        )
        .bind(&request.id)
        .fetch_optional(&mut *access.connection())
        .await
        .map_err(internal)?;
        let Some((uid, track_id, name, track_title, venue_name)) = row else {
            return Err(HostCallError::new("not_found", "no such score"));
        };
        if uid.as_deref() != Some(principal.as_str()) {
            return Err(HostCallError::new(
                "forbidden",
                "luma.scores reads only your own scores",
            ));
        }
        let score = rows::load_score(access.connection(), &request.id)
            .await
            .map_err(|error| HostCallError::new("invalid_score", error))?;
        let spoken = SpokenGroups::read(&self.resource_root, &mut access)
            .await
            .map_err(|error| HostCallError::new("internal", error))?;
        let groups: Vec<(Option<String>, i64, i64, Option<String>)> = sqlx::query_as(
            "SELECT grp.name, COUNT(DISTINCT member.fixture_id), COUNT(member.head_index),
                    GROUP_CONCAT(DISTINCT fixture.model)
             FROM fixture_groups grp
             LEFT JOIN fixture_group_members member ON member.group_id = grp.id
             LEFT JOIN fixtures fixture ON fixture.id = member.fixture_id
             WHERE grp.venue_id = ?
             GROUP BY grp.id
             ORDER BY grp.display_order, grp.name",
        )
        .bind(&venue_id)
        .fetch_all(&mut *access.connection())
        .await
        .map_err(internal)?;
        let groups: Vec<Value> = groups
            .into_iter()
            .map(|(name, fixtures, cells, models)| {
                let models: Vec<&str> = models
                    .as_deref()
                    .map(|models| models.split(',').collect())
                    .unwrap_or_default();
                json!({ "name": name, "fixtures": fixtures, "cells": cells, "models": models })
            })
            .collect();
        Ok(json!({
            "id": request.id,
            "name": name,
            "track_id": track_id,
            "track_title": track_title,
            "same_track": track_id == self.scope.track_id,
            "venue": { "id": venue_id, "name": venue_name, "groups": groups },
            "document": spoken.spoken(json!(score)),
        }))
    }
}
