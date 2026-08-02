//! One-time, idempotent upgrade of relational graphs authored before Git became
//! the source of truth.
//!
//! This runs before SQL migrations and before authored-state reconciliation.
//! Clean current graphs remain byte-for-byte untouched. Rows carrying an
//! explicit legacy marker are upgraded transactionally and then validated
//! against the current executor catalog before any write is allowed.

use std::collections::HashSet;

use serde_json::{json, Value};
use sqlx::{SqliteConnection, SqlitePool};

use crate::models::node_graph::{Graph, NodeInstance, PatternArgType};
use crate::services::graph_documents::{canonicalize_graph, exact_graph_json};

const PATTERN_ARGS_NODE_ID: &str = "pattern_args";
const LEGACY_NODE_TYPES: [&str; 4] = ["audio_source", "pattern_entry", "beat_crop", "beat_clock"];
const LEGACY_LIGHT_POINTS_COLORS: &str =
    r##"["#ff2244","#cc44ff","#ff7722","#ffeecc","#8833ff","#55ccaa"]"##;

#[derive(Debug)]
struct PendingUpdate {
    id: String,
    previous_json: String,
    upgraded_json: String,
}

#[derive(Debug)]
struct AdmissionSnapshot {
    armed: i64,
    accepting: i64,
    maintenance: i64,
    remote_writes: i64,
    active_uid: Option<String>,
    generation: i64,
}

/// Upgrade legacy relational graph blobs before migrations install or enforce
/// the authored-state projection machinery. The transform is its own durable
/// marker: after a successful run no legacy marker remains, so later launches
/// scan and perform no writes.
pub(super) async fn upgrade_legacy_graph_json(pool: &SqlitePool) -> Result<(), String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open legacy graph upgrade transaction: {error}"))?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("Failed to begin legacy graph upgrade transaction: {error}"))?;

    match upgrade_in_transaction(&mut connection).await {
        Ok(()) => {
            if let Err(error) = sqlx::query("COMMIT").execute(&mut *connection).await {
                let rollback = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                return Err(match rollback {
                    Ok(_) => format!("Failed to commit legacy graph upgrade: {error}"),
                    Err(rollback_error) => format!(
                        "Failed to commit legacy graph upgrade ({error}); rollback also failed: {rollback_error}"
                    ),
                });
            }
            Ok(())
        }
        Err(error) => match sqlx::query("ROLLBACK").execute(&mut *connection).await {
            Ok(_) => Err(error),
            Err(rollback_error) => Err(format!(
                "{error}; failed to roll back legacy graph upgrade: {rollback_error}"
            )),
        },
    }
}

async fn upgrade_in_transaction(connection: &mut SqliteConnection) -> Result<(), String> {
    if !table_exists(connection, "implementations").await? {
        return Ok(());
    }

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT CAST(id AS TEXT), graph_json FROM implementations ORDER BY CAST(id AS TEXT)",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| format!("Failed to load relational graphs for legacy upgrade: {error}"))?;

    // Transform and validate every candidate before taking admission or writing
    // any row. One ambiguous graph aborts the whole upgrade.
    let mut updates = Vec::new();
    for (id, graph_json) in rows {
        if let Some(upgraded_json) = upgrade_graph_json(&id, &graph_json)? {
            updates.push(PendingUpdate {
                id,
                previous_json: graph_json,
                upgraded_json,
            });
        }
    }
    if updates.is_empty() {
        return Ok(());
    }

    let admission = enter_upgrade_admission(connection).await?;
    for update in updates {
        let changed = sqlx::query(
            "UPDATE implementations SET graph_json = ?
             WHERE CAST(id AS TEXT) = ? AND graph_json = ?",
        )
        .bind(&update.upgraded_json)
        .bind(&update.id)
        .bind(&update.previous_json)
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            format!(
                "Failed to store upgraded graph for implementation {}: {error}",
                update.id
            )
        })?
        .rows_affected();
        if changed != 1 {
            return Err(format!(
                "Implementation {} changed during the legacy graph upgrade",
                update.id
            ));
        }
    }
    if let Some(snapshot) = admission {
        restore_upgrade_admission(connection, &snapshot).await?;
    }
    Ok(())
}

async fn table_exists(connection: &mut SqliteConnection, table: &str) -> Result<bool, String> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?
         )",
    )
    .bind(table)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| format!("Failed to inspect app database schema: {error}"))?;
    Ok(exists == 1)
}

async fn enter_upgrade_admission(
    connection: &mut SqliteConnection,
) -> Result<Option<AdmissionSnapshot>, String> {
    if !table_exists(connection, "auth_write_admission").await? {
        return Ok(None);
    }
    let row: Option<(i64, i64, i64, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT armed, accepting, maintenance, remote_writes, active_uid, generation
         FROM auth_write_admission WHERE singleton = 1",
    )
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| format!("Failed to inspect write admission for graph upgrade: {error}"))?;
    let Some((armed, accepting, maintenance, remote_writes, active_uid, generation)) = row else {
        return Err("Write admission singleton is missing during legacy graph upgrade".into());
    };
    for (name, value) in [
        ("armed", armed),
        ("accepting", accepting),
        ("maintenance", maintenance),
        ("remote_writes", remote_writes),
    ] {
        if !matches!(value, 0 | 1) {
            return Err(format!(
                "Write admission has invalid {name}={value} during legacy graph upgrade"
            ));
        }
    }
    if armed == 0 {
        return Ok(None);
    }
    if maintenance != 0 || remote_writes != 0 {
        return Err("Write admission is not quiescent during the legacy graph upgrade".into());
    }

    let snapshot = AdmissionSnapshot {
        armed,
        accepting,
        maintenance,
        remote_writes,
        active_uid,
        generation,
    };
    let changed = sqlx::query(
        "UPDATE auth_write_admission SET accepting = 0, maintenance = 1
         WHERE singleton = 1 AND armed = ? AND accepting = ?
           AND maintenance = ? AND remote_writes = ? AND active_uid IS ?
           AND generation = ?",
    )
    .bind(snapshot.armed)
    .bind(snapshot.accepting)
    .bind(snapshot.maintenance)
    .bind(snapshot.remote_writes)
    .bind(snapshot.active_uid.as_deref())
    .bind(snapshot.generation)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to enter write admission for graph upgrade: {error}"))?
    .rows_affected();
    if changed != 1 {
        return Err("Write admission changed before the legacy graph upgrade".into());
    }
    Ok(Some(snapshot))
}

async fn restore_upgrade_admission(
    connection: &mut SqliteConnection,
    snapshot: &AdmissionSnapshot,
) -> Result<(), String> {
    let changed = sqlx::query(
        "UPDATE auth_write_admission
         SET armed = ?, accepting = ?, maintenance = ?, remote_writes = ?,
             active_uid = ?, generation = ?
         WHERE singleton = 1 AND armed = ? AND accepting = 0
           AND maintenance = 1 AND remote_writes = ? AND active_uid IS ?
           AND generation = ?",
    )
    .bind(snapshot.armed)
    .bind(snapshot.accepting)
    .bind(snapshot.maintenance)
    .bind(snapshot.remote_writes)
    .bind(snapshot.active_uid.as_deref())
    .bind(snapshot.generation)
    .bind(snapshot.armed)
    .bind(snapshot.remote_writes)
    .bind(snapshot.active_uid.as_deref())
    .bind(snapshot.generation)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to restore write admission after graph upgrade: {error}"))?
    .rows_affected();
    if changed != 1 {
        return Err("Write admission changed during the legacy graph upgrade".into());
    }
    Ok(())
}

fn upgrade_graph_json(implementation_id: &str, source: &str) -> Result<Option<String>, String> {
    let mut graph: Graph = serde_json::from_str(source).map_err(|error| {
        format!("Implementation {implementation_id} has invalid graph JSON: {error}")
    })?;
    if !upgrade_graph(implementation_id, &mut graph)? {
        return Ok(None);
    }
    let graph = canonicalize_graph(&graph).map_err(|error| {
        format!(
            "Implementation {implementation_id} is invalid after its legacy graph upgrade: {error}"
        )
    })?;
    exact_graph_json(&graph)
        .map(Some)
        .map_err(|error| format!("Failed to serialize upgraded graph {implementation_id}: {error}"))
}

fn upgrade_graph(implementation_id: &str, graph: &mut Graph) -> Result<bool, String> {
    ensure_unambiguous_node_ids(implementation_id, graph)?;
    let mut changed = upgrade_argument_values(implementation_id, graph)?;
    changed |= upgrade_legacy_node_params(implementation_id, graph)?;
    changed |= migrate_select_nodes(implementation_id, graph)?;
    changed |= remove_retired_nodes(implementation_id, graph)?;
    changed |= canonicalize_pattern_args_node(implementation_id, graph)?;
    changed |= remove_stale_pattern_arg_edges(graph);
    Ok(changed)
}

fn ensure_unambiguous_node_ids(implementation_id: &str, graph: &Graph) -> Result<(), String> {
    let mut ids = HashSet::new();
    for node in &graph.nodes {
        if !ids.insert(node.id.as_str()) {
            return Err(format!(
                "Implementation {implementation_id} has duplicate node id {}",
                node.id
            ));
        }
    }
    Ok(())
}

fn upgrade_argument_values(implementation_id: &str, graph: &mut Graph) -> Result<bool, String> {
    let mut changed = false;
    for arg in &mut graph.args {
        if arg.id == "fixtures" && arg.name == "Fixtures" {
            arg.name = "fixtures".into();
            changed = true;
        }
        match arg.arg_type {
            PatternArgType::Color => {
                if let Some(hex) = arg.default_value.as_str() {
                    arg.default_value = parse_legacy_hex_color(hex).map_err(|error| {
                        format!(
                            "Implementation {implementation_id} argument {} has an invalid legacy default: {error}",
                            arg.id
                        )
                    })?;
                    changed = true;
                }
            }
            PatternArgType::Selection => {
                let Some(default) = arg.default_value.as_object() else {
                    continue;
                };
                let (Some(expression), Some(spatial_reference)) = (
                    default.get("expression").and_then(Value::as_str),
                    default.get("spatialReference").and_then(Value::as_str),
                ) else {
                    continue;
                };
                if is_legacy_string_spread(default, expression) {
                    arg.default_value = json!({
                        "expression": expression,
                        "spatialReference": spatial_reference,
                    });
                    changed = true;
                }
            }
            PatternArgType::Scalar | PatternArgType::Palette | PatternArgType::Gradient => {}
        }
    }
    Ok(changed)
}

fn is_legacy_string_spread(default: &serde_json::Map<String, Value>, expression: &str) -> bool {
    let extras: Vec<(&str, &Value)> = default
        .iter()
        .filter_map(|(key, value)| {
            (!matches!(key.as_str(), "expression" | "spatialReference"))
                .then_some((key.as_str(), value))
        })
        .collect();
    let characters: Vec<char> = expression.chars().collect();
    if extras.len() != characters.len() || extras.is_empty() {
        return false;
    }
    characters.iter().enumerate().all(|(index, character)| {
        default
            .get(&index.to_string())
            .and_then(Value::as_str)
            .is_some_and(|value| value == character.to_string())
    })
}

fn parse_legacy_hex_color(hex: &str) -> Result<Value, String> {
    let Some(digits) = hex.strip_prefix('#') else {
        return Err(format!("Legacy Color default {hex:?} must start with '#'"));
    };
    if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "Legacy Color default {hex:?} must be #rrggbb or #rrggbbaa"
        ));
    }
    let channel = |start| {
        u8::from_str_radix(&digits[start..start + 2], 16)
            .map_err(|error| format!("Failed to decode legacy Color default {hex:?}: {error}"))
    };
    let alpha = if digits.len() == 8 {
        f64::from(channel(6)?) / 255.0
    } else {
        1.0
    };
    Ok(json!({
        "r": channel(0)?,
        "g": channel(2)?,
        "b": channel(4)?,
        "a": alpha,
    }))
}

fn upgrade_legacy_node_params(implementation_id: &str, graph: &mut Graph) -> Result<bool, String> {
    let incoming: HashSet<(String, String)> = graph
        .edges
        .iter()
        .map(|edge| (edge.to_node.clone(), edge.to_port.clone()))
        .collect();
    let mut changed = false;
    for node in &mut graph.nodes {
        match node.type_id.as_str() {
            "chroma_palette" if node.params.contains_key("palette") => {
                if node.params.get("palette") != Some(&Value::String("Rainbow".into())) {
                    return Err(format!(
                        "Implementation {implementation_id} node {} has an unknown legacy chroma palette",
                        node.id
                    ));
                }
                node.params.remove("palette");
                changed = true;
            }
            "random_select_mask" if node.params.contains_key("count") => {
                if !incoming.contains(&(node.id.clone(), "count".into())) {
                    return Err(format!(
                        "Implementation {implementation_id} node {} has legacy count without a count input",
                        node.id
                    ));
                }
                node.params.remove("count");
                changed = true;
            }
            "noise" if node.params.contains_key("speed") => {
                let speed_is_one = node
                    .params
                    .get("speed")
                    .and_then(Value::as_f64)
                    .is_some_and(|speed| speed == 1.0);
                if !speed_is_one || !incoming.contains(&(node.id.clone(), "time".into())) {
                    return Err(format!(
                        "Implementation {implementation_id} node {} has an ambiguous legacy noise speed",
                        node.id
                    ));
                }
                node.params.remove("speed");
                changed = true;
            }
            "sine_wave" if node.params.contains_key("frequency_hz") => {
                if !node.params.contains_key("subdivision") {
                    return Err(format!(
                        "Implementation {implementation_id} node {} has frequency_hz without subdivision",
                        node.id
                    ));
                }
                node.params.remove("frequency_hz");
                changed = true;
            }
            _ => {}
        }
    }
    Ok(changed)
}

fn migrate_select_nodes(implementation_id: &str, graph: &mut Graph) -> Result<bool, String> {
    let select_nodes: Vec<NodeInstance> = graph
        .nodes
        .iter()
        .filter(|node| node.type_id == "select")
        .cloned()
        .collect();
    if select_nodes.is_empty() {
        return Ok(false);
    }
    for node in select_nodes {
        if node.id == PATTERN_ARGS_NODE_ID {
            return Err(format!(
                "Implementation {implementation_id} uses reserved id pattern_args for a select node"
            ));
        }
        if let Some(param) = node
            .params
            .keys()
            .find(|param| !matches!(param.as_str(), "tag_expression" | "spatial_reference"))
        {
            return Err(format!(
                "Implementation {implementation_id} select node {} has unknown parameter {param}",
                node.id
            ));
        }
        if graph.edges.iter().any(|edge| edge.to_node == node.id) {
            return Err(format!(
                "Implementation {implementation_id} select node {} has an unexpected input edge",
                node.id
            ));
        }
        if graph
            .edges
            .iter()
            .any(|edge| edge.from_node == node.id && edge.from_port != "out")
        {
            return Err(format!(
                "Implementation {implementation_id} select node {} has an unknown output port",
                node.id
            ));
        }
        let expression = legacy_truthy_text(implementation_id, &node, "tag_expression", "all")?;
        let spatial_reference =
            legacy_truthy_text(implementation_id, &node, "spatial_reference", "global")?;
        let mut arg_id = "selection".to_string();
        let mut suffix = 2;
        while graph.args.iter().any(|arg| arg.id == arg_id) {
            arg_id = format!("selection_{suffix}");
            suffix += 1;
        }
        graph.args.push(crate::models::node_graph::PatternArgDef {
            id: arg_id.clone(),
            name: arg_id.clone(),
            arg_type: PatternArgType::Selection,
            default_value: json!({
                "expression": expression,
                "spatialReference": spatial_reference,
            }),
        });
        for edge in &mut graph.edges {
            if edge.from_node == node.id && edge.from_port == "out" {
                edge.from_node = PATTERN_ARGS_NODE_ID.into();
                edge.from_port = arg_id.clone();
            }
        }
    }
    Ok(true)
}

fn legacy_truthy_text(
    implementation_id: &str,
    node: &NodeInstance,
    param: &str,
    fallback: &str,
) -> Result<String, String> {
    let Some(value) = node.params.get(param) else {
        return Ok(fallback.into());
    };
    match value {
        Value::String(value) if value.is_empty() => Ok(fallback.into()),
        Value::String(value) => Ok(value.clone()),
        Value::Null | Value::Bool(false) => Ok(fallback.into()),
        Value::Number(value) if value.as_f64() == Some(0.0) => Ok(fallback.into()),
        _ => Err(format!(
            "Implementation {implementation_id} select node {} has non-text {param}",
            node.id
        )),
    }
}

fn remove_retired_nodes(implementation_id: &str, graph: &mut Graph) -> Result<bool, String> {
    let mut retired_ids = HashSet::new();
    for node in &graph.nodes {
        if LEGACY_NODE_TYPES.contains(&node.type_id.as_str()) || node.type_id == "select" {
            retired_ids.insert(node.id.clone());
        } else if node.type_id == "light_points" {
            validate_known_noop_light_points(implementation_id, node)?;
            retired_ids.insert(node.id.clone());
        }
    }
    if retired_ids.is_empty() {
        return Ok(false);
    }
    graph.nodes.retain(|node| !retired_ids.contains(&node.id));
    graph.edges.retain(|edge| {
        !retired_ids.contains(&edge.from_node) && !retired_ids.contains(&edge.to_node)
    });
    Ok(true)
}

/// This exact node was never implemented: the legacy executor logged it as
/// unknown and skipped it, making its incident subgraph a no-op. Removing only
/// the observed shape preserves that behavior without inventing a mapping to
/// the later, semantically different `soft_voronoi` node.
fn validate_known_noop_light_points(
    implementation_id: &str,
    node: &NodeInstance,
) -> Result<(), String> {
    let known = node.params.len() == 4
        && node.params.get("colors") == Some(&Value::String(LEGACY_LIGHT_POINTS_COLORS.into()))
        && node.params.get("speed").and_then(Value::as_f64) == Some(0.01)
        && node.params.get("radius").and_then(Value::as_f64) == Some(0.7)
        && node.params.get("softness").and_then(Value::as_f64) == Some(3.0);
    if known {
        Ok(())
    } else {
        Err(format!(
            "Implementation {implementation_id} node {} is not the known legacy no-op light_points shape",
            node.id
        ))
    }
}

fn canonicalize_pattern_args_node(
    implementation_id: &str,
    graph: &mut Graph,
) -> Result<bool, String> {
    let synthetic_indices: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| (node.type_id == PATTERN_ARGS_NODE_ID).then_some(index))
        .collect();
    if synthetic_indices.len() > 1 {
        return Err(format!(
            "Implementation {implementation_id} has multiple pattern_args nodes"
        ));
    }

    let ordinary_collision = graph
        .nodes
        .iter()
        .any(|node| node.id == PATTERN_ARGS_NODE_ID && node.type_id != PATTERN_ARGS_NODE_ID);
    if ordinary_collision {
        return Err(format!(
            "Implementation {implementation_id} has a non-synthetic node using id pattern_args"
        ));
    }

    if graph.args.is_empty() {
        let Some(index) = synthetic_indices.first().copied() else {
            return Ok(false);
        };
        let id = graph.nodes[index].id.clone();
        graph.nodes.remove(index);
        graph
            .edges
            .retain(|edge| edge.from_node != id && edge.to_node != id);
        return Ok(true);
    }

    let Some(index) = synthetic_indices.first().copied() else {
        graph.nodes.push(NodeInstance {
            id: PATTERN_ARGS_NODE_ID.into(),
            type_id: PATTERN_ARGS_NODE_ID.into(),
            params: Default::default(),
            position_x: Some(-320.0),
            position_y: Some(-120.0),
        });
        return Ok(true);
    };
    if graph.nodes[index].id == PATTERN_ARGS_NODE_ID {
        return Ok(false);
    }

    let previous_id =
        std::mem::replace(&mut graph.nodes[index].id, PATTERN_ARGS_NODE_ID.to_string());
    for edge in &mut graph.edges {
        if edge.from_node == previous_id {
            edge.from_node = PATTERN_ARGS_NODE_ID.into();
        }
        if edge.to_node == previous_id {
            edge.to_node = PATTERN_ARGS_NODE_ID.into();
        }
    }
    Ok(true)
}

/// `withPatternArgsNode` historically discarded wires from arguments that no
/// longer existed after an interface edit. Keep that exact rule: only outgoing
/// wires from the canonical synthetic node are considered, and only when their
/// source port is absent from the current argument interface.
fn remove_stale_pattern_arg_edges(graph: &mut Graph) -> bool {
    let valid_args: HashSet<String> = graph.args.iter().map(|arg| arg.id.clone()).collect();
    let previous_len = graph.edges.len();
    graph.edges.retain(|edge| {
        edge.from_node != PATTERN_ARGS_NODE_ID || valid_args.contains(&edge.from_port)
    });
    graph.edges.len() != previous_len
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn edge(id: &str, from_node: &str, from_port: &str, to_node: &str, to_port: &str) -> Value {
        json!({
            "id": id,
            "fromNode": from_node,
            "fromPort": from_port,
            "toNode": to_node,
            "toPort": to_port,
        })
    }

    fn comprehensive_legacy_graph() -> String {
        json!({
            "nodes": [
                {"id":"args","typeId":"pattern_args","params":{},"positionX":-10,"positionY":-20},
                {"id":"select-old","typeId":"select","params":{"tag_expression":"front_wash","spatial_reference":"group_local"},"positionX":0,"positionY":0},
                {"id":"old-audio","typeId":"audio_source","params":{},"positionX":0,"positionY":0},
                {"id":"points","typeId":"light_points","params":{
                    "colors": LEGACY_LIGHT_POINTS_COLORS,
                    "speed": 0.01,
                    "radius": 0.7,
                    "softness": 3
                },"positionX":0,"positionY":0},
                {"id":"level","typeId":"scalar","params":{"value":1},"positionX":0,"positionY":0},
                {"id":"apply","typeId":"apply_color","params":{},"positionX":0,"positionY":0},
                {"id":"mask","typeId":"random_select_mask","params":{"count":1,"avoid_repeat":1},"positionX":0,"positionY":0},
                {"id":"noise","typeId":"noise","params":{"speed":1,"scale":2,"octaves":1,"amplitude":1,"offset":0},"positionX":0,"positionY":0},
                {"id":"sine","typeId":"sine_wave","params":{"frequency_hz":0.25,"subdivision":0.5,"phase_deg":0,"amplitude":1,"offset":0},"positionX":0,"positionY":0},
                {"id":"view","typeId":"view_signal","params":{},"positionX":0,"positionY":0},
                {"id":"chroma","typeId":"chroma_palette","params":{"palette":"Rainbow"},"positionX":0,"positionY":0}
            ],
            "edges": [
                edge("select","select-old","out","apply","selection"),
                edge("points","points","out","apply","signal"),
                edge("retired","old-audio","out","points","audio"),
                edge("count","args","count","mask","count"),
                edge("time","level","out","noise","time"),
                edge("view","sine","out","view","in")
            ],
            "args": [
                {"id":"selection","name":"selection","argType":"Selection","defaultValue":{"expression":"all","spatialReference":"global"}},
                {"id":"fixtures","name":"Fixtures","argType":"Selection","defaultValue":{"0":"a","1":"l","2":"l","expression":"all","spatialReference":"global"}},
                {"id":"flash_color","name":"flash_color","argType":"Color","defaultValue":"#ff640080"},
                {"id":"count","name":"count","argType":"Scalar","defaultValue":1}
            ]
        })
        .to_string()
    }

    fn clean_graph() -> String {
        r#"{"nodes":[{"id":"value","typeId":"scalar","params":{"value":1},"positionX":0,"positionY":0}],"edges":[],"args":[]}"#.into()
    }

    fn selection_extension_graph() -> String {
        r#"{"nodes":[{"id":"pattern_args","typeId":"pattern_args","params":{},"positionX":0,"positionY":0}],"edges":[],"args":[{"id":"selection","name":"selection","argType":"Selection","defaultValue":{"expression":"all","spatialReference":"global","futureExtension":{"enabled":true}}}]}"#.into()
    }

    fn stale_pattern_arg_edge_graph() -> String {
        json!({
            "nodes": [
                {"id":"pattern_args","typeId":"pattern_args","params":{},"positionX":0,"positionY":0},
                {"id":"apply","typeId":"apply_color","params":{},"positionX":0,"positionY":0},
                {"id":"level","typeId":"scalar","params":{"value":1},"positionX":0,"positionY":0}
            ],
            "edges": [
                edge("stale","pattern_args","color_1","apply","selection"),
                edge("signal","level","out","apply","signal")
            ],
            "args": [
                {"id":"selection","name":"selection","argType":"Selection","defaultValue":{"expression":"all","spatialReference":"global"}}
            ]
        })
        .to_string()
    }

    fn mutate_legacy_node(
        node_id: &str,
        mutate: impl FnOnce(&mut serde_json::Map<String, Value>),
    ) -> String {
        let mut graph: Value = serde_json::from_str(&comprehensive_legacy_graph()).unwrap();
        let node = graph["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["id"] == node_id)
            .unwrap();
        mutate(node["params"].as_object_mut().unwrap());
        graph.to_string()
    }

    async fn test_pool(with_admission: bool) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE implementations (
                id TEXT PRIMARY KEY,
                graph_json TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        if with_admission {
            sqlx::query(
                "CREATE TABLE auth_write_admission (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    armed INTEGER NOT NULL,
                    accepting INTEGER NOT NULL,
                    maintenance INTEGER NOT NULL,
                    remote_writes INTEGER NOT NULL,
                    active_uid TEXT,
                    generation INTEGER NOT NULL,
                    CHECK (maintenance = 0 OR (accepting = 0 AND remote_writes = 0))
                );
                INSERT INTO auth_write_admission VALUES (1, 1, 1, 0, 0, 'alice', 7);
                CREATE TRIGGER guard_implementation_update
                BEFORE UPDATE ON implementations FOR EACH ROW
                WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
                 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
                BEGIN SELECT RAISE(ABORT, 'write admission blocked graph update'); END;",
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        pool
    }

    async fn insert_graph(pool: &SqlitePool, id: &str, graph: &str) {
        sqlx::query("INSERT INTO implementations (id, graph_json) VALUES (?, ?)")
            .bind(id)
            .bind(graph)
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn upgrades_frontend_legacy_semantics_and_current_catalog_markers() {
        let source = comprehensive_legacy_graph();
        let upgraded = upgrade_graph_json("legacy", &source).unwrap().unwrap();
        let graph: Graph = serde_json::from_str(&upgraded).unwrap();

        assert!(graph.nodes.iter().any(|node| {
            node.id == PATTERN_ARGS_NODE_ID && node.type_id == PATTERN_ARGS_NODE_ID
        }));
        assert!(!graph.nodes.iter().any(|node| matches!(
            node.type_id.as_str(),
            "select" | "audio_source" | "light_points"
        )));
        assert!(graph.edges.iter().any(|edge| {
            edge.from_node == PATTERN_ARGS_NODE_ID
                && edge.from_port == "selection_2"
                && edge.to_node == "apply"
                && edge.to_port == "selection"
        }));
        assert!(graph.edges.iter().any(|edge| {
            edge.from_node == PATTERN_ARGS_NODE_ID
                && edge.from_port == "count"
                && edge.to_node == "mask"
        }));
        assert!(!graph.edges.iter().any(|edge| {
            ["points", "old-audio"].contains(&edge.from_node.as_str())
                || ["points", "old-audio"].contains(&edge.to_node.as_str())
        }));

        let selection = graph
            .args
            .iter()
            .find(|arg| arg.id == "selection_2")
            .unwrap();
        assert_eq!(
            selection.default_value,
            json!({"expression":"front_wash","spatialReference":"group_local"})
        );
        let fixtures = graph.args.iter().find(|arg| arg.id == "fixtures").unwrap();
        assert_eq!(fixtures.name, "fixtures");
        assert_eq!(
            fixtures.default_value,
            json!({"expression":"all","spatialReference":"global"})
        );
        assert_eq!(
            graph
                .args
                .iter()
                .find(|arg| arg.id == "flash_color")
                .unwrap()
                .default_value,
            json!({"r":255,"g":100,"b":0,"a":128.0 / 255.0})
        );
        for (node_id, removed_param) in [
            ("mask", "count"),
            ("noise", "speed"),
            ("sine", "frequency_hz"),
            ("chroma", "palette"),
        ] {
            assert!(!graph
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .unwrap()
                .params
                .contains_key(removed_param));
        }
    }

    #[test]
    fn known_light_points_is_removed_because_it_was_a_documented_legacy_noop() {
        let upgraded = upgrade_graph_json("legacy", &comprehensive_legacy_graph())
            .unwrap()
            .unwrap();
        let graph: Graph = serde_json::from_str(&upgraded).unwrap();
        assert!(!graph.nodes.iter().any(|node| node.id == "points"));
        assert!(!graph
            .edges
            .iter()
            .any(|edge| edge.from_node == "points" || edge.to_node == "points"));
    }

    #[test]
    fn clean_current_graph_is_byte_for_byte_untouched() {
        let source = clean_graph();
        assert_eq!(upgrade_graph_json("current", &source).unwrap(), None);
    }

    #[test]
    fn arbitrary_selection_extensions_are_not_mistaken_for_string_spread_debris() {
        let source = selection_extension_graph();
        assert_eq!(upgrade_graph_json("extended", &source).unwrap(), None);
    }

    #[test]
    fn removes_only_outgoing_edges_for_arguments_that_no_longer_exist() {
        let upgraded = upgrade_graph_json("stale-arg", &stale_pattern_arg_edge_graph())
            .unwrap()
            .unwrap();
        let graph: Graph = serde_json::from_str(&upgraded).unwrap();
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from_node, "level");
        assert_eq!(graph.edges[0].to_port, "signal");
    }

    #[test]
    fn ambiguous_select_shape_fails_instead_of_partially_rewiring() {
        let candidate = mutate_legacy_node("select-old", |params| {
            params.insert("future_semantics".into(), Value::Bool(true));
        });
        let error = upgrade_graph_json("ambiguous-select", &candidate).unwrap_err();
        assert!(error.contains("unknown parameter future_semantics"));
    }

    #[test]
    fn fails_closed_for_semantically_ambiguous_legacy_values() {
        let cases = [
            (
                mutate_legacy_node("chroma", |params| {
                    params.insert("palette".into(), Value::String("Custom".into()));
                }),
                "unknown legacy chroma palette",
            ),
            (
                mutate_legacy_node("noise", |params| {
                    params.insert("speed".into(), Value::from(2));
                }),
                "ambiguous legacy noise speed",
            ),
            (
                mutate_legacy_node("sine", |params| {
                    params.remove("subdivision");
                }),
                "without subdivision",
            ),
            (
                mutate_legacy_node("points", |params| {
                    params.insert("colors".into(), Value::String("[]".into()));
                }),
                "not the known legacy no-op light_points shape",
            ),
        ];
        for (candidate, expected) in cases {
            let error = upgrade_graph_json("ambiguous", &candidate).unwrap_err();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[tokio::test]
    async fn transaction_is_idempotent_and_restores_armed_admission_exactly() {
        let pool = test_pool(true).await;
        insert_graph(&pool, "legacy", &comprehensive_legacy_graph()).await;
        insert_graph(&pool, "current", &clean_graph()).await;
        let current_before: String =
            sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = 'current'")
                .fetch_one(&pool)
                .await
                .unwrap();

        upgrade_legacy_graph_json(&pool).await.unwrap();
        let first: String =
            sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = 'legacy'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let current_after: String =
            sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = 'current'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let admission: (i64, i64, i64, i64, Option<String>, i64) = sqlx::query_as(
            "SELECT armed, accepting, maintenance, remote_writes, active_uid, generation
             FROM auth_write_admission WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(current_after, current_before);
        assert_eq!(admission, (1, 1, 0, 0, Some("alice".into()), 7));

        upgrade_legacy_graph_json(&pool).await.unwrap();
        let second: String =
            sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = 'legacy'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(second, first);
    }

    #[tokio::test]
    async fn prevalidation_failure_writes_no_graph_or_admission_state() {
        let pool = test_pool(true).await;
        let valid = comprehensive_legacy_graph();
        let ambiguous = mutate_legacy_node("sine", |params| {
            params.remove("subdivision");
        });
        insert_graph(&pool, "a-valid", &valid).await;
        insert_graph(&pool, "z-ambiguous", &ambiguous).await;

        let error = upgrade_legacy_graph_json(&pool).await.unwrap_err();
        assert!(error.contains("without subdivision"));
        let stored: Vec<(String, String)> =
            sqlx::query_as("SELECT id, graph_json FROM implementations ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            stored,
            vec![("a-valid".into(), valid), ("z-ambiguous".into(), ambiguous)]
        );
        let admission: (i64, i64, i64, i64, Option<String>, i64) = sqlx::query_as(
            "SELECT armed, accepting, maintenance, remote_writes, active_uid, generation
             FROM auth_write_admission WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(admission, (1, 1, 0, 0, Some("alice".into()), 7));
    }

    #[tokio::test]
    async fn write_failure_rolls_back_prior_graph_and_admission_transition() {
        let pool = test_pool(true).await;
        let first = comprehensive_legacy_graph();
        let second = comprehensive_legacy_graph();
        insert_graph(&pool, "a-first", &first).await;
        insert_graph(&pool, "z-fail", &second).await;
        sqlx::query(
            "CREATE TRIGGER fail_second_graph_upgrade
             BEFORE UPDATE ON implementations FOR EACH ROW
             WHEN OLD.id = 'z-fail'
             BEGIN SELECT RAISE(ABORT, 'forced second graph failure'); END;",
        )
        .execute(&pool)
        .await
        .unwrap();

        let error = upgrade_legacy_graph_json(&pool).await.unwrap_err();
        assert!(error.contains("forced second graph failure"));
        let stored: Vec<(String, String)> =
            sqlx::query_as("SELECT id, graph_json FROM implementations ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            stored,
            vec![("a-first".into(), first), ("z-fail".into(), second)]
        );
        let admission: (i64, i64, i64, i64, Option<String>, i64) = sqlx::query_as(
            "SELECT armed, accepting, maintenance, remote_writes, active_uid, generation
             FROM auth_write_admission WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(admission, (1, 1, 0, 0, Some("alice".into()), 7));
    }

    #[tokio::test]
    async fn missing_implementations_table_is_a_clean_noop() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        upgrade_legacy_graph_json(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn app_database_init_upgrades_existing_graph_before_returning() {
        let directory = tempfile::tempdir().unwrap();
        let db = super::super::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        sqlx::query("INSERT INTO patterns (id, name) VALUES ('pattern', 'pattern')")
            .execute(&db.0)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json)
             VALUES ('implementation', 'pattern', ?)",
        )
        .bind(comprehensive_legacy_graph())
        .execute(&db.0)
        .await
        .unwrap();
        db.0.close().await;

        let reopened = super::super::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        let stored: String = sqlx::query_scalar(
            "SELECT graph_json FROM implementations WHERE id = 'implementation'",
        )
        .fetch_one(&reopened.0)
        .await
        .unwrap();
        let graph: Graph = serde_json::from_str(&stored).unwrap();
        assert!(graph.nodes.iter().any(|node| {
            node.id == PATTERN_ARGS_NODE_ID && node.type_id == PATTERN_ARGS_NODE_ID
        }));
        assert!(!graph.nodes.iter().any(|node| matches!(
            node.type_id.as_str(),
            "select" | "audio_source" | "light_points"
        )));
        reopened.0.close().await;
    }

    #[tokio::test]
    async fn non_quiescent_admission_fails_without_writing() {
        let pool = test_pool(true).await;
        insert_graph(&pool, "legacy", &comprehensive_legacy_graph()).await;
        sqlx::query(
            "UPDATE auth_write_admission
             SET accepting = 0, maintenance = 0, remote_writes = 1",
        )
        .execute(&pool)
        .await
        .unwrap();
        let before: String =
            sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = 'legacy'")
                .fetch_one(&pool)
                .await
                .unwrap();

        let error = upgrade_legacy_graph_json(&pool).await.unwrap_err();
        assert!(error.contains("not quiescent"));
        let after: String =
            sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = 'legacy'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after, before);
    }
}
