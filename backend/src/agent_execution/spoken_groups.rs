//! The agent says groups by name; the library stores them by id.
//!
//! Every host translates here, once, at the edge: a name the agent wrote
//! becomes the id of the group it names, and a document on its way to Python
//! has every group id spelled by name. A selection crosses as
//! `{"expression": "front_wash | spots"}`, which is what the Python API has
//! always read and written.

use serde_json::{json, Value};
use std::path::Path;

use crate::controls::ALL;
use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::groups::{normalize_group_name, GroupTreeNode};
use crate::models::selection::Selection;
use crate::services::groups::{GroupNames, GroupSources};
use luma_patterns::Score;

/// One read of a venue's groups, for every name in one host call.
#[derive(Default)]
pub(crate) struct SpokenGroups {
    tree: Vec<GroupTreeNode>,
}

impl SpokenGroups {
    pub(crate) async fn read(
        resource_root: &Path,
        access: &mut impl AuthorizedVenue,
    ) -> Result<Self, String> {
        Ok(Self {
            tree: GroupSources::read(resource_root, access).await?.tree(),
        })
    }

    fn names(&self) -> GroupNames<'_> {
        GroupNames::of_tree(&self.tree)
    }

    /// The selection `text` names: `all`, a group, or several joined by `|`.
    pub(crate) fn selection(&self, text: &str) -> Result<Selection, String> {
        self.names().parse(text).map_err(|error| {
            format!(
                "{error} Use groups() for exact names, group(name, fixtures) to create one — \
                 also for an intersection or a subset — or generate_groups() for suggestions."
            )
        })
    }

    /// The id of the one group `name` names, or `all`: what a Perform group
    /// cell holds.
    pub(crate) fn group(&self, name: &str) -> Result<String, String> {
        match self.selection(name)? {
            Selection::All => Ok(ALL.to_owned()),
            Selection::Groups(groups) => match <[String; 1]>::try_from(groups) {
                Ok([group]) => Ok(group),
                Err(_) => Err(format!("{name:?} is several groups; name one")),
            },
        }
    }

    /// `value` as the agent reads it: every selection as its expression, and
    /// every other mention of a group's id — a string or an object key — as
    /// its name. A deleted group has no name; its id stays.
    pub(crate) fn spoken(&self, mut value: Value) -> Value {
        self.speak(&mut value);
        value
    }

    fn speak(&self, value: &mut Value) {
        if let Some(selection) = Selection::from_value(value) {
            *value = json!({ "expression": self.names().spell(&selection) });
            return;
        }
        match value {
            Value::String(text) => {
                if let Some(name) = self.names().name(text) {
                    *text = name.to_owned();
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.speak(item)),
            Value::Object(object) => {
                let names = self.names();
                *object = std::mem::take(object)
                    .into_iter()
                    .map(|(key, mut child)| {
                        self.speak(&mut child);
                        (names.name(&key).map_or(key.clone(), str::to_owned), child)
                    })
                    .collect();
            }
            _ => {}
        }
    }

    /// The score a Python edit hands back, each clip's expression turned into
    /// the groups it names.
    ///
    /// A clip whose expression is what `stored` already spells keeps the
    /// stored selection as it is, so a clip the edit did not touch is not
    /// rewritten — nor refused for a group deleted since.
    pub(crate) fn score(&self, mut document: Value, stored: &Score) -> Result<Score, String> {
        let names = self.names();
        if let Some(clips) = document.get_mut("clips").and_then(Value::as_object_mut) {
            for (id, clip) in clips {
                let Some(selection) = clip.get_mut("selection") else {
                    continue;
                };
                let expression = selection
                    .get("expression")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("clip {id}: a selection is a group expression"))?;
                let kept = stored
                    .clips
                    .get(id)
                    .map(|clip| &clip.selection)
                    .filter(|stored| names.spell(stored) == normalized(expression));
                *selection = match kept {
                    Some(stored) => stored.to_value(),
                    None => self
                        .selection(expression)
                        .map_err(|error| format!("clip {id}: {error}"))?
                        .to_value(),
                };
            }
        }
        serde_json::from_value(document).map_err(|error| error.to_string())
    }
}

/// `expression` as [`GroupNames::spell`] would write it, so an expression the
/// agent retyped with other spacing or case still reads as unchanged.
fn normalized(expression: &str) -> String {
    expression
        .split('|')
        .map(|term| normalize_group_name(term.trim()))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::groups::GroupOrigin;

    fn spoken() -> SpokenGroups {
        SpokenGroups {
            tree: ["front_wash", "spots"]
                .iter()
                .map(|name| GroupTreeNode {
                    id: format!("id-{name}"),
                    name: (*name).into(),
                    label: (*name).into(),
                    parent_id: None,
                    origin: GroupOrigin::Manual,
                    role: None,
                    fixtures: vec![],
                })
                .collect(),
        }
    }

    fn clip(selection: Value) -> Value {
        json!({
            "name": "Clip", "start": 0.0, "duration": 4.0, "seed": 1,
            "selection": selection,
            "graph": {"version": 3, "nodes": {"color1": {"kind": "color"}}},
        })
    }

    /// What the agent reads names groups; what it writes back is stored by id.
    #[test]
    fn a_score_round_trips_through_its_names() {
        let groups = spoken();
        let stored: Score = serde_json::from_value(json!({"clips": {
            "a": clip(json!({"groups": ["id-front_wash", "id-spots"]})),
            "b": clip(json!({"all": true})),
        }}))
        .unwrap();
        let read = groups.spoken(json!(stored));
        assert_eq!(
            read["clips"]["a"]["selection"],
            json!({"expression": "front_wash | spots"})
        );
        assert_eq!(read["clips"]["b"]["selection"], json!({"expression": "all"}));
        assert_eq!(groups.score(read, &stored), Ok(stored));
    }

    /// A clip the edit left alone keeps a group that has since been deleted;
    /// one it changed must name groups the venue has.
    #[test]
    fn an_untouched_clip_keeps_a_deleted_group() {
        let groups = spoken();
        let stored: Score = serde_json::from_value(json!({"clips": {
            "a": clip(json!({"groups": ["gone", "id-spots"]})),
        }}))
        .unwrap();
        let read = groups.spoken(json!(stored));
        assert_eq!(read["clips"]["a"]["selection"], json!({"expression": "spots"}));
        assert_eq!(groups.score(read.clone(), &stored), Ok(stored.clone()));

        let mut changed = read;
        changed["clips"]["a"]["selection"] = json!({"expression": "front_wash"});
        let saved = groups.score(changed.clone(), &stored).unwrap();
        assert_eq!(
            saved.clips["a"].selection,
            Selection::groups(["id-front_wash"])
        );
        changed["clips"]["a"]["selection"] = json!({"expression": "missing"});
        let error = groups.score(changed, &stored).unwrap_err();
        assert!(error.starts_with("clip a: Unknown group name(s): missing"), "{error}");
    }

    /// Group ids are spelled wherever they appear, keys included; `all` and
    /// everything else passes through.
    #[test]
    fn group_ids_are_spoken_wherever_they_appear() {
        let groups = spoken();
        assert_eq!(
            groups.spoken(json!({
                "chosen": ["id-spots", "all"],
                "base": {"id-front_wash": [1.0, 0.0, 0.0]},
                "cell": {"kind": "group", "group": "id-spots", "label": "id"},
            })),
            json!({
                "chosen": ["spots", "all"],
                "base": {"front_wash": [1.0, 0.0, 0.0]},
                "cell": {"kind": "group", "group": "spots", "label": "id"},
            })
        );
        assert_eq!(groups.group("Spots"), Ok("id-spots".into()));
        assert_eq!(groups.group("all"), Ok("all".into()));
        assert!(groups.group("spots | front_wash").is_err());
    }
}
