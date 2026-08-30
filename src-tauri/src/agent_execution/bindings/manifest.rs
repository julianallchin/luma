//! The binding manifest — contract C1 between the Rust host and the Python
//! worker. The serialized shape here is load-bearing: `luma_exec` parses exactly
//! this JSON. Changing a field name or a tag is a protocol break.
//!
//! ```text
//! {
//!   "schema_version": 1,
//!   "revision": "r-<uuid>",
//!   "agent_kind": "track_copilot" | "pattern_graph",
//!   "scope": { "track_id": ..., "venue_id": ..., "score_id": ...,
//!              "pattern_id": ..., "implementation_id": ...,
//!              "window": {"start_s":..,"end_s":..} | null },
//!   "root": <BindingValue>,
//!   "artifacts": { "<artifact_id>": <ArtifactDescriptor> }
//! }
//! ```
//!
//! `BindingValue` is *untagged* for the plain JSON scalars, containers and
//! records; only tensors and unavailable branches carry a `"$kind"` discriminant.
//! That keeps records indistinguishable from ordinary JSON objects on the Python
//! side, which is what makes `luma.features.beats` read like an attribute tree.

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::agent_execution::artifacts::ArtifactDescriptor;
use crate::agent_execution::error::{err, Result};

pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Opaque, immutable identity of one assembled binding revision (`r-<uuid>`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingRevision(String);

impl BindingRevision {
    /// Mint a fresh revision id.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(format!("r-{}", uuid::Uuid::new_v4()))
    }

    /// Adopt an existing id, validating the `r-` prefix.
    pub fn parse(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        if !s.starts_with("r-") || s.len() <= 2 {
            return err(format!("invalid binding revision id: {s}"));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BindingRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque identity of one artifact within a workspace's artifact store. On the
/// wire it is only ever a key in the `artifacts` map, so `ArtifactDescriptor`
/// skips it and [`BindingManifest::from_json`] restores it from the key.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// Mint a fresh artifact id.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(format!("a-{}", uuid::Uuid::new_v4()))
    }

    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    TrackCopilot,
    PatternGraph,
    /// The room builder. Its namespace is `luma.venue` alone.
    VenueRig,
}

/// The window of interest, in absolute track seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnalysisWindow {
    pub start_s: f64,
    pub end_s: f64,
}

/// What the agent is looking at. Every field is emitted, `null` when absent —
/// the loader reads it as a fixed-shape record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalysisScope {
    pub track_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    pub pattern_id: Option<String>,
    pub implementation_id: Option<String>,
    pub window: Option<AnalysisWindow>,
}

// ---------------------------------------------------------------------------
// Tensors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DType {
    F32,
    F64,
    F16,
    I64,
}

impl DType {
    pub fn size_bytes(self) -> u64 {
        match self {
            DType::F32 => 4,
            DType::F64 => 8,
            DType::F16 => 2,
            DType::I64 => 8,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DType::F32 => "f32",
            DType::F64 => "f64",
            DType::F16 => "f16",
            DType::I64 => "i64",
        }
    }

    /// Little-endian NumPy descr string for this dtype.
    pub fn npy_descr(self) -> &'static str {
        match self {
            DType::F32 => "<f4",
            DType::F64 => "<f8",
            DType::F16 => "<f2",
            DType::I64 => "<i8",
        }
    }

    pub fn from_npy_descr(descr: &str) -> Result<Self> {
        match descr {
            "<f4" | "=f4" | "|f4" | "f4" => Ok(DType::F32),
            "<f8" | "=f8" | "|f8" | "f8" => Ok(DType::F64),
            "<f2" | "=f2" | "|f2" | "f2" => Ok(DType::F16),
            "<i8" | "=i8" | "|i8" | "i8" => Ok(DType::I64),
            other => err(format!("unsupported npy dtype descr: {other}")),
        }
    }
}

/// Where a tensor's numbers came from. Preserved verbatim into the manifest so
/// the agent can reason about processor versions rather than guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processor_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Provenance {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            processor_version: None,
            note: None,
        }
    }

    pub fn with_version(mut self, v: impl Into<String>) -> Self {
        self.processor_version = Some(v.into());
        self
    }

    pub fn with_note(mut self, n: impl Into<String>) -> Self {
        self.note = Some(n.into());
        self
    }
}

/// A pointer into an artifact plus the semantics needed to materialize it as a
/// `LumaTensor`. Serializes *without* `$kind`; `BindingValue::Tensor` adds the
/// tag, and an axis's coordinate tensor is emitted bare (contract C1).
#[derive(Debug, Clone, PartialEq)]
pub struct TensorRef {
    pub artifact_id: ArtifactId,
    pub dtype: DType,
    pub shape: Vec<usize>,
    pub byte_offset: u64,
    pub axes: Vec<AxisSpec>,
    pub unit: Option<String>,
    pub provenance: Provenance,
}

impl TensorRef {
    pub fn new(
        artifact_id: ArtifactId,
        dtype: DType,
        shape: Vec<usize>,
        axes: Vec<AxisSpec>,
        provenance: Provenance,
    ) -> Self {
        Self {
            artifact_id,
            dtype,
            shape,
            byte_offset: 0,
            axes,
            unit: None,
            provenance,
        }
    }

    pub fn with_offset(mut self, byte_offset: u64) -> Self {
        self.byte_offset = byte_offset;
        self
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Number of elements implied by the shape.
    pub fn element_count(&self) -> u64 {
        self.shape.iter().fold(1u64, |acc, d| acc * (*d as u64))
    }

    /// Bytes the tensor occupies in its artifact.
    pub fn byte_size(&self) -> u64 {
        self.element_count() * self.dtype.size_bytes()
    }

    fn write_fields<M: SerializeMap>(&self, map: &mut M) -> std::result::Result<(), M::Error> {
        map.serialize_entry("artifact_id", &self.artifact_id)?;
        map.serialize_entry("dtype", &self.dtype)?;
        map.serialize_entry("shape", &self.shape)?;
        map.serialize_entry("byte_offset", &self.byte_offset)?;
        map.serialize_entry("axes", &self.axes)?;
        map.serialize_entry("unit", &self.unit)?;
        map.serialize_entry("provenance", &self.provenance)?;
        Ok(())
    }

    fn from_json(v: &serde_json::Value) -> std::result::Result<Self, String> {
        let obj = v.as_object().ok_or("tensor ref must be an object")?;
        let artifact_id = ArtifactId::from_string(
            obj.get("artifact_id")
                .and_then(|v| v.as_str())
                .ok_or("tensor ref missing artifact_id")?,
        );
        let dtype: DType = serde_json::from_value(
            obj.get("dtype")
                .cloned()
                .ok_or("tensor ref missing dtype")?,
        )
        .map_err(|e| e.to_string())?;
        let shape: Vec<usize> = serde_json::from_value(
            obj.get("shape")
                .cloned()
                .ok_or("tensor ref missing shape")?,
        )
        .map_err(|e| e.to_string())?;
        let byte_offset = obj.get("byte_offset").and_then(|v| v.as_u64()).unwrap_or(0);
        let axes = match obj.get("axes") {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .map(AxisSpec::from_json)
                .collect::<std::result::Result<Vec<_>, _>>()?,
            _ => return Err("tensor ref missing axes".into()),
        };
        let unit = obj
            .get("unit")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let provenance: Provenance = serde_json::from_value(
            obj.get("provenance")
                .cloned()
                .ok_or("tensor ref missing provenance")?,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            artifact_id,
            dtype,
            shape,
            byte_offset,
            axes,
            unit,
            provenance,
        })
    }
}

impl Serialize for TensorRef {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(7))?;
        self.write_fields(&mut map)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for TensorRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        TensorRef::from_json(&v).map_err(D::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Axes
// ---------------------------------------------------------------------------

/// Coordinate values for a `Coordinates` axis: inline for small axes, an
/// artifact-backed tensor for large ones.
#[derive(Debug, Clone, PartialEq)]
pub enum Coordinates {
    Inline(Vec<f64>),
    Tensor(Box<TensorRef>),
}

/// The semantic meaning of one tensor dimension.
#[derive(Debug, Clone, PartialEq)]
pub enum AxisSpec {
    Linear {
        name: String,
        start: f64,
        step: f64,
        count: usize,
        unit: Option<String>,
    },
    Coordinates {
        name: String,
        values: Coordinates,
        unit: Option<String>,
    },
    Labels {
        name: String,
        labels: Vec<String>,
    },
    Index {
        name: String,
        count: usize,
    },
}

impl AxisSpec {
    pub fn linear(name: impl Into<String>, start: f64, step: f64, count: usize) -> Self {
        AxisSpec::Linear {
            name: name.into(),
            start,
            step,
            count,
            unit: None,
        }
    }

    pub fn linear_unit(
        name: impl Into<String>,
        start: f64,
        step: f64,
        count: usize,
        unit: impl Into<String>,
    ) -> Self {
        AxisSpec::Linear {
            name: name.into(),
            start,
            step,
            count,
            unit: Some(unit.into()),
        }
    }

    pub fn coordinates(name: impl Into<String>, values: Vec<f64>, unit: Option<String>) -> Self {
        AxisSpec::Coordinates {
            name: name.into(),
            values: Coordinates::Inline(values),
            unit,
        }
    }

    pub fn coordinate_tensor(
        name: impl Into<String>,
        tensor: TensorRef,
        unit: Option<String>,
    ) -> Self {
        AxisSpec::Coordinates {
            name: name.into(),
            values: Coordinates::Tensor(Box::new(tensor)),
            unit,
        }
    }

    pub fn labels(name: impl Into<String>, labels: Vec<String>) -> Self {
        AxisSpec::Labels {
            name: name.into(),
            labels,
        }
    }

    pub fn index(name: impl Into<String>, count: usize) -> Self {
        AxisSpec::Index {
            name: name.into(),
            count,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            AxisSpec::Linear { name, .. }
            | AxisSpec::Coordinates { name, .. }
            | AxisSpec::Labels { name, .. }
            | AxisSpec::Index { name, .. } => name,
        }
    }

    /// The dimension length this axis describes, when it is knowable without
    /// resolving an artifact. `Coordinates { Tensor }` reports its tensor's
    /// leading dimension.
    pub fn len(&self) -> Option<usize> {
        match self {
            AxisSpec::Linear { count, .. } | AxisSpec::Index { count, .. } => Some(*count),
            AxisSpec::Labels { labels, .. } => Some(labels.len()),
            AxisSpec::Coordinates { values, .. } => match values {
                Coordinates::Inline(v) => Some(v.len()),
                Coordinates::Tensor(t) => t.shape.first().copied(),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    fn from_json(v: &serde_json::Value) -> std::result::Result<Self, String> {
        let obj = v.as_object().ok_or("axis must be an object")?;
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or("axis missing kind")?;
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("axis missing name")?
            .to_string();
        let unit = obj
            .get("unit")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        match kind {
            "linear" => Ok(AxisSpec::Linear {
                name,
                start: obj
                    .get("start")
                    .and_then(|v| v.as_f64())
                    .ok_or("linear axis missing start")?,
                step: obj
                    .get("step")
                    .and_then(|v| v.as_f64())
                    .ok_or("linear axis missing step")?,
                count: obj
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .ok_or("linear axis missing count")? as usize,
                unit,
            }),
            "coordinates" => {
                let values = if let Some(t) = obj.get("tensor") {
                    Coordinates::Tensor(Box::new(TensorRef::from_json(t)?))
                } else {
                    let arr = obj
                        .get("values")
                        .and_then(|v| v.as_array())
                        .ok_or("coordinates axis missing values or tensor")?;
                    Coordinates::Inline(
                        arr.iter()
                            .map(|v| v.as_f64().ok_or("coordinate value must be a number"))
                            .collect::<std::result::Result<Vec<_>, _>>()?,
                    )
                };
                Ok(AxisSpec::Coordinates { name, values, unit })
            }
            "labels" => {
                let arr = obj
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .ok_or("labels axis missing labels")?;
                Ok(AxisSpec::Labels {
                    name,
                    labels: arr
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(|s| s.to_string())
                                .ok_or("label must be a string")
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?,
                })
            }
            "index" => Ok(AxisSpec::Index {
                name,
                count: obj
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .ok_or("index axis missing count")? as usize,
            }),
            other => Err(format!("unknown axis kind: {other}")),
        }
    }
}

impl Serialize for AxisSpec {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            AxisSpec::Linear {
                name,
                start,
                step,
                count,
                unit,
            } => {
                let mut m = s.serialize_map(Some(6))?;
                m.serialize_entry("kind", "linear")?;
                m.serialize_entry("name", name)?;
                m.serialize_entry("start", start)?;
                m.serialize_entry("step", step)?;
                m.serialize_entry("count", count)?;
                m.serialize_entry("unit", unit)?;
                m.end()
            }
            AxisSpec::Coordinates { name, values, unit } => {
                let mut m = s.serialize_map(Some(4))?;
                m.serialize_entry("kind", "coordinates")?;
                m.serialize_entry("name", name)?;
                match values {
                    Coordinates::Inline(v) => m.serialize_entry("values", v)?,
                    Coordinates::Tensor(t) => m.serialize_entry("tensor", t)?,
                }
                m.serialize_entry("unit", unit)?;
                m.end()
            }
            AxisSpec::Labels { name, labels } => {
                let mut m = s.serialize_map(Some(3))?;
                m.serialize_entry("kind", "labels")?;
                m.serialize_entry("name", name)?;
                m.serialize_entry("labels", labels)?;
                m.end()
            }
            AxisSpec::Index { name, count } => {
                let mut m = s.serialize_map(Some(3))?;
                m.serialize_entry("kind", "index")?;
                m.serialize_entry("name", name)?;
                m.serialize_entry("count", count)?;
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for AxisSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        AxisSpec::from_json(&v).map_err(D::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// One node of the `luma` namespace tree.
#[derive(Debug, Clone, PartialEq)]
pub enum BindingValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    List(Vec<BindingValue>),
    Record(BTreeMap<String, BindingValue>),
    Tensor(Box<TensorRef>),
    /// A branch that exists in the schema but has no data — distinct from an
    /// empty tensor or an empty record (design §9.7).
    Unavailable {
        reason: String,
        provenance: Option<Provenance>,
    },
}

impl BindingValue {
    pub fn record() -> Self {
        BindingValue::Record(BTreeMap::new())
    }

    pub fn tensor(t: TensorRef) -> Self {
        BindingValue::Tensor(Box::new(t))
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        BindingValue::Unavailable {
            reason: reason.into(),
            provenance: None,
        }
    }

    pub fn is_record(&self) -> bool {
        matches!(self, BindingValue::Record(_))
    }

    pub fn as_record_mut(&mut self) -> Option<&mut BTreeMap<String, BindingValue>> {
        match self {
            BindingValue::Record(m) => Some(m),
            _ => None,
        }
    }

    /// Convert an arbitrary serde_json value into a binding value. Objects
    /// become records; a `"$kind"` key is rejected because it would collide with
    /// the tensor/unavailable tags.
    pub fn from_json(v: serde_json::Value) -> Result<Self> {
        match v {
            serde_json::Value::Null => Ok(BindingValue::Null),
            serde_json::Value::Bool(b) => Ok(BindingValue::Bool(b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(BindingValue::I64(i))
                } else if let Some(f) = n.as_f64() {
                    Ok(BindingValue::F64(f))
                } else {
                    err(format!("unrepresentable number: {n}"))
                }
            }
            serde_json::Value::String(s) => Ok(BindingValue::String(s)),
            serde_json::Value::Array(items) => Ok(BindingValue::List(
                items
                    .into_iter()
                    .map(BindingValue::from_json)
                    .collect::<Result<Vec<_>>>()?,
            )),
            serde_json::Value::Object(obj) => {
                if obj.contains_key("$kind") {
                    return Self::tagged_from_json(&serde_json::Value::Object(obj));
                }
                let mut map = BTreeMap::new();
                for (k, v) in obj {
                    map.insert(k, BindingValue::from_json(v)?);
                }
                Ok(BindingValue::Record(map))
            }
        }
    }

    /// Serialize any `Serialize` value into a binding value (the `inline` path).
    pub fn from_serializable<T: Serialize>(value: &T) -> Result<Self> {
        BindingValue::from_json(serde_json::to_value(value)?)
    }

    fn tagged_from_json(v: &serde_json::Value) -> Result<Self> {
        let obj = v.as_object().expect("checked object");
        match obj.get("$kind").and_then(|k| k.as_str()) {
            Some("tensor") => Ok(BindingValue::Tensor(Box::new(
                TensorRef::from_json(v).map_err(crate::agent_execution::error::DataPlaneError)?,
            ))),
            Some("unavailable") => Ok(BindingValue::Unavailable {
                reason: obj
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or_default()
                    .to_string(),
                provenance: match obj.get("provenance") {
                    Some(serde_json::Value::Null) | None => None,
                    Some(p) => Some(serde_json::from_value(p.clone())?),
                },
            }),
            other => err(format!("unknown $kind: {other:?}")),
        }
    }

    /// Depth-first walk over every tensor reference in the tree, including the
    /// coordinate tensors hidden inside axes.
    pub fn visit_tensors<'a>(&'a self, out: &mut Vec<(String, &'a TensorRef)>) {
        fn walk<'a>(path: &str, v: &'a BindingValue, out: &mut Vec<(String, &'a TensorRef)>) {
            match v {
                BindingValue::Tensor(t) => push_tensor(path, t, out),
                BindingValue::Record(map) => {
                    for (k, child) in map {
                        let child_path = if path.is_empty() {
                            k.clone()
                        } else {
                            format!("{path}.{k}")
                        };
                        walk(&child_path, child, out);
                    }
                }
                BindingValue::List(items) => {
                    for (i, child) in items.iter().enumerate() {
                        walk(&format!("{path}[{i}]"), child, out);
                    }
                }
                _ => {}
            }
        }
        fn push_tensor<'a>(path: &str, t: &'a TensorRef, out: &mut Vec<(String, &'a TensorRef)>) {
            out.push((path.to_string(), t));
            for axis in &t.axes {
                if let AxisSpec::Coordinates {
                    name,
                    values: Coordinates::Tensor(inner),
                    ..
                } = axis
                {
                    push_tensor(&format!("{path}.axes.{name}"), inner, out);
                }
            }
        }
        walk("", self, out);
    }
}

impl Serialize for BindingValue {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            BindingValue::Null => s.serialize_unit(),
            BindingValue::Bool(b) => s.serialize_bool(*b),
            BindingValue::I64(i) => s.serialize_i64(*i),
            BindingValue::F64(f) => s.serialize_f64(*f),
            BindingValue::String(v) => s.serialize_str(v),
            BindingValue::List(items) => s.collect_seq(items),
            BindingValue::Record(map) => s.collect_map(map),
            BindingValue::Tensor(t) => {
                let mut m = s.serialize_map(Some(8))?;
                m.serialize_entry("$kind", "tensor")?;
                t.write_fields(&mut m)?;
                m.end()
            }
            BindingValue::Unavailable { reason, provenance } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("$kind", "unavailable")?;
                m.serialize_entry("reason", reason)?;
                if let Some(p) = provenance {
                    m.serialize_entry("provenance", p)?;
                }
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for BindingValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        BindingValue::from_json(v).map_err(|e| D::Error::custom(e.0))
    }
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingManifest {
    pub schema_version: u32,
    pub revision: BindingRevision,
    pub agent_kind: AgentKind,
    pub scope: AnalysisScope,
    pub root: BindingValue,
    pub artifacts: BTreeMap<ArtifactId, ArtifactDescriptor>,
}

impl BindingManifest {
    /// Canonical on-disk name inside `<workspace>/inputs/`.
    pub fn file_name(&self) -> String {
        format!("manifest-{}.json", self.revision)
    }

    /// Workspace-relative path, the value handed to the worker as `manifest_rel`.
    pub fn rel_path(&self) -> String {
        format!("inputs/{}", self.file_name())
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        let mut manifest: BindingManifest = serde_json::from_str(s)?;
        // `id` is the map key on the wire; restore it into the descriptors.
        manifest.artifacts = manifest
            .artifacts
            .into_iter()
            .map(|(id, mut d)| {
                d.id = id.clone();
                (id, d)
            })
            .collect();
        if manifest.schema_version != SCHEMA_VERSION {
            return err(format!(
                "unsupported manifest schema_version {} (expected {SCHEMA_VERSION})",
                manifest.schema_version
            ));
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_execution::artifacts::{ArtifactEncoding, ArtifactKind};

    fn descriptor(id: &str, encoding: ArtifactEncoding, byte_len: u64) -> ArtifactDescriptor {
        ArtifactDescriptor {
            id: ArtifactId::from_string(id),
            kind: ArtifactKind::Tensor,
            encoding,
            rel_path: format!("inputs/{id}.bin"),
            byte_len,
            content_hash: None,
            sample_rate_hz: None,
            channels: None,
        }
    }

    #[test]
    fn record_serializes_as_a_plain_object() {
        let mut map = BTreeMap::new();
        map.insert("bpm".to_string(), BindingValue::F64(128.0));
        map.insert("title".to_string(), BindingValue::String("Hex".into()));
        map.insert("bars".to_string(), BindingValue::I64(64));
        map.insert("liked".to_string(), BindingValue::Bool(true));
        map.insert("key".to_string(), BindingValue::Null);
        map.insert(
            "tags".to_string(),
            BindingValue::List(vec![BindingValue::String("techno".into())]),
        );
        let json = serde_json::to_string(&BindingValue::Record(map)).unwrap();
        assert_eq!(
            json,
            r#"{"bars":64,"bpm":128.0,"key":null,"liked":true,"tags":["techno"],"title":"Hex"}"#
        );
    }

    #[test]
    fn tensor_serializes_with_kind_tag() {
        let t = TensorRef::new(
            ArtifactId::from_string("a-1"),
            DType::F32,
            vec![3],
            vec![AxisSpec::index("event", 3)],
            Provenance::new("beat_this"),
        )
        .with_unit("s");
        let json = serde_json::to_string(&BindingValue::tensor(t)).unwrap();
        assert_eq!(
            json,
            r#"{"$kind":"tensor","artifact_id":"a-1","dtype":"f32","shape":[3],"byte_offset":0,"axes":[{"kind":"index","name":"event","count":3}],"unit":"s","provenance":{"source":"beat_this"}}"#
        );
    }

    #[test]
    fn unavailable_serializes_with_reason() {
        let json = serde_json::to_string(&BindingValue::unavailable("stems not computed")).unwrap();
        assert_eq!(
            json,
            r#"{"$kind":"unavailable","reason":"stems not computed"}"#
        );
    }

    #[test]
    fn unavailable_keeps_provenance_when_present() {
        let v = BindingValue::Unavailable {
            reason: "drum-onset preprocessing failed".into(),
            provenance: Some(Provenance::new("adtof").with_version("1.2")),
        };
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"$kind":"unavailable","reason":"drum-onset preprocessing failed","provenance":{"source":"adtof","processor_version":"1.2"}}"#
        );
    }

    #[test]
    fn unavailable_is_not_an_empty_tensor_or_record() {
        let empty_tensor = BindingValue::tensor(TensorRef::new(
            ArtifactId::from_string("a-1"),
            DType::F32,
            vec![0],
            vec![AxisSpec::index("event", 0)],
            Provenance::new("adtof"),
        ));
        let unavailable = BindingValue::unavailable("failed");
        let empty_record = BindingValue::record();
        assert_ne!(empty_tensor, unavailable);
        assert_ne!(empty_record, unavailable);
        let empty_json = serde_json::to_string(&empty_tensor).unwrap();
        assert!(empty_json.contains(r#""$kind":"tensor""#));
        assert!(empty_json.contains(r#""shape":[0]"#));
        assert_eq!(serde_json::to_string(&empty_record).unwrap(), "{}");
    }

    #[test]
    fn axis_variants_serialize_exactly() {
        assert_eq!(
            serde_json::to_string(&AxisSpec::linear_unit("time", 0.0, 2.0833e-5, 4, "s")).unwrap(),
            // serde_json renders small floats positionally; still valid JSON,
            // and the loader only ever sees the parsed number.
            r#"{"kind":"linear","name":"time","start":0.0,"step":0.000020833,"count":4,"unit":"s"}"#
        );
        assert_eq!(
            serde_json::to_string(&AxisSpec::coordinates(
                "bar",
                vec![0.0, 1.5],
                Some("s".into())
            ))
            .unwrap(),
            r#"{"kind":"coordinates","name":"bar","values":[0.0,1.5],"unit":"s"}"#
        );
        assert_eq!(
            serde_json::to_string(&AxisSpec::labels(
                "channel",
                vec!["r".into(), "g".into(), "b".into()]
            ))
            .unwrap(),
            r#"{"kind":"labels","name":"channel","labels":["r","g","b"]}"#
        );
        assert_eq!(
            serde_json::to_string(&AxisSpec::index("event", 12)).unwrap(),
            r#"{"kind":"index","name":"event","count":12}"#
        );
    }

    #[test]
    fn coordinate_tensor_axis_embeds_a_bare_tensor_ref() {
        let inner = TensorRef::new(
            ArtifactId::from_string("a-times"),
            DType::F64,
            vec![2],
            vec![AxisSpec::index("frame", 2)],
            Provenance::new("graph_run"),
        );
        let axis = AxisSpec::coordinate_tensor("time", inner, Some("s".into()));
        let json = serde_json::to_string(&axis).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"coordinates","name":"time","tensor":{"artifact_id":"a-times","dtype":"f64","shape":[2],"byte_offset":0,"axes":[{"kind":"index","name":"frame","count":2}],"unit":null,"provenance":{"source":"graph_run"}},"unit":"s"}"#
        );
        assert!(!json.contains("$kind"));
    }

    #[test]
    fn manifest_serializes_exactly_per_contract_c1() {
        let mut features = BTreeMap::new();
        features.insert(
            "beats".to_string(),
            BindingValue::tensor(
                TensorRef::new(
                    ArtifactId::from_string("a-beats"),
                    DType::F32,
                    vec![2],
                    vec![AxisSpec::index("event", 2)],
                    Provenance::new("beat_this").with_version("0.1.4"),
                )
                .with_unit("s"),
            ),
        );
        features.insert(
            "drum_onsets".to_string(),
            BindingValue::unavailable("drum onset analysis has not run"),
        );
        let mut root = BTreeMap::new();
        root.insert("features".to_string(), BindingValue::Record(features));

        let mut artifacts = BTreeMap::new();
        artifacts.insert(
            ArtifactId::from_string("a-beats"),
            ArtifactDescriptor {
                id: ArtifactId::from_string("a-beats"),
                kind: ArtifactKind::Tensor,
                encoding: ArtifactEncoding::RawLe,
                rel_path: "inputs/a-beats.bin".into(),
                byte_len: 8,
                content_hash: None,
                sample_rate_hz: None,
                channels: None,
            },
        );

        let manifest = BindingManifest {
            schema_version: SCHEMA_VERSION,
            revision: BindingRevision::parse("r-0000").unwrap(),
            agent_kind: AgentKind::TrackCopilot,
            scope: AnalysisScope {
                track_id: Some("t-1".into()),
                venue_id: None,
                score_id: None,
                pattern_id: None,
                implementation_id: None,
                window: Some(AnalysisWindow {
                    start_s: 0.0,
                    end_s: 30.0,
                }),
            },
            root: BindingValue::Record(root),
            artifacts,
        };

        let expected = concat!(
            r#"{"schema_version":1,"revision":"r-0000","agent_kind":"track_copilot","#,
            r#""scope":{"track_id":"t-1","venue_id":null,"score_id":null,"pattern_id":null,"implementation_id":null,"#,
            r#""window":{"start_s":0.0,"end_s":30.0}},"#,
            r#""root":{"features":{"#,
            r#""beats":{"$kind":"tensor","artifact_id":"a-beats","dtype":"f32","shape":[2],"#,
            r#""byte_offset":0,"axes":[{"kind":"index","name":"event","count":2}],"unit":"s","#,
            r#""provenance":{"source":"beat_this","processor_version":"0.1.4"}},"#,
            r#""drum_onsets":{"$kind":"unavailable","reason":"drum onset analysis has not run"}}},"#,
            r#""artifacts":{"a-beats":{"kind":"tensor","encoding":"raw_le","#,
            r#""rel_path":"inputs/a-beats.bin","byte_len":8,"content_hash":null}}}"#
        );
        assert_eq!(manifest.to_json().unwrap(), expected);
        assert_eq!(manifest.rel_path(), "inputs/manifest-r-0000.json");
    }

    #[test]
    fn pcm_descriptor_carries_sample_rate_and_channels() {
        let mut d = descriptor("a-pcm", ArtifactEncoding::PcmF32, 818);
        d.kind = ArtifactKind::Tensor;
        d.sample_rate_hz = Some(48000);
        d.channels = Some(2);
        assert_eq!(
            serde_json::to_string(&d).unwrap(),
            r#"{"kind":"tensor","encoding":"pcm_f32","rel_path":"inputs/a-pcm.bin","byte_len":818,"content_hash":null,"sample_rate_hz":48000,"channels":2}"#
        );
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let mut root = BTreeMap::new();
        root.insert(
            "audio".to_string(),
            BindingValue::tensor(
                TensorRef::new(
                    ArtifactId::from_string("a-mix"),
                    DType::F32,
                    vec![4, 2],
                    vec![
                        AxisSpec::linear_unit("time", 0.0, 1.0 / 48000.0, 4, "s"),
                        AxisSpec::labels("channel", vec!["l".into(), "r".into()]),
                    ],
                    Provenance::new("audio_cache").with_note("mix"),
                )
                .with_offset(18),
            ),
        );
        root.insert("empty".to_string(), BindingValue::record());
        root.insert(
            "missing".to_string(),
            BindingValue::unavailable("no source"),
        );
        root.insert(
            "list".to_string(),
            BindingValue::List(vec![BindingValue::I64(1), BindingValue::F64(2.5)]),
        );

        let mut artifacts = BTreeMap::new();
        artifacts.insert(
            ArtifactId::from_string("a-mix"),
            descriptor("a-mix", ArtifactEncoding::PcmF32, 18 + 32),
        );

        let manifest = BindingManifest {
            schema_version: SCHEMA_VERSION,
            revision: BindingRevision::new(),
            agent_kind: AgentKind::PatternGraph,
            scope: AnalysisScope::default(),
            root: BindingValue::Record(root),
            artifacts,
        };
        let json = manifest.to_json().unwrap();
        let back = BindingManifest::from_json(&json).unwrap();
        assert_eq!(manifest, back);
        assert_eq!(back.to_json().unwrap(), json);
    }

    #[test]
    fn schema_version_is_checked_on_load() {
        let json = r#"{"schema_version":99,"revision":"r-1","agent_kind":"track_copilot","scope":{"track_id":null,"venue_id":null,"score_id":null,"pattern_id":null,"window":null},"root":{},"artifacts":{}}"#;
        let e = BindingManifest::from_json(json).unwrap_err();
        assert!(e.message().contains("schema_version"), "{e}");
    }

    #[test]
    fn revision_ids_are_validated() {
        assert!(BindingRevision::parse("r-abc").is_ok());
        assert!(BindingRevision::parse("abc").is_err());
        assert!(BindingRevision::parse("r-").is_err());
        assert!(BindingRevision::new().as_str().starts_with("r-"));
    }

    #[test]
    fn dollar_kind_in_inline_json_is_rejected() {
        let v = serde_json::json!({ "$kind": "sneaky" });
        assert!(BindingValue::from_json(v).is_err());
    }

    #[test]
    fn visit_tensors_reaches_axis_coordinate_tensors() {
        let inner = TensorRef::new(
            ArtifactId::from_string("a-times"),
            DType::F64,
            vec![2],
            vec![AxisSpec::index("frame", 2)],
            Provenance::new("graph_run"),
        );
        let outer = TensorRef::new(
            ArtifactId::from_string("a-view"),
            DType::F32,
            vec![2],
            vec![AxisSpec::coordinate_tensor("time", inner, Some("s".into()))],
            Provenance::new("graph_run"),
        );
        let mut root = BTreeMap::new();
        root.insert(
            "graph".to_string(),
            BindingValue::Record(BTreeMap::from([(
                "view".to_string(),
                BindingValue::tensor(outer),
            )])),
        );
        let value = BindingValue::Record(root);
        let mut found = Vec::new();
        value.visit_tensors(&mut found);
        let paths: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["graph.view", "graph.view.axes.time"]);
    }
}
