//! The Selection arg value: *which* fixtures a clip targets.
//!
//! This is the one place the wire shape is spelled. Every producer and consumer
//! — eval, the score rows, the picker, Perform — goes through this type rather
//! than re-spelling the object.
//!
//! ```json
//! { "all": true }
//! { "groups": ["0b0f…", "9c1e…"] }
//! ```
//!
//! A selection is the whole venue or the union of some of its groups, each
//! named by the id of its row. A reference is to the group, not to what it is
//! called, so renaming a group changes no selection. A group that has since
//! been deleted is still listed and selects nothing.
//!
//! It always means the whole matched set. A random share of it is the Sparkle
//! form's coverage; a specific set of heads is a group.
//!
//! People and the agent say groups by name. That spelling —
//! `front_wash | back_wash` — is turned into ids where it is typed, by the
//! host that knows the venue, and is never stored.

use serde::{Deserialize, Serialize};

/// A Selection arg value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Selection {
    /// Every fixture of the venue.
    All,
    /// The union of these groups, by id, in the order they were chosen.
    Groups(Vec<String>),
}

/// The wire object. Exactly one of the two keys is what makes a value a
/// selection and not some other arg's JSON.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    all: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    groups: Option<Vec<String>>,
}

impl Serialize for Selection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => Wire {
                all: Some(true),
                groups: None,
            },
            Self::Groups(groups) => Wire {
                all: None,
                groups: Some(groups.clone()),
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Selection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Wire::deserialize(deserializer)? {
            Wire {
                all: Some(true),
                groups: None,
            } => Ok(Self::All),
            Wire {
                all: None,
                groups: Some(groups),
            } => Ok(Self::Groups(groups)),
            _ => Err(serde::de::Error::custom(
                "a selection is {\"all\": true} or {\"groups\": [group ids]}",
            )),
        }
    }
}

impl Selection {
    /// The whole venue — what a preview with no venue context uses.
    #[must_use]
    pub fn all() -> Self {
        Self::All
    }

    /// The union of `groups`, by id. Repeats are dropped, first mention kept.
    #[must_use]
    pub fn groups(groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut ids: Vec<String> = Vec::new();
        for id in groups {
            let id = id.into();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Self::Groups(ids)
    }

    /// The group ids this selection names; none for the whole venue.
    #[must_use]
    pub fn group_ids(&self) -> &[String] {
        match self {
            Self::All => &[],
            Self::Groups(groups) => groups,
        }
    }

    /// One string for this selection: what a cell is labelled with so heads
    /// can be split by the selection they came from, and what a log line says.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::All => "all".into(),
            Self::Groups(groups) => groups.join("|"),
        }
    }

    /// Read a stored arg value. `None` when the value is not a selection.
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
    fn both_forms_round_trip() {
        for (value, selection) in [
            (json!({"all": true}), Selection::All),
            (json!({"groups": ["a", "b"]}), Selection::groups(["a", "b"])),
            (json!({"groups": []}), Selection::Groups(vec![])),
        ] {
            assert_eq!(Selection::from_value(&value), Some(selection.clone()));
            assert_eq!(selection.to_value(), value);
        }
    }

    /// Choosing a group twice is choosing it once.
    #[test]
    fn a_repeated_group_is_listed_once() {
        assert_eq!(
            Selection::groups(["a", "b", "a"]),
            Selection::Groups(vec!["a".into(), "b".into()])
        );
    }

    /// Anything that is not exactly one of the two forms is not a selection:
    /// a name expression from before groups were referenced by id, both keys
    /// at once, or another arg's value.
    #[test]
    fn other_values_are_not_selections() {
        for value in [
            json!({"expression": "front_wash"}),
            json!({"all": true, "groups": ["a"]}),
            json!({"all": false}),
            json!({}),
            json!("front_wash"),
        ] {
            assert_eq!(Selection::from_value(&value), None, "{value}");
        }
    }
}
