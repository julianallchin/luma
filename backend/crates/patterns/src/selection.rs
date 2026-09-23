//! The Selection arg value: *which* fixtures a clip targets.
//!
//! This is the one place the wire shape is spelled. Every producer and consumer
//! — eval, the score DSL, the legacy upgrade, the graph validator, the strip UI,
//! Python — goes through this type rather than re-spelling the object.
//!
//! ```json
//! { "expression": "front_wash & left" }
//! ```
//!
//! A selection is the group expression alone: there is no positional grammar, so
//! a picker UI never has to parse an expression back out. It always means the
//! whole matched set. A random share of it is the Sparkle form's coverage; a
//! specific set of heads is a group.
//!
//! Keys this type does not name are dropped on read and never written back: a
//! `spatialReference` or a `subset` from an older writer, and the junk of the
//! legacy string-spread bug. A stored `subset` therefore reads as the whole group.

use serde::{Deserialize, Serialize};

/// A Selection arg value.
///
/// Deserializing requires `expression`, which is what distinguishes a Selection
/// value from any other arg's JSON, and drops every other key (see the module
/// doc).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub expression: String,
}

impl Selection {
    pub fn validate(&self) -> crate::Result<()> {
        if self.expression.trim().is_empty() {
            return Err(crate::Error(
                "selection expression must not be empty".into(),
            ));
        }
        Ok(())
    }

    /// The whole venue — what a preview with no venue context uses.
    #[must_use]
    pub fn all() -> Self {
        Self::new("all")
    }

    /// A selection over `expression`.
    #[must_use]
    pub fn new(expression: impl Into<String>) -> Self {
        Self {
            expression: expression.into(),
        }
    }

    /// Read a stored arg value. `None` when the value is not a selection —
    /// no `expression` string.
    #[must_use]
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }

    /// The wire value. Infallible: every field is JSON-representable.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Selection is always representable as JSON")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_expression_round_trips() {
        let value = json!({"expression": "front_wash"});
        let selection = Selection::from_value(&value).unwrap();
        assert_eq!(selection, Selection::new("front_wash"));
        assert_eq!(selection.to_value(), value);
    }

    /// Selections stored while subsets existed still carry `subset`. Every
    /// stored form reads back as the whole group and saves without the key.
    #[test]
    fn a_stored_subset_loads_as_the_whole_group() {
        for subset in [
            json!("all"),
            json!({"fraction": 0.5}),
            json!({"count": 3}),
            json!("half"),
        ] {
            let stored = json!({"expression": "spots", "subset": subset});
            let selection = Selection::from_value(&stored).unwrap();
            assert_eq!(selection, Selection::new("spots"));
            assert_eq!(selection.to_value(), json!({"expression": "spots"}));
        }
    }

    /// Stored selections still carry `spatialReference`, which no longer means
    /// anything: reading tolerates it and writing drops it.
    #[test]
    fn a_stored_spatial_reference_loads_and_is_dropped_on_save() {
        for stored in [
            json!({"expression": "front_wash", "spatialReference": "global"}),
            json!({"expression": "front_wash", "spatialReference": "group_local"}),
        ] {
            let selection = Selection::from_value(&stored).unwrap();
            assert_eq!(selection.expression, "front_wash");
            assert_eq!(selection.to_value(), json!({"expression": "front_wash"}));
        }
    }

    /// A legacy default carrying junk keys (the string-spread bug) is still a
    /// selection; a value with no expression is not.
    #[test]
    fn unknown_keys_are_tolerated_but_an_expression_is_required() {
        assert!(
            Selection::from_value(&json!({"0": "a", "expression": "all", "extra": 1})).is_some()
        );
        assert!(Selection::from_value(&json!({"spatialReference": "global"})).is_none());
        assert!(Selection::from_value(&json!("front_wash")).is_none());
    }
}
