//! Convert a complete row-based score from one authorized database snapshot.
//! The caller publishes the candidate through authored history with the old CAS;
//! this layer never changes the rows or the library patterns it copies.
use super::GraphScoreDocument;
use crate::{
    database::local::venue_access::AuthorizedVenue,
    models::node_graph::{Graph, PatternArgType},
    node_graph::migration::{argument_value, pattern, ROOT},
    services::{
        graph_documents::load_visible_graph_document_for_connection,
        track_edits::{TrackDocument, TrackScope},
    },
};
use luma_patterns::{self as p, Body};
use std::collections::{BTreeMap, HashMap};

/// `None` keeps an older, untyped score explicit until its node vocabulary has a
/// converter. Never publish a partially migrated score or discard an old clip.
pub(crate) async fn upgrade_rows(
    access: &mut impl AuthorizedVenue,
    scope: &TrackScope,
    document: &TrackDocument,
) -> Result<Option<GraphScoreDocument>, String> {
    let mut score = p::Score::default();
    let mut patterns = BTreeMap::<String, (Graph, String)>::new();
    for clip in &document.clips {
        if patterns.contains_key(&clip.pattern_id) {
            continue;
        }
        let implemented: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM implementations WHERE pattern_id = ?)")
                .bind(&clip.pattern_id)
                .fetch_one(access.connection())
                .await
                .map_err(|e| e.to_string())?;
        if !implemented {
            // A library placeholder is valid legacy authored state. Keep the
            // whole score in that format until its graph is available.
            return Ok(None);
        }
        let stored = load_visible_graph_document_for_connection(
            access.connection(),
            &clip.pattern_id,
            Some(&scope.venue_id),
            None,
        )
        .await
        .map_err(|e| format!("Pattern {}: {e}", clip.pattern_id))?;
        let name: String = sqlx::query_scalar("SELECT name FROM patterns WHERE id = ?")
            .bind(&clip.pattern_id)
            .fetch_one(access.connection())
            .await
            .map_err(|e| e.to_string())?;
        let Some(mut converted) =
            pattern(&stored.graph, &name).map_err(|e| format!("Pattern {name}: {e}"))?
        else {
            return Ok(None);
        };
        // Copies share one score-owned definition per selected implementation.
        // Namespace every converted helper so two source patterns cannot collide.
        let prefix = format!("pattern/{}:{}/", clip.pattern_id.len(), clip.pattern_id);
        let remap: BTreeMap<_, _> = converted
            .definitions
            .keys()
            .map(|id| (id.clone(), format!("{prefix}{id}")))
            .collect();
        let root = remap
            .get(ROOT)
            .ok_or("converted pattern lost its root graph")?
            .clone();
        converted.definitions = converted
            .definitions
            .into_iter()
            .map(|(id, mut definition)| {
                if let Body::Graph(graph) = &mut definition.body {
                    for node in graph.nodes.values_mut() {
                        if let Some(id) = remap.get(&node.definition) {
                            node.definition = id.clone();
                        }
                    }
                }
                (remap[&id].clone(), definition)
            })
            .collect();
        score.definitions.extend(converted.definitions);
        patterns.insert(clip.pattern_id.clone(), (stored.graph, root));
    }
    if document.clips.is_empty() {
        return GraphScoreDocument::new(score).map(Some);
    }
    let grid = crate::services::tracks::get_track_beats_for_connection(
        access.connection(),
        &scope.track_id,
    )
    .await?
    .ok_or("Analyze the track before converting its clips to musical time")?;
    let clock = grid.timeline().map_err(|e| e.to_string())?;
    for clip in &document.clips {
        let (graph, root) = &patterns[&clip.pattern_id];
        let provided = clip
            .args
            .as_object()
            .ok_or_else(|| format!("Clip {} arguments must be an object", clip.id))?;
        let provided: HashMap<_, _> = provided
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let args = crate::eval::graph_run::merge_arg_values(graph, Some(&provided), false);
        let mut inputs = BTreeMap::new();
        for (id, value) in &provided {
            let arg = graph
                .args
                .iter()
                .find(|a| a.id == *id)
                .ok_or_else(|| format!("Clip {} has an unknown argument {id}", clip.id))?;
            if arg.arg_type == PatternArgType::Selection {
                continue;
            }
            let input = score.definitions[root]
                .inputs
                .get(id)
                .ok_or_else(|| format!("Conversion lost input {id}"))?;
            inputs.insert(
                id.clone(),
                argument_value(&arg.arg_type, input.value_type, value)?,
            );
        }
        let (selection, selection_seed) = crate::eval::context::graph_selection(
            &graph.nodes,
            &graph.edges,
            &args,
            Some(&clip.id),
        )
        .unwrap_or_else(|| (p::Selection::all(), 0));
        let start = clock.beat_at(clip.start_time).map_err(|e| e.to_string())?;
        let duration = clock.beat_at(clip.end_time).map_err(|e| e.to_string())? - start;
        score.clips.insert(
            clip.id.clone(),
            p::Clip {
                graph: root.clone(),
                start,
                duration,
                seed: crate::eval::context::seed_for(Some(&clip.id), "lighting"),
                selection_seed: Some(selection_seed),
                selection,
                z_index: clip.z_index,
                blend_mode: clip.blend_mode,
                inputs,
            },
        );
    }
    GraphScoreDocument::new(score).map(Some)
}
