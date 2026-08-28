//! The Selection arg value: *which* fixtures a clip targets, and *how many*
//! of them.
//!
//! This is the one place the wire shape is spelled. Every producer and consumer
//! — eval, the score DSL, the legacy upgrade, the graph validator, the strip UI,
//! Python — goes through this type rather than re-spelling the object.
//!
//! ```json
//! { "expression": "front_wash & left", "spatialReference": "global", "subset": {"fraction": 0.5} }
//! ```
//!
//! *Which* is the group expression alone: there is no positional grammar, so a
//! picker UI never has to parse an expression back out. *How many* is [`Subset`],
//! a separate field.
//!
//! Absence is meaning, not an error: a value without `subset` is the whole set,
//! and one without `spatialReference` is `global` — so every selection stored
//! before this field existed reads back unchanged, and an all-subset never
//! serializes the key.

use serde::{Deserialize, Serialize};

/// How much of the resolved head set a selection keeps.
///
/// The wire forms are `"all"`, `{"fraction": 0.5}` and `{"count": 3}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subset {
    /// Every fixture the expression matched.
    #[default]
    All,
    /// A share of the matched set, rounded to nearest and never below one.
    Fraction(f64),
    /// A fixed number, clamped to the size of the matched set.
    Count(u32),
}

impl Subset {
    /// Whether this is the whole set — the default, and the value that is
    /// omitted from the wire entirely.
    #[must_use]
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// How many of `n` units to keep. `All` keeps `n`; a fraction is clamped to
    /// `0..=1`, rounded to nearest, and floored at one for a non-empty set; a
    /// count is clamped to `n`.
    #[must_use]
    pub fn keep(&self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        match *self {
            Self::All => n,
            #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
            Self::Fraction(f) => {
                let k = (n as f64 * f.clamp(0.0, 1.0)).round() as usize;
                k.clamp(1, n)
            }
            Self::Count(c) => (c as usize).min(n),
        }
    }
}

fn global() -> String {
    "global".to_owned()
}

/// A Selection arg value.
///
/// Deserializing tolerates the two optional halves being absent (see the module
/// doc) but requires `expression`, which is what distinguishes a Selection value
/// from any other arg's JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub expression: String,
    #[serde(default = "global")]
    pub spatial_reference: String,
    /// Skipped when `All` so a selection that never used a subset keeps the
    /// exact bytes it had before the field existed.
    #[serde(default, skip_serializing_if = "Subset::is_all")]
    pub subset: Subset,
}

impl Selection {
    /// The whole venue, whole set — what a preview with no venue context uses.
    #[must_use]
    pub fn all() -> Self {
        Self::new("all")
    }

    /// A whole-set selection over `expression`, in the `global` space.
    #[must_use]
    pub fn new(expression: impl Into<String>) -> Self {
        Self {
            expression: expression.into(),
            spatial_reference: global(),
            subset: Subset::All,
        }
    }

    #[must_use]
    pub fn with_spatial_reference(mut self, spatial_reference: impl Into<String>) -> Self {
        self.spatial_reference = spatial_reference.into();
        self
    }

    #[must_use]
    pub fn with_subset(mut self, subset: Subset) -> Self {
        self.subset = subset;
        self
    }

    /// Read a stored arg value. `None` when the value is not a selection —
    /// no `expression` string, or a `subset` that is none of the three forms.
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

    /// The zero-migration promise: a stored two-key selection reads back as an
    /// all-subset, and writing it emits the same two keys.
    #[test]
    fn absence_of_subset_round_trips_as_all() {
        let value = json!({"expression": "front_wash", "spatialReference": "group_local"});
        let selection = Selection::from_value(&value).unwrap();
        assert_eq!(selection.subset, Subset::All);
        assert_eq!(selection.to_value(), value);
    }

    #[test]
    fn both_optional_halves_may_be_absent() {
        let selection = Selection::from_value(&json!({"expression": "all"})).unwrap();
        assert_eq!(selection.spatial_reference, "global");
        assert_eq!(selection.subset, Subset::All);
    }

    #[test]
    fn subset_forms_round_trip() {
        for (wire, subset) in [
            (json!({"fraction": 0.5}), Subset::Fraction(0.5)),
            (json!({"count": 3}), Subset::Count(3)),
        ] {
            let value = json!({
                "expression": "spots",
                "spatialReference": "global",
                "subset": wire,
            });
            let selection = Selection::from_value(&value).unwrap();
            assert_eq!(selection.subset, subset);
            assert_eq!(selection.to_value(), value);
        }
    }

    /// A legacy default carrying junk keys (the string-spread bug) is still a
    /// selection; a value with no expression, or a malformed subset, is not.
    #[test]
    fn unknown_keys_are_tolerated_but_a_bad_subset_is_not() {
        assert!(
            Selection::from_value(&json!({"0": "a", "expression": "all", "extra": 1})).is_some()
        );
        assert!(Selection::from_value(&json!({"spatialReference": "global"})).is_none());
        assert!(Selection::from_value(&json!({"expression": "all", "subset": "half"})).is_none());
        assert!(Selection::from_value(&json!("front_wash")).is_none());
    }

    #[test]
    fn keep_rounds_to_nearest_and_never_empties_a_non_empty_set() {
        assert_eq!(Subset::All.keep(7), 7);
        assert_eq!(Subset::Fraction(0.5).keep(7), 4); // 3.5 rounds up
        assert_eq!(Subset::Fraction(0.5).keep(6), 3);
        assert_eq!(Subset::Fraction(0.1).keep(4), 1); // 0.4 would round to 0
        assert_eq!(Subset::Fraction(0.5).keep(0), 0);
        assert_eq!(Subset::Fraction(2.0).keep(5), 5);
        assert_eq!(Subset::Count(3).keep(7), 3);
        assert_eq!(Subset::Count(99).keep(7), 7); // clamped to the set
        assert_eq!(Subset::Count(3).keep(0), 0);
    }
}
