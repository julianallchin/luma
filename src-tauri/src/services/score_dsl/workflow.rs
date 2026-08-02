use std::collections::HashMap;

use sqlx::{SqliteConnection, SqlitePool};

use crate::database::local::patterns as pattern_db;
use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::node_graph::{BeatGrid, PatternArgDef};
use crate::services::graph_documents::{
    load_pattern_interface_for_connection, resolve_graph_implementation_for_connection,
};
use crate::services::track_edits::{load_track_document_for_connection, TrackDocument, TrackScope};
use crate::services::tracks;

use super::{
    convert::build_registry_with_unavailable, track_document_to_canonical_dsl,
    track_document_to_exemplar_dsl, PatternNames, PatternRegistry,
};

/// All host-owned inputs required to parse or serialize one score. Callers do
/// not pass pattern interfaces or beat data over Tauri, so validation and
/// import cannot disagree with the installed backend state.
#[derive(Clone, Debug)]
pub struct ScoreDslContext {
    pub beat_grid: BeatGrid,
    pub registry: PatternRegistry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreDslExportKind {
    Canonical,
    Exemplar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScoreDslExport {
    pub source: String,
    pub revision: String,
    pub clip_count: usize,
}

/// Load timing and catalog every installed pattern interface. A legacy graph's
/// stale internal nodes or edges do not make its public score interface
/// unavailable. Ambiguous implementations, malformed JSON, and invalid
/// argument definitions remain explicit unavailable entries: unrelated damage
/// does not block a score, while a referenced corrupt interface fails at the
/// exact reference instead of being treated as an arg-less pattern.
pub async fn load_score_dsl_context(
    pool: &SqlitePool,
    scope: &TrackScope,
) -> Result<ScoreDslContext, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open score DSL context: {error}"))?;
    load_score_dsl_context_for_connection(&mut connection, scope).await
}

pub(crate) async fn load_score_dsl_context_for_connection(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
) -> Result<ScoreDslContext, String> {
    let beat_grid = tracks::get_track_beats_for_connection(connection, &scope.track_id)
        .await?
        .unwrap_or_else(empty_beat_grid);
    let patterns = pattern_db::list_patterns_for_connection(connection).await?;

    let mut args_by_pattern: HashMap<String, Vec<PatternArgDef>> = HashMap::new();
    let mut unavailable_by_pattern = HashMap::new();
    for pattern in &patterns {
        let implementation_id = match resolve_graph_implementation_for_connection(
            connection,
            &pattern.id,
            Some(&scope.venue_id),
            None,
        )
        .await
        {
            Ok(implementation_id) => implementation_id,
            Err(error) => {
                unavailable_by_pattern.insert(pattern.id.clone(), error.to_string());
                continue;
            }
        };
        match load_pattern_interface_for_connection(connection, &pattern.id, &implementation_id)
            .await
        {
            Ok(args) => {
                args_by_pattern.insert(pattern.id.clone(), args);
            }
            Err(error) => {
                unavailable_by_pattern.insert(
                    pattern.id.clone(),
                    format!("implementation {implementation_id}: {error}"),
                );
            }
        }
    }

    Ok(ScoreDslContext {
        beat_grid,
        registry: build_registry_with_unavailable(
            &patterns,
            &args_by_pattern,
            unavailable_by_pattern,
        ),
    })
}

/// The canonical Git codec needs only human-readable labels for stable pattern
/// IDs. Loading them separately keeps commit serialization independent from
/// mutable beat analysis and pattern implementation interfaces.
pub async fn load_score_pattern_names(pool: &SqlitePool) -> Result<PatternNames, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open score pattern labels: {error}"))?;
    load_score_pattern_names_for_connection(&mut connection).await
}

async fn load_score_pattern_names_for_connection(
    connection: &mut SqliteConnection,
) -> Result<PatternNames, String> {
    Ok(pattern_db::list_patterns_for_connection(connection)
        .await?
        .into_iter()
        .map(|pattern| (pattern.id, pattern.name))
        .collect())
}

pub async fn load_score_dsl_document_with_access(
    access: &mut impl AuthorizedVenue,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
) -> Result<(TrackDocument, ScoreDslContext), String> {
    if access.venue_id() != scope.venue_id {
        return Err("Venue resource not found".into());
    }
    let document = load_track_document_for_connection(access.connection(), scope, owner_user_id)
        .await
        .map_err(|error| error.to_string())?;
    let context = load_score_dsl_context_for_connection(access.connection(), scope).await?;
    Ok((document, context))
}

pub async fn export_score_source_with_access(
    access: &mut impl AuthorizedVenue,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
    kind: ScoreDslExportKind,
) -> Result<ScoreDslExport, String> {
    if access.venue_id() != scope.venue_id {
        return Err("Venue resource not found".into());
    }
    let document = load_track_document_for_connection(access.connection(), scope, owner_user_id)
        .await
        .map_err(|error| error.to_string())?;
    let source = match kind {
        ScoreDslExportKind::Canonical => track_document_to_canonical_dsl(
            &document,
            &load_score_pattern_names_for_connection(access.connection()).await?,
        ),
        ScoreDslExportKind::Exemplar => {
            let context = load_score_dsl_context_for_connection(access.connection(), scope).await?;
            track_document_to_exemplar_dsl(&document, &context.beat_grid, &context.registry)
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(ScoreDslExport {
        source,
        revision: document.revision,
        clip_count: document.clips.len(),
    })
}

fn empty_beat_grid() -> BeatGrid {
    BeatGrid {
        beats: Vec::new(),
        downbeats: Vec::new(),
        bpm: 120.0,
        downbeat_offset: 0.0,
        beats_per_bar: 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::node_graph::BlendMode;
    use crate::services::graph_documents::load_graph_document_unscoped;
    use crate::services::score_dsl::{
        clips_to_document, compile_import_track_document, document_to_clips,
    };
    use crate::services::track_edits::TrackClip;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    #[tokio::test]
    async fn score_context_accepts_valid_interfaces_and_defers_corrupt_ones() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("luma-test.db");
        let migrate_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .unwrap();
        migrate_pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO tracks
             (id, track_hash, title, duration_seconds, file_path)
             VALUES ('track', 'hash', 'track', 60.0, '/tmp/track.wav');
             INSERT INTO venues (id, name) VALUES ('venue', 'venue');
             INSERT INTO patterns (id, name) VALUES
                ('good-pattern', 'good'),
                ('legacy-pattern', 'legacy'),
                ('invalid-interface-pattern', 'invalid_interface'),
                ('broken-pattern', 'broken');
             INSERT INTO implementations (id, pattern_id, graph_json) VALUES
                ('good-implementation', 'good-pattern',
                 '{\"nodes\":[],\"edges\":[],\"args\":[]}'),
                ('invalid-interface-implementation', 'invalid-interface-pattern',
                 '{\"args\":[{\"id\":\"gain\",\"name\":\"gain\",\"argType\":\"Scalar\",\"defaultValue\":\"loud\"}]}'),
                ('broken-implementation', 'broken-pattern', 'not-json');",
        )
        .execute(&pool)
        .await
        .unwrap();
        let legacy_graph = json!({
            "nodes": [
                {
                    "id": "pattern_args",
                    "typeId": "pattern_args",
                    "params": {},
                    "positionX": 0.0,
                    "positionY": 0.0
                },
                {
                    "id": "view",
                    "typeId": "view_signal",
                    "params": {},
                    "positionX": 1.0,
                    "positionY": 0.0
                }
            ],
            "edges": [{
                "id": "stale",
                "fromNode": "pattern_args",
                "fromPort": "removed_arg",
                "toNode": "view",
                "toPort": "in"
            }],
            "args": [
                {
                    "id": "selection",
                    "name": "selection",
                    "argType": "Selection",
                    "defaultValue": {
                        "expression": "all",
                        "spatialReference": "global"
                    }
                },
                {
                    "id": "gain",
                    "name": "gain",
                    "argType": "Scalar",
                    "defaultValue": 1.0
                }
            ]
        });
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json)
             VALUES ('legacy-implementation', 'legacy-pattern', ?)",
        )
        .bind(legacy_graph.to_string())
        .execute(&pool)
        .await
        .unwrap();
        let scope = TrackScope {
            score_id: "score".into(),
            track_id: "track".into(),
            venue_id: "venue".into(),
        };

        let context = load_score_dsl_context(&pool, &scope).await.unwrap();
        assert!(context.registry.by_id("good-pattern").is_some());
        assert!(context.registry.by_id("legacy-pattern").is_some());
        assert!(context
            .registry
            .by_id("invalid-interface-pattern")
            .is_none());
        assert!(context.registry.by_id("broken-pattern").is_none());

        let graph_error =
            load_graph_document_unscoped(&pool, "legacy-pattern", "legacy-implementation")
                .await
                .unwrap_err();
        assert!(graph_error.to_string().contains("removed_arg"));

        let clip = TrackClip {
            id: "legacy-clip".into(),
            pattern_id: "legacy-pattern".into(),
            start_time: 0.25,
            end_time: 1.75,
            z_index: -2,
            blend_mode: BlendMode::Add,
            args: json!({
                "selection": {
                    "expression": "front & left",
                    "spatialReference": "group_local"
                },
                "gain": 0.625,
                "orphaned_arg": {"nested": [true, null, "preserved"]}
            }),
        };
        let document = clips_to_document(
            std::slice::from_ref(&clip),
            &context.beat_grid,
            &context.registry,
        )
        .expect("stale graph internals must not block score export");
        assert_eq!(
            document_to_clips(&document, &context.beat_grid, &context.registry).unwrap(),
            vec![clip]
        );

        compile_import_track_document("good[\"good-pattern\"](all) @0s-1s", &context, true)
            .expect("unreferenced corrupt pattern must not block a valid score");
        let error =
            compile_import_track_document("broken[\"broken-pattern\"](all) @0s-1s", &context, true)
                .unwrap_err();
        assert!(error.to_string().contains("graph is unavailable"));
        assert!(error.to_string().contains("broken-implementation"));

        let error = compile_import_track_document(
            "invalid_interface[\"invalid-interface-pattern\"](all) @0s-1s",
            &context,
            true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("graph is unavailable"));
        assert!(error.to_string().contains("invalid Scalar default"));
    }
}
