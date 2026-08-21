//! Validation, canonicalization, and relational projection for pattern graphs.
//!
//! A graph is authored state, not an opaque JSON preference. `AuthoredDocuments`
//! owns relational revision history and the outer transaction; this module owns canonical
//! ordering, structural/type validation, revision checks, and the
//! in-transaction SQLite projector.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use ts_rs::TS;

use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::models::node_graph::{
    Edge, Graph, NodeInstance, NodeTypeDef, ParamType, PatternArgDef, PatternArgType, PortType,
};

const REVISION_DOMAIN: &[u8] = b"luma.graph-document.v1\0";
const GRAPH_FILE_V1_SCHEMA_VERSION: u32 = 1;
const GRAPH_FILE_SCHEMA_VERSION: u32 = GRAPH_FILE_V1_SCHEMA_VERSION;
const MAX_GRAPH_JSON_BYTES: usize = 6 * 1024 * 1024;
const MAX_NODES: usize = 4096;
const MAX_EDGES: usize = 16_384;
const MAX_ARGS: usize = 512;
const PATTERN_ARGS_NODE_ID: &str = "pattern_args";

/// Version 1 of the durable semantic graph file. These wire structs are kept
/// separate from the live executor model on purpose: a future breaking node,
/// port, or field change adds a new version and a sequential migration without
/// changing how an old commit is decoded.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphFileV1 {
    schema_version: u32,
    nodes: Vec<GraphNodeV1>,
    edges: Vec<GraphEdgeV1>,
    args: Vec<GraphArgV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphNodeV1 {
    id: String,
    type_id: String,
    params: HashMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphEdgeV1 {
    id: String,
    from_node: String,
    from_port: String,
    to_node: String,
    to_port: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphArgV1 {
    id: String,
    name: String,
    arg_type: GraphArgTypeV1,
    default_value: Value,
}

/// Frozen argument-type vocabulary for graph schema v1. Do not add, remove,
/// or rename variants here after release; change the current wire version and
/// register a migration instead.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum GraphArgTypeV1 {
    Color,
    Scalar,
    Selection,
    Palette,
    Gradient,
}

impl From<&PatternArgType> for GraphArgTypeV1 {
    fn from(value: &PatternArgType) -> Self {
        match value {
            PatternArgType::Color => Self::Color,
            PatternArgType::Scalar => Self::Scalar,
            PatternArgType::Selection => Self::Selection,
            PatternArgType::Palette => Self::Palette,
            PatternArgType::Gradient => Self::Gradient,
        }
    }
}

impl From<GraphArgTypeV1> for PatternArgType {
    fn from(value: GraphArgTypeV1) -> Self {
        match value {
            GraphArgTypeV1::Color => Self::Color,
            GraphArgTypeV1::Scalar => Self::Scalar,
            GraphArgTypeV1::Selection => Self::Selection,
            GraphArgTypeV1::Palette => Self::Palette,
            GraphArgTypeV1::Gradient => Self::Gradient,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphLayoutFileV1 {
    schema_version: u32,
    nodes: BTreeMap<String, GraphNodeLayoutV1>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphNodeLayoutV1 {
    position_x: Option<f64>,
    position_y: Option<f64>,
}

impl GraphFileV1 {
    fn from_graph(graph: &Graph) -> Self {
        Self {
            schema_version: GRAPH_FILE_V1_SCHEMA_VERSION,
            nodes: graph
                .nodes
                .iter()
                .map(|node| GraphNodeV1 {
                    id: node.id.clone(),
                    type_id: node.type_id.clone(),
                    params: node.params.clone(),
                })
                .collect(),
            edges: graph
                .edges
                .iter()
                .map(|edge| GraphEdgeV1 {
                    id: edge.id.clone(),
                    from_node: edge.from_node.clone(),
                    from_port: edge.from_port.clone(),
                    to_node: edge.to_node.clone(),
                    to_port: edge.to_port.clone(),
                })
                .collect(),
            args: graph
                .args
                .iter()
                .map(|arg| GraphArgV1 {
                    id: arg.id.clone(),
                    name: arg.name.clone(),
                    arg_type: (&arg.arg_type).into(),
                    default_value: arg.default_value.clone(),
                })
                .collect(),
        }
    }

    fn into_graph(self) -> Graph {
        Graph {
            nodes: self
                .nodes
                .into_iter()
                .map(|node| NodeInstance {
                    id: node.id,
                    type_id: node.type_id,
                    params: node.params,
                    position_x: None,
                    position_y: None,
                })
                .collect(),
            edges: self
                .edges
                .into_iter()
                .map(|edge| Edge {
                    id: edge.id,
                    from_node: edge.from_node,
                    from_port: edge.from_port,
                    to_node: edge.to_node,
                    to_port: edge.to_port,
                })
                .collect(),
            args: self
                .args
                .into_iter()
                .map(|arg| PatternArgDef {
                    id: arg.id,
                    name: arg.name,
                    arg_type: arg.arg_type.into(),
                    default_value: arg.default_value,
                })
                .collect(),
        }
    }
}

impl GraphLayoutFileV1 {
    fn from_graph(graph: &Graph) -> Self {
        Self {
            schema_version: GRAPH_FILE_V1_SCHEMA_VERSION,
            nodes: graph
                .nodes
                .iter()
                .map(|node| {
                    (
                        node.id.clone(),
                        GraphNodeLayoutV1 {
                            position_x: node.position_x,
                            position_y: node.position_y,
                        },
                    )
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphScope {
    pub pattern_id: String,
    pub implementation_id: String,
    /// Trusted current principal. `None` owns only local signed-out patterns.
    pub owner_user_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct GraphDocument {
    pub implementation_id: String,
    pub revision: String,
    pub graph: Graph,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct GraphEditPlan {
    pub base_revision: String,
    pub candidate: Graph,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct GraphEditResult {
    pub revision: String,
    pub graph: Graph,
    pub changed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphValidationIssue {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphDocumentError {
    Conflict {
        #[serde(rename = "expectedRevision")]
        expected_revision: String,
        #[serde(rename = "currentRevision")]
        current_revision: String,
    },
    Invalid {
        issues: Vec<GraphValidationIssue>,
    },
    Scope {
        message: String,
    },
    Storage {
        message: String,
    },
}

impl GraphDocumentError {
    fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid {
            issues: vec![GraphValidationIssue {
                path: path.into(),
                message: message.into(),
            }],
        }
    }

    fn storage(message: impl Into<String>) -> Self {
        Self::Storage {
            message: message.into(),
        }
    }
}

impl fmt::Display for GraphDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict {
                expected_revision,
                current_revision,
            } => write!(
                formatter,
                "graph changed while this edit was open (expected {expected_revision}, current {current_revision})"
            ),
            Self::Invalid { issues } => write!(
                formatter,
                "invalid graph: {}",
                issues
                    .iter()
                    .map(|issue| format!("{}: {}", issue.path, issue.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::Scope { message } | Self::Storage { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for GraphDocumentError {}

#[derive(FromRow)]
struct ImplementationRow {
    id: String,
    name: Option<String>,
    graph_json: String,
}

/// The score codec depends on a pattern's public argument interface, not its
/// executable graph internals. Deserializing this narrow projection lets a
/// score remain authorable when a legacy implementation contains stale nodes
/// or edges, while malformed argument definitions still fail closed.
#[derive(Deserialize)]
struct PatternInterface {
    #[serde(default)]
    args: Vec<PatternArgDef>,
}

/// Resolve the implementation a caller intends to use, before entering the
/// authored-document layer. An explicit implementation is authoritative; a
/// venue override is next; otherwise the sole unnamed implementation is the
/// pattern default. A pattern with several possible defaults is invalid rather
/// than being resolved by row order.
pub async fn resolve_graph_implementation(
    pool: &SqlitePool,
    pattern_id: &str,
    venue_id: Option<&str>,
    explicit_implementation_id: Option<&str>,
) -> Result<String, GraphDocumentError> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| GraphDocumentError::storage(format!("open graph resolver: {error}")))?;

    resolve_graph_implementation_for_connection(
        &mut connection,
        pattern_id,
        venue_id,
        explicit_implementation_id,
    )
    .await
}

pub(crate) async fn resolve_graph_implementation_for_connection(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    venue_id: Option<&str>,
    explicit_implementation_id: Option<&str>,
) -> Result<String, GraphDocumentError> {
    let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM patterns WHERE id = ?")
        .bind(pattern_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| GraphDocumentError::storage(format!("resolve pattern: {error}")))?;
    if exists.is_none() {
        return Err(GraphDocumentError::Scope {
            message: format!("pattern {pattern_id} does not exist"),
        });
    }

    if let Some(implementation_id) = explicit_implementation_id {
        ensure_implementation_belongs_to_pattern(connection, pattern_id, implementation_id).await?;
        return Ok(implementation_id.to_owned());
    }

    if let Some(venue_id) = venue_id {
        let override_id = sqlx::query_scalar::<_, String>(
            "SELECT implementation_id FROM venue_implementation_overrides
             WHERE venue_id = ? AND pattern_id = ?",
        )
        .bind(venue_id)
        .bind(pattern_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| {
            GraphDocumentError::storage(format!("resolve venue implementation override: {error}"))
        })?;
        if let Some(implementation_id) = override_id {
            if let Err(error) =
                ensure_implementation_belongs_to_pattern(connection, pattern_id, &implementation_id)
                    .await
            {
                return match error {
                    GraphDocumentError::Scope { .. } => Err(GraphDocumentError::Scope {
                        message: format!(
                            "venue {venue_id} selects implementation {implementation_id}, which does not belong to pattern {pattern_id}"
                        ),
                    }),
                    other => Err(other),
                };
            }
            return Ok(implementation_id);
        }
    }

    resolve_default_implementation(connection, pattern_id).await
}

/// Load one immutable graph snapshot through the single admitted pattern
/// capability. Venue-specific resolution uses the same venue read transaction
/// for visibility, override selection, and graph bytes.
pub async fn load_visible_graph_document(
    pool: &SqlitePool,
    pattern_id: &str,
    venue_id: Option<&str>,
    implementation_id: Option<&str>,
) -> Result<GraphDocument, GraphDocumentError> {
    match venue_id {
        Some(venue_id) => {
            let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id))
                .await
                .map_err(|message| GraphDocumentError::Scope { message })?;
            load_visible_graph_document_for_connection(
                access.connection(),
                pattern_id,
                Some(venue_id),
                implementation_id,
            )
            .await
        }
        None => {
            let mut transaction = pool.begin().await.map_err(|error| {
                GraphDocumentError::storage(format!("begin visible graph read: {error}"))
            })?;
            load_visible_graph_document_for_connection(
                &mut transaction,
                pattern_id,
                None,
                implementation_id,
            )
            .await
        }
    }
}

async fn load_visible_graph_document_for_connection(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    venue_id: Option<&str>,
    implementation_id: Option<&str>,
) -> Result<GraphDocument, GraphDocumentError> {
    let visible: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM auth_visible_patterns WHERE pattern_id = ?")
            .bind(pattern_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| {
                GraphDocumentError::storage(format!("authorize visible pattern: {error}"))
            })?;
    if visible.is_none() {
        return Err(GraphDocumentError::Scope {
            message: format!("pattern {pattern_id} does not exist"),
        });
    }
    let implementation_id = resolve_graph_implementation_for_connection(
        connection,
        pattern_id,
        venue_id,
        implementation_id,
    )
    .await?;
    load_unscoped_graph_document_for_connection(connection, pattern_id, &implementation_id).await
}

/// Validate and deterministically order a graph without consulting the live
/// executor catalog. Durable revisions, historical restores, and typed merges
/// use this path so an installed node upgrade cannot make old history opaque.
pub fn canonicalize_graph_structure(graph: &Graph) -> Result<Graph, GraphDocumentError> {
    validate_graph_structure(graph).map_err(|issues| GraphDocumentError::Invalid { issues })?;
    Ok(canonical_graph_order(graph))
}

/// Validate against the current executor catalog, then deterministically order
/// the graph. Current relational projection must always use this path.
pub fn canonicalize_graph(graph: &Graph) -> Result<Graph, GraphDocumentError> {
    validate_graph(graph).map_err(|issues| GraphDocumentError::Invalid { issues })?;
    Ok(canonical_graph_order(graph))
}

fn canonical_graph_order(graph: &Graph) -> Graph {
    let mut graph = graph.clone();
    graph.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    graph.args.sort_by(|left, right| left.id.cmp(&right.id));
    for edge in &mut graph.edges {
        edge.id = canonical_edge_id(edge);
    }
    graph.edges.sort_by(|left, right| {
        left.to_node
            .cmp(&right.to_node)
            .then(left.to_port.cmp(&right.to_port))
            .then(left.from_node.cmp(&right.from_node))
            .then(left.from_port.cmp(&right.from_port))
    });
    graph
}

/// Validate and deterministically order the public argument interface of a
/// pattern independently from the implementation graph that consumes it.
pub(crate) fn canonicalize_pattern_args(
    args: &[PatternArgDef],
) -> Result<Vec<PatternArgDef>, GraphDocumentError> {
    let mut issues = Vec::new();
    validate_pattern_args(args, &mut issues);
    issues.sort();
    issues.dedup();
    if !issues.is_empty() {
        return Err(GraphDocumentError::Invalid { issues });
    }

    let mut args = args.to_vec();
    args.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(args)
}

/// Revision of the exact authored graph, including layout coordinates.
pub fn graph_revision(graph: &Graph) -> Result<String, GraphDocumentError> {
    let graph = canonicalize_graph_structure(graph)?;
    let value = serde_json::to_value(graph)
        .map_err(|error| GraphDocumentError::storage(format!("serialize graph: {error}")))?;
    let mut hash = Sha256::new();
    hash.update(REVISION_DOMAIN);
    hash.update(crate::canonical_json::to_string(&value).as_bytes());
    Ok(format!("sha256:{:x}", hash.finalize()))
}

/// Canonical, current-version semantic graph file. Layout is deliberately
/// stored separately, and neither serializer consults the installed catalog.
pub fn semantic_graph_json(graph: &Graph) -> Result<String, GraphDocumentError> {
    let graph = canonicalize_graph_structure(graph)?;
    let value = serde_json::to_value(GraphFileV1::from_graph(&graph))
        .map_err(|error| GraphDocumentError::storage(format!("serialize graph: {error}")))?;
    Ok(format!("{}\n", crate::canonical_json::to_string(&value)))
}

/// Canonical, current-version non-semantic layout keyed by stable node ID.
pub fn graph_layout_json(graph: &Graph) -> Result<String, GraphDocumentError> {
    let graph = canonicalize_graph_structure(graph)?;
    let value = serde_json::to_value(GraphLayoutFileV1::from_graph(&graph))
        .map_err(|error| GraphDocumentError::storage(format!("serialize layout: {error}")))?;
    Ok(format!("{}\n", crate::canonical_json::to_string(&value)))
}

/// Decode and migrate the two tracked files into the current structural graph
/// model. This codec is deliberately catalog-independent; projection performs
/// current-runtime validation later, immediately before SQLite/main advance.
pub fn graph_from_files(
    semantic_json: &str,
    layout_json: &str,
) -> Result<Graph, GraphDocumentError> {
    let semantic = parse_graph_file(semantic_json, "graph.json")?;
    let layout = parse_graph_file(layout_json, "layout.json")?;
    let semantic_version = graph_file_version(&semantic, "graph.json")?;
    let layout_version = graph_file_version(&layout, "layout.json")?;
    if semantic_version != layout_version {
        return Err(GraphDocumentError::invalid(
            "layout.json.schemaVersion",
            format!(
                "schema version {layout_version} does not match graph.json version {semantic_version}"
            ),
        ));
    }
    let (semantic, layout) = migrate_graph_files_to_current(semantic_version, semantic, layout)?;
    validate_graph_file_v1_fields(&semantic)?;
    validate_layout_file_v1_fields(&layout)?;
    let semantic: GraphFileV1 = serde_json::from_value(semantic)
        .map_err(|error| GraphDocumentError::invalid("graph.json", error.to_string()))?;
    let layout: GraphLayoutFileV1 = serde_json::from_value(layout)
        .map_err(|error| GraphDocumentError::invalid("layout.json", error.to_string()))?;
    let mut graph = semantic.into_graph();
    let node_ids: BTreeSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    for id in layout.nodes.keys() {
        if !node_ids.contains(id.as_str()) {
            return Err(GraphDocumentError::invalid(
                format!("layout.json.nodes.{id}"),
                "layout references an unknown node",
            ));
        }
    }
    for node in &mut graph.nodes {
        if let Some(position) = layout.nodes.get(&node.id) {
            node.position_x = position.position_x;
            node.position_y = position.position_y;
        }
    }
    canonicalize_graph_structure(&graph)
}

fn parse_graph_file(source: &str, path: &str) -> Result<Value, GraphDocumentError> {
    if source.len() > MAX_GRAPH_JSON_BYTES {
        return Err(GraphDocumentError::invalid(
            path,
            format!("file exceeds {MAX_GRAPH_JSON_BYTES} encoded bytes"),
        ));
    }
    serde_json::from_str(source)
        .map_err(|error| GraphDocumentError::invalid(path, error.to_string()))
}

fn graph_file_version(value: &Value, path: &str) -> Result<u32, GraphDocumentError> {
    let object = value.as_object().ok_or_else(|| {
        GraphDocumentError::invalid(path, "versioned graph file must be a JSON object")
    })?;
    let version_path = format!("{path}.schemaVersion");
    let version = object
        .get("schemaVersion")
        .ok_or_else(|| GraphDocumentError::invalid(&version_path, "schema version is required"))?;
    let version = version.as_u64().ok_or_else(|| {
        GraphDocumentError::invalid(&version_path, "schema version must be an integer")
    })?;
    u32::try_from(version)
        .map_err(|_| GraphDocumentError::invalid(&version_path, "schema version is out of range"))
}

/// Sequential migration seam for durable authored graph files. When version 2 is
/// introduced, add exactly the `1 -> 2` transform to
/// `migrate_graph_files_once`; later versions continue one step at a time.
fn migrate_graph_files_to_current(
    mut version: u32,
    mut semantic: Value,
    mut layout: Value,
) -> Result<(Value, Value), GraphDocumentError> {
    if version > GRAPH_FILE_SCHEMA_VERSION {
        return Err(GraphDocumentError::invalid(
            "graph.json.schemaVersion",
            format!(
                "unsupported graph schema version {version}; current version is {GRAPH_FILE_SCHEMA_VERSION}"
            ),
        ));
    }
    while version < GRAPH_FILE_SCHEMA_VERSION {
        (semantic, layout) = migrate_graph_files_once(version, semantic, layout)?;
        version += 1;
    }
    Ok((semantic, layout))
}

fn migrate_graph_files_once(
    version: u32,
    _semantic: Value,
    _layout: Value,
) -> Result<(Value, Value), GraphDocumentError> {
    Err(GraphDocumentError::invalid(
        "graph.json.schemaVersion",
        format!("unsupported graph schema version {version}; no migration is registered"),
    ))
}

fn validate_graph_file_v1_fields(value: &Value) -> Result<(), GraphDocumentError> {
    reject_unknown_fields(
        value,
        "graph.json",
        &["schemaVersion", "nodes", "edges", "args"],
    )?;
    if let Some(nodes) = value.get("nodes").and_then(Value::as_array) {
        for (index, node) in nodes.iter().enumerate() {
            reject_unknown_fields(
                node,
                &format!("graph.json.nodes[{index}]"),
                &["id", "typeId", "params"],
            )?;
        }
    }
    if let Some(edges) = value.get("edges").and_then(Value::as_array) {
        for (index, edge) in edges.iter().enumerate() {
            reject_unknown_fields(
                edge,
                &format!("graph.json.edges[{index}]"),
                &["id", "fromNode", "fromPort", "toNode", "toPort"],
            )?;
        }
    }
    if let Some(args) = value.get("args").and_then(Value::as_array) {
        for (index, arg) in args.iter().enumerate() {
            reject_unknown_fields(
                arg,
                &format!("graph.json.args[{index}]"),
                &["id", "name", "argType", "defaultValue"],
            )?;
        }
    }
    Ok(())
}

fn validate_layout_file_v1_fields(value: &Value) -> Result<(), GraphDocumentError> {
    reject_unknown_fields(value, "layout.json", &["schemaVersion", "nodes"])?;
    if let Some(nodes) = value.get("nodes").and_then(Value::as_object) {
        for (id, position) in nodes {
            reject_unknown_fields(
                position,
                &format!("layout.json.nodes.{id}"),
                &["positionX", "positionY"],
            )?;
        }
    }
    Ok(())
}

fn reject_unknown_fields(
    value: &Value,
    path: &str,
    allowed: &[&str],
) -> Result<(), GraphDocumentError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    unknown.sort_unstable();
    if let Some(field) = unknown.first() {
        return Err(GraphDocumentError::invalid(
            format!("{path}.{field}"),
            "unknown field",
        ));
    }
    Ok(())
}

/// Catalog-independent invariants encoded by graph schema version 1.
pub fn validate_graph_structure(graph: &Graph) -> Result<(), Vec<GraphValidationIssue>> {
    let mut issues = Vec::new();
    if graph.nodes.len() > MAX_NODES {
        issue(
            &mut issues,
            "nodes",
            format!("at most {MAX_NODES} nodes are allowed"),
        );
    }
    if graph.edges.len() > MAX_EDGES {
        issue(
            &mut issues,
            "edges",
            format!("at most {MAX_EDGES} edges are allowed"),
        );
    }
    let mut nodes = HashMap::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        let path = format!("nodes[{index}]");
        if node.id.is_empty() {
            issue(&mut issues, format!("{path}.id"), "node id cannot be empty");
        }
        if node.type_id.is_empty() {
            issue(
                &mut issues,
                format!("{path}.typeId"),
                "node type cannot be empty",
            );
        }
        if nodes.insert(node.id.as_str(), node).is_some() {
            issue(
                &mut issues,
                format!("{path}.id"),
                format!("duplicate node id {}", node.id),
            );
        }
        if node.position_x.is_some_and(|value| !value.is_finite())
            || node.position_y.is_some_and(|value| !value.is_finite())
        {
            issue(
                &mut issues,
                format!("{path}.position"),
                "position must be finite",
            );
        }
        if node.id == PATTERN_ARGS_NODE_ID || node.type_id == PATTERN_ARGS_NODE_ID {
            if node.id != PATTERN_ARGS_NODE_ID || node.type_id != PATTERN_ARGS_NODE_ID {
                issue(
                    &mut issues,
                    path.clone(),
                    "the synthetic pattern_args node must use pattern_args for both id and type",
                );
            }
            if !node.params.is_empty() {
                issue(
                    &mut issues,
                    format!("{path}.params"),
                    "pattern_args cannot have parameters",
                );
            }
        }
    }

    validate_pattern_args(&graph.args, &mut issues);

    let pattern_args_nodes = graph
        .nodes
        .iter()
        .filter(|node| node.id == PATTERN_ARGS_NODE_ID && node.type_id == PATTERN_ARGS_NODE_ID)
        .count();
    if graph.args.is_empty() && pattern_args_nodes != 0 {
        issue(
            &mut issues,
            "nodes.pattern_args",
            "pattern_args must be absent when the graph has no arguments",
        );
    } else if !graph.args.is_empty() && pattern_args_nodes != 1 {
        issue(
            &mut issues,
            "nodes.pattern_args",
            "a graph with arguments requires exactly one pattern_args node",
        );
    }

    let mut input_slots = HashSet::new();
    let mut edge_ids = HashSet::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        let path = format!("edges[{index}]");
        if !edge_ids.insert(edge.id.as_str()) {
            issue(
                &mut issues,
                format!("{path}.id"),
                format!("duplicate edge id {}", edge.id),
            );
        }
        if edge.from_port.is_empty() {
            issue(
                &mut issues,
                format!("{path}.fromPort"),
                "output port cannot be empty",
            );
        }
        if edge.to_port.is_empty() {
            issue(
                &mut issues,
                format!("{path}.toPort"),
                "input port cannot be empty",
            );
        }
        if !input_slots.insert((edge.to_node.as_str(), edge.to_port.as_str())) {
            issue(
                &mut issues,
                path.clone(),
                format!(
                    "input {}.{} has more than one source",
                    edge.to_node, edge.to_port
                ),
            );
        }
        if !nodes.contains_key(edge.from_node.as_str()) {
            issue(
                &mut issues,
                format!("{path}.fromNode"),
                format!("unknown node {}", edge.from_node),
            );
        }
        if !nodes.contains_key(edge.to_node.as_str()) {
            issue(
                &mut issues,
                format!("{path}.toNode"),
                format!("unknown node {}", edge.to_node),
            );
        }
    }

    validate_dag(graph, &mut issues);
    let encoded_bytes = serde_json::to_vec(graph).map_or(usize::MAX, |bytes| bytes.len());
    if encoded_bytes > MAX_GRAPH_JSON_BYTES {
        issue(
            &mut issues,
            "graph",
            format!("graph exceeds {MAX_GRAPH_JSON_BYTES} encoded bytes"),
        );
    }

    finish_validation(issues)
}

/// Full current-runtime validation against the installed node catalog.
pub fn validate_graph(graph: &Graph) -> Result<(), Vec<GraphValidationIssue>> {
    let mut issues = validate_graph_structure(graph).err().unwrap_or_default();
    let defs: HashMap<String, NodeTypeDef> = crate::node_graph::nodes::get_node_types()
        .into_iter()
        .map(|definition| (definition.id.clone(), definition))
        .collect();
    let nodes: HashMap<&str, &NodeInstance> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    for (index, node) in graph.nodes.iter().enumerate() {
        if node.id == PATTERN_ARGS_NODE_ID && node.type_id == PATTERN_ARGS_NODE_ID {
            continue;
        }
        let path = format!("nodes[{index}]");
        let Some(definition) = defs.get(&node.type_id) else {
            issue(
                &mut issues,
                format!("{path}.typeId"),
                format!("unknown node type {}", node.type_id),
            );
            continue;
        };
        validate_params(&mut issues, &path, node, definition);
    }

    let args: HashMap<&str, &PatternArgDef> = graph
        .args
        .iter()
        .map(|arg| (arg.id.as_str(), arg))
        .collect();
    for (index, edge) in graph.edges.iter().enumerate() {
        let path = format!("edges[{index}]");
        let (Some(from), Some(to)) = (
            nodes.get(edge.from_node.as_str()).copied(),
            nodes.get(edge.to_node.as_str()).copied(),
        ) else {
            continue;
        };
        let output = output_port_type(from, &edge.from_port, &defs, &args);
        let input = input_port_type(to, &edge.to_port, &defs);
        match (output, input) {
            (None, _) => issue(
                &mut issues,
                format!("{path}.fromPort"),
                format!("unknown output {}.{}", edge.from_node, edge.from_port),
            ),
            (_, None) => issue(
                &mut issues,
                format!("{path}.toPort"),
                format!("unknown input {}.{}", edge.to_node, edge.to_port),
            ),
            (Some(output), Some(input)) if output != input => issue(
                &mut issues,
                path,
                format!("port type mismatch: {output:?} cannot feed {input:?}"),
            ),
            _ => {}
        }
    }
    finish_validation(issues)
}

fn finish_validation(
    mut issues: Vec<GraphValidationIssue>,
) -> Result<(), Vec<GraphValidationIssue>> {
    issues.sort();
    issues.dedup();
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

pub async fn load_graph_document(
    pool: &SqlitePool,
    scope: &GraphScope,
) -> Result<GraphDocument, GraphDocumentError> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| GraphDocumentError::storage(format!("open graph read: {error}")))?;
    authorize_scope(&mut connection, scope).await?;
    load_document(&mut connection, &scope.pattern_id, &scope.implementation_id).await
}

/// Strict exact-implementation read for evaluation, export, and forks. Unlike
/// the scoped editor path this does not confer mutation authority. Callers must
/// first resolve venue/default selection with [`resolve_graph_implementation`]
/// and then retain this immutable identity for the whole operation.
pub async fn load_graph_document_unscoped(
    pool: &SqlitePool,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<GraphDocument, GraphDocumentError> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| GraphDocumentError::storage(format!("open graph read: {error}")))?;
    ensure_implementation_belongs_to_pattern(&mut connection, pattern_id, implementation_id)
        .await?;
    load_document(&mut connection, pattern_id, implementation_id).await
}

pub(crate) async fn load_pattern_interface_for_connection(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<Vec<PatternArgDef>, GraphDocumentError> {
    load_pattern_interface(connection, pattern_id, implementation_id).await
}

/// Transaction-local exact-implementation read for compound authored
/// operations such as pattern fork. The caller owns the surrounding SQLite
/// snapshot, so pattern metadata and graph bytes cannot come from different
/// revisions.
pub(crate) async fn load_unscoped_graph_document_for_connection(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<GraphDocument, GraphDocumentError> {
    load_document(connection, pattern_id, implementation_id).await
}

/// Test harness for exercising the in-transaction projector in isolation.
#[cfg(test)]
async fn apply_graph_edit(
    pool: &SqlitePool,
    scope: &GraphScope,
    plan: GraphEditPlan,
) -> Result<GraphEditResult, GraphDocumentError> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| GraphDocumentError::storage(format!("begin graph edit: {error}")))?;
    let result = apply_graph_edit_in_transaction(&mut transaction, scope, plan).await?;
    transaction
        .commit()
        .await
        .map_err(|error| GraphDocumentError::storage(format!("commit graph edit: {error}")))?;
    Ok(result)
}

/// Apply a graph through the same validator/CAS path while participating in a
/// caller-owned SQLite transaction. Authored-state projection uses this to
/// update the relational graph and its authored revision as one atomic
/// database operation.
pub(crate) async fn apply_graph_edit_in_transaction(
    connection: &mut SqliteConnection,
    scope: &GraphScope,
    plan: GraphEditPlan,
) -> Result<GraphEditResult, GraphDocumentError> {
    let candidate = canonicalize_graph(&plan.candidate)?;
    let graph_json = exact_graph_json(&candidate)?;
    authorize_scope(connection, scope).await?;
    let current = load_document(connection, &scope.pattern_id, &scope.implementation_id).await?;
    if current.revision != plan.base_revision {
        return Err(GraphDocumentError::Conflict {
            expected_revision: plan.base_revision,
            current_revision: current.revision,
        });
    }
    let revision = graph_revision(&candidate)?;
    let changed = revision != current.revision;
    if changed {
        let updated = sqlx::query(
            "UPDATE implementations SET graph_json = ? WHERE id = ? AND pattern_id = ?",
        )
        .bind(&graph_json)
        .bind(&scope.implementation_id)
        .bind(&scope.pattern_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            GraphDocumentError::storage(format!("update graph implementation: {error}"))
        })?
        .rows_affected();
        if updated != 1 {
            return Err(GraphDocumentError::storage(
                "graph implementation disappeared during exclusive edit",
            ));
        }
    }
    Ok(GraphEditResult {
        revision,
        graph: candidate,
        changed,
    })
}

async fn authorize_scope(
    connection: &mut SqliteConnection,
    scope: &GraphScope,
) -> Result<(), GraphDocumentError> {
    let found = match scope.owner_user_id.as_deref() {
        Some(owner) => {
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM patterns WHERE id = ? AND uid = ?")
                .bind(&scope.pattern_id)
                .bind(owner)
                .fetch_optional(&mut *connection)
                .await
        }
        None => {
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM patterns WHERE id = ? AND uid IS NULL")
                .bind(&scope.pattern_id)
                .fetch_optional(&mut *connection)
                .await
        }
    }
    .map_err(|error| GraphDocumentError::storage(format!("authorize graph scope: {error}")))?;
    found.map(|_| ()).ok_or_else(|| GraphDocumentError::Scope {
        message: format!(
            "pattern {} does not belong to the current principal",
            scope.pattern_id
        ),
    })?;
    ensure_implementation_belongs_to_pattern(
        connection,
        &scope.pattern_id,
        &scope.implementation_id,
    )
    .await
}

async fn load_document(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<GraphDocument, GraphDocumentError> {
    let row = sqlx::query_as::<_, ImplementationRow>(
        "SELECT id, name, graph_json FROM implementations WHERE id = ? AND pattern_id = ?",
    )
    .bind(implementation_id)
    .bind(pattern_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| GraphDocumentError::storage(format!("load graph implementation: {error}")))?
    .ok_or_else(|| GraphDocumentError::Scope {
        message: format!(
            "implementation {implementation_id} does not belong to pattern {pattern_id}"
        ),
    })?;
    let graph = serde_json::from_str(&row.graph_json).map_err(|error| {
        GraphDocumentError::invalid(
            "graph_json",
            format!("stored pattern graph is corrupt: {error}"),
        )
    })?;
    let graph = canonicalize_graph(&graph)?;
    let revision = graph_revision(&graph)?;
    Ok(GraphDocument {
        implementation_id: row.id,
        revision,
        graph,
    })
}

async fn load_pattern_interface(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<Vec<PatternArgDef>, GraphDocumentError> {
    let graph_json = sqlx::query_scalar::<_, String>(
        "SELECT graph_json FROM implementations WHERE id = ? AND pattern_id = ?",
    )
    .bind(implementation_id)
    .bind(pattern_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| GraphDocumentError::storage(format!("load pattern interface: {error}")))?
    .ok_or_else(|| GraphDocumentError::Scope {
        message: format!(
            "implementation {implementation_id} does not belong to pattern {pattern_id}"
        ),
    })?;
    if graph_json.len() > MAX_GRAPH_JSON_BYTES {
        return Err(GraphDocumentError::invalid(
            "graph_json",
            format!("graph exceeds {MAX_GRAPH_JSON_BYTES} encoded bytes"),
        ));
    }
    let interface: PatternInterface = serde_json::from_str(&graph_json).map_err(|error| {
        GraphDocumentError::invalid(
            "graph_json",
            format!("stored pattern interface is corrupt: {error}"),
        )
    })?;
    canonicalize_pattern_args(&interface.args)
}

async fn implementation_rows(
    connection: &mut SqliteConnection,
    pattern_id: &str,
) -> Result<Vec<ImplementationRow>, GraphDocumentError> {
    sqlx::query_as::<_, ImplementationRow>(
        "SELECT id, name, graph_json FROM implementations WHERE pattern_id = ? ORDER BY created_at, id",
    )
    .bind(pattern_id)
    .fetch_all(connection)
    .await
    .map_err(|error| GraphDocumentError::storage(format!("load pattern graph: {error}")))
}

async fn ensure_implementation_belongs_to_pattern(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<(), GraphDocumentError> {
    let found = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM implementations WHERE id = ? AND pattern_id = ?",
    )
    .bind(implementation_id)
    .bind(pattern_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| {
        GraphDocumentError::storage(format!("resolve graph implementation: {error}"))
    })?;
    found.map(|_| ()).ok_or_else(|| GraphDocumentError::Scope {
        message: format!(
            "implementation {implementation_id} does not belong to pattern {pattern_id}"
        ),
    })
}

async fn resolve_default_implementation(
    connection: &mut SqliteConnection,
    pattern_id: &str,
) -> Result<String, GraphDocumentError> {
    let rows = implementation_rows(connection, pattern_id).await?;
    match rows.as_slice() {
        [] => Err(GraphDocumentError::Scope {
            message: format!("pattern {pattern_id} has no graph implementation"),
        }),
        [row] => Ok(row.id.clone()),
        rows => {
            let defaults: Vec<&ImplementationRow> =
                rows.iter().filter(|row| row.name.is_none()).collect();
            match defaults.as_slice() {
                [row] => Ok(row.id.clone()),
                [] => Err(GraphDocumentError::Scope {
                    message: format!(
                        "pattern {pattern_id} has multiple implementations and no unnamed default; select an implementation explicitly"
                    ),
                }),
                _ => Err(GraphDocumentError::Scope {
                    message: format!(
                        "pattern {pattern_id} has multiple unnamed default implementations; select an implementation explicitly"
                    ),
                }),
            }
        }
    }
}

pub(crate) fn exact_graph_json(graph: &Graph) -> Result<String, GraphDocumentError> {
    let value = serde_json::to_value(graph)
        .map_err(|error| GraphDocumentError::storage(format!("serialize graph: {error}")))?;
    Ok(crate::canonical_json::to_string(&value))
}

fn validate_params(
    issues: &mut Vec<GraphValidationIssue>,
    node_path: &str,
    node: &NodeInstance,
    definition: &NodeTypeDef,
) {
    let params: HashMap<&str, _> = definition
        .params
        .iter()
        .map(|param| (param.id.as_str(), param))
        .collect();
    for (id, value) in &node.params {
        let path = format!("{node_path}.params.{id}");
        let Some(definition) = params.get(id.as_str()) else {
            issue(issues, path, "unknown parameter");
            continue;
        };
        match &definition.param_type {
            ParamType::Number if value.as_f64().is_some_and(f64::is_finite) => {}
            ParamType::Text if value.is_string() => {}
            // A closed set is checked by membership, not just by JSON shape —
            // that is what keeps an uncompilable option out of a saved graph.
            ParamType::Enum { options } => {
                let chosen = value.as_str();
                if !options
                    .iter()
                    .any(|option| Some(option.id.as_str()) == chosen)
                {
                    let names: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
                    issue(
                        issues,
                        path,
                        format!("expected one of {}", names.join(", ")),
                    );
                }
            }
            other => issue(issues, path, format!("expected {other:?} value")),
        }
    }
}

fn validate_arg_default(issues: &mut Vec<GraphValidationIssue>, path: &str, arg: &PatternArgDef) {
    let value = &arg.default_value;
    let valid = match arg.arg_type {
        PatternArgType::Scalar => value.as_f64().is_some_and(f64::is_finite),
        PatternArgType::Color => {
            let object = value.as_object();
            object.is_some_and(|object| {
                ["r", "g", "b", "a"].iter().all(|key| {
                    object
                        .get(*key)
                        .and_then(Value::as_f64)
                        .is_some_and(f64::is_finite)
                })
            })
        }
        PatternArgType::Selection => value.as_object().is_some_and(|object| {
            object.get("expression").and_then(Value::as_str).is_some()
                && object
                    .get("spatialReference")
                    .and_then(Value::as_str)
                    .is_some()
        }),
        PatternArgType::Palette => value.get("colors").and_then(Value::as_array).is_some(),
        PatternArgType::Gradient => value.get("stops").and_then(Value::as_array).is_some(),
    };
    if !valid {
        issue(
            issues,
            format!("{path}.defaultValue"),
            format!("invalid {:?} default", arg.arg_type),
        );
    }
}

fn validate_pattern_args<'a>(
    pattern_args: &'a [PatternArgDef],
    issues: &mut Vec<GraphValidationIssue>,
) -> HashMap<&'a str, &'a PatternArgDef> {
    if pattern_args.len() > MAX_ARGS {
        issue(
            issues,
            "args",
            format!("at most {MAX_ARGS} arguments are allowed"),
        );
    }

    let mut args = HashMap::new();
    for (index, arg) in pattern_args.iter().enumerate() {
        let path = format!("args[{index}]");
        if !valid_identifier(&arg.id) {
            issue(
                issues,
                format!("{path}.id"),
                "argument id must be snake_case",
            );
        }
        if !valid_identifier(&arg.name) {
            issue(
                issues,
                format!("{path}.name"),
                "argument name must be snake_case",
            );
        }
        if args.insert(arg.id.as_str(), arg).is_some() {
            issue(
                issues,
                format!("{path}.id"),
                format!("duplicate argument id {}", arg.id),
            );
        }
        validate_arg_default(issues, &path, arg);
    }
    args
}

fn output_port_type(
    node: &NodeInstance,
    port: &str,
    definitions: &HashMap<String, NodeTypeDef>,
    args: &HashMap<&str, &PatternArgDef>,
) -> Option<PortType> {
    if node.id == PATTERN_ARGS_NODE_ID && node.type_id == PATTERN_ARGS_NODE_ID {
        return args.get(port).map(|arg| match arg.arg_type {
            PatternArgType::Selection => PortType::Selection,
            PatternArgType::Palette | PatternArgType::Gradient => PortType::Stops,
            PatternArgType::Color | PatternArgType::Scalar => PortType::Signal,
        });
    }
    definitions
        .get(&node.type_id)?
        .outputs
        .iter()
        .find(|definition| definition.id == port)
        .map(|definition| definition.port_type.clone())
}

fn input_port_type(
    node: &NodeInstance,
    port: &str,
    definitions: &HashMap<String, NodeTypeDef>,
) -> Option<PortType> {
    definitions
        .get(&node.type_id)?
        .inputs
        .iter()
        .find(|definition| definition.id == port)
        .map(|definition| definition.port_type.clone())
}

fn validate_dag(graph: &Graph, issues: &mut Vec<GraphValidationIssue>) {
    let node_ids: HashSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    let mut incoming: HashMap<&str, usize> = node_ids.iter().map(|id| (*id, 0)).collect();
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if node_ids.contains(edge.from_node.as_str()) && node_ids.contains(edge.to_node.as_str()) {
            *incoming.entry(edge.to_node.as_str()).or_default() += 1;
            outgoing
                .entry(edge.from_node.as_str())
                .or_default()
                .push(edge.to_node.as_str());
        }
    }
    let mut ready: BTreeSet<&str> = incoming
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut visited = 0;
    while let Some(id) = ready.pop_first() {
        visited += 1;
        for target in outgoing.get(id).into_iter().flatten() {
            if let Some(count) = incoming.get_mut(target) {
                *count -= 1;
                if *count == 0 {
                    ready.insert(target);
                }
            }
        }
    }
    if visited != node_ids.len() {
        issue(issues, "edges", "graph contains a cycle");
    }
}

fn canonical_edge_id(edge: &Edge) -> String {
    format!(
        "{}:{}->{}:{}",
        edge.from_node, edge.from_port, edge.to_node, edge.to_port
    )
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('_')
        && !value.ends_with('_')
        && !value.contains("__")
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() && index > 0 || byte == b'_'
        })
}

fn issue(
    issues: &mut Vec<GraphValidationIssue>,
    path: impl Into<String>,
    message: impl Into<String>,
) {
    issues.push(GraphValidationIssue {
        path: path.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, type_id: &str) -> NodeInstance {
        let definition = crate::node_graph::nodes::get_node_types()
            .into_iter()
            .find(|definition| definition.id == type_id)
            .unwrap();
        let params = definition
            .params
            .into_iter()
            .map(|param| {
                let value = match param.param_type {
                    ParamType::Number => Value::from(param.default_number.unwrap_or(0.0)),
                    ParamType::Text | ParamType::Enum { .. } => {
                        Value::from(param.default_text.unwrap_or_default())
                    }
                };
                (param.id, value)
            })
            .collect();
        NodeInstance {
            id: id.to_string(),
            type_id: type_id.to_string(),
            params,
            position_x: Some(0.0),
            position_y: Some(0.0),
        }
    }

    fn edge(from: &str, from_port: &str, to: &str, to_port: &str) -> Edge {
        Edge {
            id: format!("{from}:{from_port}->{to}:{to_port}"),
            from_node: from.to_string(),
            from_port: from_port.to_string(),
            to_node: to.to_string(),
            to_port: to_port.to_string(),
        }
    }

    fn valid_graph() -> Graph {
        Graph {
            nodes: vec![node("source", "scalar"), node("view", "view_signal")],
            edges: vec![edge("source", "out", "view", "in")],
            args: Vec::new(),
        }
    }

    fn catalog_unknown_graph(value: f64) -> Graph {
        Graph {
            nodes: vec![NodeInstance {
                id: "legacy".into(),
                type_id: "retired_node_type".into(),
                params: HashMap::from([("amount".into(), Value::from(value))]),
                position_x: Some(12.0),
                position_y: Some(34.0),
            }],
            edges: Vec::new(),
            args: Vec::new(),
        }
    }

    fn file_round_trip(graph: &Graph) -> Graph {
        graph_from_files(
            &semantic_graph_json(graph).unwrap(),
            &graph_layout_json(graph).unwrap(),
        )
        .unwrap()
    }

    fn invalid_path(error: GraphDocumentError) -> String {
        match error {
            GraphDocumentError::Invalid { issues } => issues[0].path.clone(),
            other => panic!("expected invalid graph file, got {other:?}"),
        }
    }

    #[test]
    fn canonical_files_round_trip_and_split_layout() {
        let graph = valid_graph();
        let semantic = semantic_graph_json(&graph).unwrap();
        let layout = graph_layout_json(&graph).unwrap();
        assert!(!semantic.contains("positionX"));
        assert!(layout.contains("source"));
        assert_eq!(
            serde_json::from_str::<Value>(&semantic).unwrap()["schemaVersion"],
            GRAPH_FILE_SCHEMA_VERSION
        );
        assert_eq!(
            serde_json::from_str::<Value>(&layout).unwrap()["schemaVersion"],
            GRAPH_FILE_SCHEMA_VERSION
        );
        let decoded = graph_from_files(&semantic, &layout).unwrap();
        assert_eq!(
            graph_revision(&decoded).unwrap(),
            graph_revision(&graph).unwrap()
        );
    }

    #[test]
    fn historical_file_decode_and_typed_merge_do_not_use_the_live_catalog() {
        let base = file_round_trip(&catalog_unknown_graph(0.0));
        let ours = file_round_trip(&catalog_unknown_graph(1.0));
        let theirs = file_round_trip(&catalog_unknown_graph(0.0));

        assert!(validate_graph_structure(&base).is_ok());
        assert!(validate_graph(&base).unwrap_err().iter().any(|issue| {
            issue.path == "nodes[0].typeId" && issue.message.contains("unknown node type")
        }));
        let merged = crate::services::authored_merge::merge_graphs(&base, &ours, &theirs)
            .into_result()
            .unwrap();
        assert_eq!(merged.nodes[0].params["amount"].as_f64(), Some(1.0));
        assert_eq!(merged.nodes[0].type_id, "retired_node_type");
    }

    #[test]
    fn versioned_codec_rejects_unknown_nested_fields_with_exact_paths() {
        let semantic =
            serde_json::from_str::<Value>(&semantic_graph_json(&valid_graph()).unwrap()).unwrap();
        let layout =
            serde_json::from_str::<Value>(&graph_layout_json(&valid_graph()).unwrap()).unwrap();
        let mut cases = Vec::new();

        let mut value = semantic.clone();
        value["ndoes"] = serde_json::json!([]);
        cases.push((value, layout.clone(), "graph.json.ndoes"));

        let mut value = semantic.clone();
        value["nodes"][0]["typeID"] = Value::String("scalar".into());
        cases.push((value, layout.clone(), "graph.json.nodes[0].typeID"));

        let mut value = semantic.clone();
        value["edges"][0]["toPrt"] = Value::String("in".into());
        cases.push((value, layout.clone(), "graph.json.edges[0].toPrt"));

        let mut value = semantic.clone();
        value["args"] = serde_json::json!([{
            "id": "amount",
            "name": "amount",
            "argType": "Scalar",
            "defaultValue": 0.5,
            "defaultVale": 0.5
        }]);
        cases.push((value, layout.clone(), "graph.json.args[0].defaultVale"));

        let mut value = layout.clone();
        value["nodez"] = serde_json::json!({});
        cases.push((semantic.clone(), value, "layout.json.nodez"));

        let mut value = layout.clone();
        value["nodes"]["source"]["postionX"] = Value::from(0.0);
        cases.push((semantic.clone(), value, "layout.json.nodes.source.postionX"));

        for (semantic, layout, expected_path) in cases {
            let error = graph_from_files(
                &serde_json::to_string(&semantic).unwrap(),
                &serde_json::to_string(&layout).unwrap(),
            )
            .unwrap_err();
            assert_eq!(invalid_path(error), expected_path);
        }
    }

    #[test]
    fn versioned_codec_rejects_unknown_versions_and_unversioned_files() {
        let mut semantic =
            serde_json::from_str::<Value>(&semantic_graph_json(&valid_graph()).unwrap()).unwrap();
        let mut layout =
            serde_json::from_str::<Value>(&graph_layout_json(&valid_graph()).unwrap()).unwrap();
        semantic["schemaVersion"] = Value::from(99);
        layout["schemaVersion"] = Value::from(99);
        let error = graph_from_files(
            &serde_json::to_string(&semantic).unwrap(),
            &serde_json::to_string(&layout).unwrap(),
        )
        .unwrap_err();
        assert_eq!(invalid_path(error), "graph.json.schemaVersion");

        semantic.as_object_mut().unwrap().remove("schemaVersion");
        let error = graph_from_files(
            &serde_json::to_string(&semantic).unwrap(),
            &graph_layout_json(&valid_graph()).unwrap(),
        )
        .unwrap_err();
        assert_eq!(invalid_path(error), "graph.json.schemaVersion");
    }

    #[test]
    fn rejects_duplicate_input_sources_and_cycles() {
        let mut graph = valid_graph();
        graph.nodes.push(node("other", "scalar"));
        graph.edges.push(edge("other", "out", "view", "in"));
        let issues = validate_graph(&graph).unwrap_err();
        assert!(issues
            .iter()
            .any(|issue| issue.message.contains("more than one source")));

        let mut cycle = Graph {
            nodes: vec![node("a", "math"), node("b", "math")],
            edges: vec![edge("a", "out", "b", "a"), edge("b", "out", "a", "a")],
            args: Vec::new(),
        };
        assert!(validate_graph(&cycle)
            .unwrap_err()
            .iter()
            .any(|issue| issue.message.contains("cycle")));
        cycle.edges.pop();
        assert!(validate_graph(&cycle).is_ok());
    }

    #[test]
    fn validates_pattern_args_as_typed_synthetic_outputs() {
        let argument = PatternArgDef {
            id: "amount".to_string(),
            name: "amount".to_string(),
            arg_type: PatternArgType::Scalar,
            default_value: Value::from(0.5),
        };
        let args_node = NodeInstance {
            id: PATTERN_ARGS_NODE_ID.to_string(),
            type_id: PATTERN_ARGS_NODE_ID.to_string(),
            params: HashMap::new(),
            position_x: None,
            position_y: None,
        };
        let graph = Graph {
            nodes: vec![args_node, node("view", "view_signal")],
            edges: vec![edge(PATTERN_ARGS_NODE_ID, "amount", "view", "in")],
            args: vec![argument],
        };
        assert!(validate_graph(&graph).is_ok());
    }

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE patterns (id TEXT PRIMARY KEY, uid TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE implementations (\
               id TEXT PRIMARY KEY, uid TEXT, pattern_id TEXT NOT NULL,\
               name TEXT, graph_json TEXT NOT NULL,\
               created_at TEXT DEFAULT CURRENT_TIMESTAMP,\
               updated_at TEXT DEFAULT CURRENT_TIMESTAMP\
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE venue_implementation_overrides (\
               venue_id TEXT NOT NULL, pattern_id TEXT NOT NULL,\
               implementation_id TEXT NOT NULL,\
               PRIMARY KEY (venue_id, pattern_id)\
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn apply_is_revision_checked_and_owner_scoped() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO patterns (id, uid) VALUES ('pattern', 'alice')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json)
             VALUES ('implementation', 'pattern', ?)",
        )
        .bind(
            exact_graph_json(&Graph {
                nodes: Vec::new(),
                edges: Vec::new(),
                args: Vec::new(),
            })
            .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap();
        let scope = GraphScope {
            pattern_id: "pattern".to_string(),
            implementation_id: "implementation".to_string(),
            owner_user_id: Some("alice".to_string()),
        };
        let empty = load_graph_document(&pool, &scope).await.unwrap();
        assert!(load_graph_document(
            &pool,
            &GraphScope {
                owner_user_id: Some("bob".to_string()),
                ..scope.clone()
            }
        )
        .await
        .is_err());

        let applied = apply_graph_edit(
            &pool,
            &scope,
            GraphEditPlan {
                base_revision: empty.revision.clone(),
                candidate: valid_graph(),
            },
        )
        .await
        .unwrap();
        assert!(applied.changed);
        let conflict = apply_graph_edit(
            &pool,
            &scope,
            GraphEditPlan {
                base_revision: empty.revision,
                candidate: Graph {
                    nodes: Vec::new(),
                    edges: Vec::new(),
                    args: Vec::new(),
                },
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(conflict, GraphDocumentError::Conflict { .. }));
    }

    #[tokio::test]
    async fn current_projection_rejects_a_structurally_decodable_unsupported_node() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO patterns (id, uid) VALUES ('pattern', NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json)
             VALUES ('implementation', 'pattern', ?)",
        )
        .bind(
            exact_graph_json(&Graph {
                nodes: Vec::new(),
                edges: Vec::new(),
                args: Vec::new(),
            })
            .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap();
        let scope = GraphScope {
            pattern_id: "pattern".into(),
            implementation_id: "implementation".into(),
            owner_user_id: None,
        };
        let before = load_graph_document(&pool, &scope).await.unwrap();
        let candidate = file_round_trip(&catalog_unknown_graph(1.0));
        let error = apply_graph_edit(
            &pool,
            &scope,
            GraphEditPlan {
                base_revision: before.revision.clone(),
                candidate,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            GraphDocumentError::Invalid { ref issues }
                if issues.iter().any(|issue| issue.path == "nodes[0].typeId")
        ));
        assert_eq!(
            load_graph_document(&pool, &scope).await.unwrap().revision,
            before.revision
        );
    }

    #[tokio::test]
    async fn corrupt_or_ambiguous_storage_fails_closed() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO patterns (id, uid) VALUES ('pattern', NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json) VALUES ('one', 'pattern', 'nope')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let scope = GraphScope {
            pattern_id: "pattern".to_string(),
            implementation_id: "one".to_string(),
            owner_user_id: None,
        };
        assert!(matches!(
            load_graph_document(&pool, &scope).await.unwrap_err(),
            GraphDocumentError::Invalid { .. }
        ));
        sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = 'one'")
            .bind(exact_graph_json(&valid_graph()).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json) VALUES ('two', 'pattern', ?)",
        )
        .bind(exact_graph_json(&valid_graph()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            load_graph_document(&pool, &scope)
                .await
                .unwrap()
                .implementation_id,
            "one"
        );
        assert!(matches!(
            resolve_graph_implementation(&pool, "pattern", None, None)
                .await
                .unwrap_err(),
            GraphDocumentError::Scope { .. }
        ));

        sqlx::query(
            "INSERT INTO venue_implementation_overrides
             (venue_id, pattern_id, implementation_id)
             VALUES ('venue', 'pattern', 'two')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            resolve_graph_implementation(&pool, "pattern", Some("venue"), None)
                .await
                .unwrap(),
            "two"
        );
        assert_eq!(
            resolve_graph_implementation(&pool, "pattern", None, Some("one"))
                .await
                .unwrap(),
            "one"
        );
    }
}
