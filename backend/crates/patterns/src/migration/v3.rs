//! Version 3 → 4: Chase, Pulse, Dissolve and Shimmer stopped being kernels
//! with built-in repeat controls. A saved node becomes either the beat-driven
//! pattern with the same argument keys, or the trigger-driven graph plus an
//! explicit Beat trigger node when its trigger was left automatic.
//!
//! Version 4 → 5 and 5 → 6: a placed effect is its composition. Both steps
//! flatten the copies of version 2 library effects the earlier conversions
//! left in a score; documents the first builds of those versions saved with
//! the copies still in them converge on the same result.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// The version 3 catalog, frozen when effects became graphs over event ages.
pub fn v3_library() -> Library {
    static LIBRARY: std::sync::OnceLock<Library> = std::sync::OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../migrations/v3-library.json"))
                .expect("checked-in v3 catalog")
        })
        .clone()
}

pub fn validate_v3(score: &Score) -> Result<()> {
    if score.version != 3 {
        return Err(Error("expected a version 3 score".into()));
    }
    let frozen = v3_library();
    score
        .validate_using(&frozen, &|op| {
            frozen
                .definitions
                .values()
                .find(|d| d.body == Body::Primitive(op))
                .expect("v3 kernel vocabulary")
                .clone()
        })
        .map(|_| ())
}

pub fn upgrade_v3(score: &Score) -> Result<Score> {
    if score.version != 3 {
        return Err(Error(format!(
            "cannot migrate score version {}",
            score.version
        )));
    }
    validate_v3(score)?;
    // Exposed inputs that carry a real value: a clip override, a non-automatic
    // default, or a caller wiring a source that is itself real. A trigger fed
    // only by automatic defaults, however deeply, still needs a Beat trigger.
    let mut supplied: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for clip in score.clips.values() {
        supplied
            .entry(&clip.graph)
            .or_default()
            .extend(clip.inputs.keys().map(String::as_str));
    }
    for (id, definition) in &score.definitions {
        for (key, input) in &definition.inputs {
            if !matches!(input.default, None | Some(Value::Events(Events::Automatic))) {
                supplied.entry(id).or_default().insert(key);
            }
        }
    }
    loop {
        let mut changed = false;
        for (caller, definition) in &score.definitions {
            let Body::Graph(graph) = &definition.body else {
                continue;
            };
            for node in graph.nodes.values() {
                for (key, binding) in &node.inputs {
                    let real = match binding {
                        Binding::Value { value } => *value != Value::Events(Events::Automatic),
                        Binding::Connection { .. } => true,
                        Binding::Input { input } => supplied
                            .get(caller.as_str())
                            .is_some_and(|keys| keys.contains(input.as_str())),
                    };
                    if real && supplied.entry(&node.definition).or_default().insert(key) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut definitions = score.definitions.clone();
    let mut unused: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    inline_retired_graphs(&mut definitions);
    let local: BTreeSet<String> = definitions.keys().cloned().collect();
    let local: BTreeSet<&str> = local.iter().map(String::as_str).collect();
    let mut clips = score.clips.clone();
    for (id, definition) in &mut definitions {
        let dropped = convert(id, definition, &supplied, &local);
        if !dropped.is_empty() {
            unused.entry(id.clone()).or_default().extend(dropped);
        }
    }
    remove_unused_exposures(&mut definitions, &mut clips, unused);
    let upgraded = Score {
        version: 4,
        definitions,
        clips,
    };
    upgraded.validate(&standard_library())?;
    Ok(upgraded)
}

/// Exposed inputs that fed only a removed control leave the interface, and
/// callers stop binding them; their own exposures may then become unused.
fn remove_unused_exposures(
    definitions: &mut BTreeMap<String, Definition>,
    clips: &mut BTreeMap<String, Clip>,
    mut unused: BTreeMap<String, BTreeSet<String>>,
) {
    let mut removed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    while !unused.is_empty() {
        let mut next: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (id, keys) in unused {
            let Some(definition) = definitions.get_mut(&id) else {
                continue;
            };
            let Body::Graph(graph) = &mut definition.body else {
                continue;
            };
            let referenced: BTreeSet<String> = graph
                .nodes
                .values()
                .flat_map(|node| node.inputs.values())
                .chain(graph.outputs.values())
                .filter_map(|binding| match binding {
                    Binding::Input { input } => Some(input.clone()),
                    _ => None,
                })
                .collect();
            let gone: BTreeSet<String> = keys
                .into_iter()
                .filter(|key| !referenced.contains(key))
                .collect();
            for key in &gone {
                graph.input_nodes.remove(key);
                definition.inputs.remove(key);
            }
            for (caller_id, caller) in definitions.iter_mut() {
                let Body::Graph(graph) = &mut caller.body else {
                    continue;
                };
                for node in graph
                    .nodes
                    .values_mut()
                    .filter(|node| node.definition == id)
                {
                    for key in &gone {
                        if let Some(Binding::Input { input }) = node.inputs.remove(key) {
                            next.entry(caller_id.clone()).or_default().insert(input);
                        }
                    }
                }
            }
            removed.entry(id).or_default().extend(gone);
        }
        unused = next;
    }
    for clip in clips.values_mut() {
        if let Some(gone) = removed.get(&clip.graph) {
            clip.inputs.retain(|key, _| !gone.contains(key));
        }
        clip.inputs
            .retain(|_, value| *value != Value::Events(Events::Automatic));
    }
}

/// Version 4 → 5.
pub fn upgrade_v4(score: &Score) -> Result<Score> {
    flatten_from(score, 4)
}

/// Version 5 → 6.
pub fn upgrade_v5(score: &Score) -> Result<Score> {
    flatten_from(score, 5)
}

fn flatten_from(score: &Score, version: u32) -> Result<Score> {
    if score.version != version {
        return Err(Error(format!(
            "cannot migrate score version {}",
            score.version
        )));
    }
    let mut upgraded = score.clone();
    super::flatten::retire(&mut upgraded);
    upgraded.validate(&standard_library())?;
    super::flatten(&mut upgraded)?;
    upgraded.version = version + 1;
    upgraded.validate(&standard_library())?;
    Ok(upgraded)
}

/// A version 3 library graph that version 4 no longer ships, but whose parts
/// still exist, becomes a score-local copy under its old id. Chase, Pulse,
/// Dissolve, Shimmer and their kernels are rebuilt instead.
fn inline_retired_graphs(definitions: &mut BTreeMap<String, Definition>) {
    let current = standard_library();
    let frozen = v3_library();
    loop {
        let wanted: BTreeSet<String> = definitions
            .values()
            .filter_map(|definition| match &definition.body {
                Body::Graph(graph) => Some(graph.nodes.values()),
                Body::Primitive(_) => None,
            })
            .flatten()
            .map(|node| node.definition.clone())
            .filter(|id| {
                !current.definitions.contains_key(id)
                    && !definitions.contains_key(id)
                    && !REBUILT.contains(&id.as_str())
                    && frozen
                        .definitions
                        .get(id)
                        .is_some_and(|definition| matches!(definition.body, Body::Graph(_)))
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        for id in wanted {
            definitions.insert(id.clone(), frozen.definitions[&id].clone());
        }
    }
}

/// Version 3 ids whose nodes are rewritten rather than copied.
const REBUILT: [&str; 11] = [
    "chase",
    "pulse",
    "pulse_dimmer",
    "dissolve_flash",
    "shimmer",
    "dissolve_mask",
    "event_mask",
    "write_dimmer",
    "core/chase_events",
    "core/pulse_events",
    "core/dissolve_events",
];

/// Convert every node of an authored version 3 graph in place. Returns the
/// exposed inputs whose only consumer was a removed control.
fn convert(
    id: &str,
    definition: &mut Definition,
    supplied_by: &BTreeMap<&str, BTreeSet<&str>>,
    local: &BTreeSet<&str>,
) -> BTreeSet<String> {
    let supplied = supplied_by.get(id).cloned().unwrap_or_default();
    let Body::Graph(graph) = &mut definition.body else {
        return BTreeSet::new();
    };
    let mut taken: BTreeSet<String> = graph.nodes.keys().cloned().collect();
    let mut renamed_outputs: BTreeMap<String, BTreeMap<&str, &str>> = BTreeMap::new();
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    let mut added = BTreeMap::new();
    for (id, node) in &mut graph.nodes {
        let explicit = |binding: Option<&Binding>| match binding {
            None
            | Some(Binding::Value {
                value: Value::Events(Events::Automatic),
            }) => false,
            Some(Binding::Input { input }) => {
                supplied.contains(input.as_str())
                    || definition.inputs.get(input).is_some_and(|spec| {
                        !matches!(spec.default, None | Some(Value::Events(Events::Automatic)))
                    })
            }
            Some(_) => true,
        };
        let drop = |node: &mut Node, keys: &[&str], dropped: &mut BTreeSet<String>| {
            for key in keys {
                if let Some(Binding::Input { input }) = node.inputs.remove(*key) {
                    dropped.insert(input);
                }
            }
        };
        let rename = |node: &mut Node, from: &str, to: &str| {
            if let Some(binding) = node.inputs.remove(from) {
                node.inputs.insert(to.into(), binding);
            }
        };
        let beat_trigger = |node: &mut Node,
                            id: &str,
                            taken: &mut BTreeSet<String>,
                            dropped: &mut BTreeSet<String>| {
            let mut inputs = BTreeMap::new();
            for key in ["repeat", "delay", "grid_aligned"] {
                if let Some(binding) = node.inputs.remove(key) {
                    inputs.insert(key.into(), binding);
                }
            }
            if let Some(Binding::Input { input }) = node.inputs.remove("trigger") {
                dropped.insert(input);
            }
            let trigger = super::unique(taken, &format!("{id}/trigger"));
            node.inputs.insert(
                "trigger".into(),
                Binding::Connection {
                    node: trigger.clone(),
                    output: "trigger".into(),
                },
            );
            (
                trigger,
                Node {
                    position: None,
                    definition: "beat_trigger".into(),
                    inputs,
                },
            )
        };
        let has_trigger = explicit(node.inputs.get("trigger"));
        match node.definition.as_str() {
            "chase" | "core/chase_events" => {
                let kernel = node.definition == "core/chase_events";
                if has_trigger || kernel {
                    node.definition = "chase".into();
                    if !kernel {
                        // The graph consumes resolved coordinates; keep the
                        // authored mapping choice on an explicit resolver.
                        let mapping = super::unique(&mut taken, &format!("{id}/mapping"));
                        let spec = node.inputs.remove("mapping").unwrap_or_else(|| {
                            v3_library().definitions["chase"].inputs["mapping"]
                                .default
                                .clone()
                                .expect("v3 chase mapping default")
                                .into()
                        });
                        added.insert(
                            mapping.clone(),
                            Node {
                                position: None,
                                definition: "resolve_mapping".into(),
                                inputs: BTreeMap::from([("mapping".into(), spec)]),
                            },
                        );
                        node.inputs.insert(
                            "mapping".into(),
                            Binding::Connection {
                                node: mapping,
                                output: "coordinates".into(),
                            },
                        );
                        renamed_outputs.insert(id.clone(), BTreeMap::from([("dimmer", "mask")]));
                    }
                    if has_trigger {
                        drop(node, &["repeat", "delay", "grid_aligned"], &mut dropped);
                    } else {
                        let (key, trigger) = beat_trigger(node, id, &mut taken, &mut dropped);
                        added.insert(key, trigger);
                    }
                } else {
                    node.definition = "beat_chase".into();
                    drop(node, &["trigger"], &mut dropped);
                }
            }
            "pulse" | "pulse_dimmer" | "core/pulse_events" => {
                let kernel = node.definition == "core/pulse_events";
                rename(node, "travel", "duration");
                if has_trigger || kernel {
                    node.definition = "pulse".into();
                    if !kernel {
                        renamed_outputs.insert(id.clone(), BTreeMap::from([("dimmer", "mask")]));
                    }
                    if has_trigger {
                        drop(node, &["repeat", "delay", "grid_aligned"], &mut dropped);
                    } else {
                        let (key, trigger) = beat_trigger(node, id, &mut taken, &mut dropped);
                        added.insert(key, trigger);
                    }
                } else {
                    node.definition = "beat_pulse".into();
                    drop(node, &["trigger"], &mut dropped);
                }
            }
            "dissolve_flash" | "core/dissolve_events" => {
                let kernel = node.definition == "core/dissolve_events";
                rename(node, "travel", "duration");
                rename(node, "shape", "proportion");
                drop(node, &["refresh", "refresh_every", "reseed"], &mut dropped);
                if has_trigger || kernel {
                    node.definition = "dissolve".into();
                    if !kernel {
                        renamed_outputs.insert(id.clone(), BTreeMap::from([("dimmer", "mask")]));
                    }
                    if has_trigger {
                        drop(node, &["repeat", "delay", "grid_aligned"], &mut dropped);
                    } else {
                        let (key, trigger) = beat_trigger(node, id, &mut taken, &mut dropped);
                        added.insert(key, trigger);
                    }
                } else {
                    node.definition = "beat_dissolve".into();
                    drop(node, &["trigger"], &mut dropped);
                }
            }
            "shimmer" => {
                node.definition = "beat_shimmer".into();
                drop(node, &["seed"], &mut dropped);
            }
            "dissolve_mask" => {
                node.definition = "random_selection".into();
                rename(node, "coverage", "proportion");
                rename(node, "cycle", "index");
                drop(node, &["refresh", "refresh_every", "reseed"], &mut dropped);
                renamed_outputs.insert(id.clone(), BTreeMap::from([("mask", "selected")]));
            }
            // A shared authored graph keeps its trigger input when another
            // caller supplies real events; this caller's automatic trigger
            // becomes its own Beat trigger from the sibling repeat controls.
            target
                if local.contains(target)
                    && !has_trigger
                    && node.inputs.contains_key("trigger")
                    && supplied_by
                        .get(target)
                        .is_some_and(|keys| keys.contains("trigger")) =>
            {
                let (key, trigger) = beat_trigger(node, id, &mut taken, &mut dropped);
                added.insert(key, trigger);
            }
            "mask_color" => {
                node.definition = "core/multiply".into();
                rename(node, "color", "a");
                rename(node, "mask", "b");
                renamed_outputs.insert(id.clone(), BTreeMap::from([("color", "value")]));
            }
            "write_dimmer" => {
                node.definition = "uniform_mask".into();
                rename(node, "value", "coverage");
                renamed_outputs.insert(id.clone(), BTreeMap::from([("dimmer", "mask")]));
            }
            "output" => {
                // Brightness now rides in the applied color.
                if let Some(dimmer) = node.inputs.remove("dimmer") {
                    let color = node
                        .inputs
                        .remove("color")
                        .unwrap_or_else(|| Value::Color([1.0; 3]).into());
                    let brightness = super::unique(&mut taken, &format!("{id}/brightness"));
                    added.insert(
                        brightness.clone(),
                        Node {
                            position: None,
                            definition: "core/multiply".into(),
                            inputs: BTreeMap::from([("a".into(), color), ("b".into(), dimmer)]),
                        },
                    );
                    node.inputs.insert(
                        "color".into(),
                        Binding::Connection {
                            node: brightness,
                            output: "value".into(),
                        },
                    );
                }
            }
            "event_mask" => {
                // duration/elapsed/present/shape → Motion, an activity product
                // and an Event envelope.
                let time = super::unique(&mut taken, &format!("{id}/time"));
                let weight = super::unique(&mut taken, &format!("{id}/weight"));
                let wire = |node: &str, output: &str| Binding::Connection {
                    node: node.into(),
                    output: output.into(),
                };
                let mut time_inputs = BTreeMap::new();
                if let Some(elapsed) = node.inputs.remove("elapsed") {
                    time_inputs.insert("elapsed".into(), elapsed);
                }
                if let Some(duration) = node.inputs.remove("duration") {
                    time_inputs.insert("travel".into(), duration);
                }
                added.insert(
                    time.clone(),
                    Node {
                        position: None,
                        definition: "motion".into(),
                        inputs: time_inputs,
                    },
                );
                let present = node
                    .inputs
                    .remove("present")
                    .unwrap_or_else(|| Value::Proportion(1.0).into());
                added.insert(
                    weight.clone(),
                    Node {
                        position: None,
                        definition: "core/multiply".into(),
                        inputs: BTreeMap::from([
                            ("a".into(), present),
                            ("b".into(), wire(&time, "active")),
                        ]),
                    },
                );
                node.definition = "event_envelope".into();
                node.inputs
                    .insert("progress".into(), wire(&time, "progress"));
                node.inputs.insert("weight".into(), wire(&weight, "value"));
            }
            _ => {}
        }
    }
    graph.nodes.extend(added);
    let rename_binding = |binding: &mut Binding| match binding {
        Binding::Connection { node, output } => {
            if let Some(to) = renamed_outputs
                .get(node)
                .and_then(|outputs| outputs.get(output.as_str()))
            {
                *output = (*to).into();
            }
        }
        Binding::Value { value } if *value == Value::Events(Events::Automatic) => {
            *value = Value::Events(Events::Beats {
                times: EventTimes::new(Vec::new()).expect("empty event stream"),
            });
        }
        _ => {}
    };
    for node in graph.nodes.values_mut() {
        node.inputs.values_mut().for_each(rename_binding);
    }
    graph.outputs.values_mut().for_each(rename_binding);
    // Exposed triggers now feed fixed event inputs; an event value never
    // varied per frame anyway.
    let triggers: BTreeSet<String> = graph
        .nodes
        .values()
        .filter_map(|node| match node.inputs.get("trigger") {
            Some(Binding::Input { input }) => Some(input.clone()),
            _ => None,
        })
        .collect();
    for (key, input) in definition.inputs.iter_mut() {
        if triggers.contains(key) {
            input.rate = Rate::Fixed;
        }
        if input.default == Some(Value::Events(Events::Automatic)) {
            input.default = Some(Value::Events(Events::Beats {
                times: EventTimes::new(Vec::new()).expect("empty event stream"),
            }));
        }
    }
    dropped
}
