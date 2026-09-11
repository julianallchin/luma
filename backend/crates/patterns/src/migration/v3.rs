//! Version 3 → 4: Chase, Pulse, Dissolve and Shimmer stopped being kernels
//! with built-in repeat controls. A saved node becomes either the beat-driven
//! pattern with the same argument keys, or the trigger-driven graph plus an
//! explicit Beat trigger node when its trigger was left automatic.
//!
//! Version 4 → 5: the score-local copies of version 2 library effects that the
//! earlier conversions left behind become the shipped patterns.
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
    upgrade_v3_tracking(score, true).map(|(score, _)| score)
}

/// Upgrade, also reporting which score-local copies of version 2 library
/// effects became which pattern. Without `retarget`, the copies stay: the
/// shape the earlier builds wrote, which version 5 recognizes.
pub(super) fn upgrade_v3_tracking(
    score: &Score,
    retarget: bool,
) -> Result<(Score, BTreeMap<String, &'static Pattern>)> {
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
    let (retargeted, mut unused) = if retarget {
        retarget_library_copies(&mut definitions, &score.clips, library_copies())
    } else {
        Default::default()
    };
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
    Ok((upgraded, retargeted))
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

/// Version 4 → 5. Documents an earlier build converted still carry the
/// copies of version 2 library effects, already rewritten around the
/// version 4 kernels; those become the shipped patterns too.
pub fn upgrade_v4(score: &Score) -> Result<Score> {
    if score.version != 4 {
        return Err(Error(format!(
            "cannot migrate score version {}",
            score.version
        )));
    }
    score.validate(&standard_library())?;
    let mut definitions = score.definitions.clone();
    let mut clips = score.clips.clone();
    let (_, unused) = retarget_library_copies(&mut definitions, &clips, library_copies_v4());
    remove_unused_exposures(&mut definitions, &mut clips, unused);
    let upgraded = Score {
        version: 5,
        definitions,
        clips,
    };
    upgraded.validate(&standard_library())?;
    Ok(upgraded)
}

/// A version 2 library effect a score used was copied into it as
/// `<effect>/signals`, nested as deeply as that library was. A copy that is
/// still the library graph becomes the pattern with the same controls.
pub struct Pattern {
    pub id: &'static str,
    pub renames: &'static [(&'static str, &'static str)],
    pub dropped: &'static [&'static str],
}
const fn keep(id: &'static str) -> (&'static str, Pattern) {
    let pattern = Pattern {
        id,
        renames: &[],
        dropped: &[],
    };
    (id, pattern)
}
const PATTERNS: [(&str, Pattern); 22] = [
    (
        "chase",
        Pattern {
            id: "beat_chase",
            renames: &[],
            dropped: &[],
        },
    ),
    (
        "pulse",
        Pattern {
            id: "beat_pulse",
            renames: &[("travel", "duration")],
            dropped: &[],
        },
    ),
    (
        "pulse_dimmer",
        Pattern {
            id: "beat_pulse",
            renames: &[("travel", "duration")],
            dropped: &[],
        },
    ),
    (
        "dissolve_flash",
        Pattern {
            id: "beat_dissolve",
            renames: &[("travel", "duration"), ("shape", "proportion")],
            dropped: &["refresh", "refresh_every", "reseed"],
        },
    ),
    (
        "write_dimmer",
        Pattern {
            id: "uniform_mask",
            renames: &[("value", "coverage")],
            dropped: &[],
        },
    ),
    keep("drum_pulse"),
    keep("band_pulse"),
    keep("noise_wash"),
    keep("random_heads"),
    keep("rainbow"),
    keep("spatial_gradient"),
    keep("gradient"),
    keep("harmony_color"),
    keep("strobe"),
    keep("wash"),
    keep("write_strobe"),
    keep("uniform_mask"),
    keep("mapped_position"),
    keep("noise_mask"),
    keep("band_mask"),
    keep("random_heads_mask"),
    keep("drum_mask"),
];

/// The copies the version 2 conversion makes of the library effects above,
/// for recognizing them in a document by content.
fn library_copies() -> &'static BTreeMap<String, Definition> {
    static COPIES: std::sync::OnceLock<BTreeMap<String, Definition>> = std::sync::OnceLock::new();
    COPIES.get_or_init(|| {
        let source = super::source_with_events(&Score {
            version: 2,
            ..Score::default()
        })
        .expect("version 2 library");
        let target = v3_library();
        let mut conversion = super::Conversion::new(&source, &target, std::iter::empty());
        for (effect, _) in &PATTERNS {
            conversion
                .definition(effect, BTreeMap::new(), false)
                .expect("library effect converts");
        }
        conversion.definitions
    })
}

/// The same copies as the version 3 → 4 conversion of an earlier build left
/// them: each placed the way a clip places an effect, so the conversion sees
/// the controls a clip supplies.
fn library_copies_v4() -> &'static BTreeMap<String, Definition> {
    static COPIES: std::sync::OnceLock<BTreeMap<String, Definition>> = std::sync::OnceLock::new();
    COPIES.get_or_init(|| {
        let mut definitions = library_copies().clone();
        for (effect, _) in &PATTERNS {
            let id = format!("{effect}/signals");
            definitions.insert(format!("{effect}/placed"), definitions[&id].instance(&id));
        }
        let score = Score {
            version: 3,
            definitions,
            clips: BTreeMap::new(),
        };
        let (score, _) = upgrade_v3_tracking(&score, false).expect("library copies convert");
        score
            .definitions
            .into_iter()
            .filter(|(id, _)| id.ends_with("/signals"))
            .collect()
    })
}

/// Returns the copies replaced, and the exposed inputs of each graph that fed
/// only a removed control.
fn retarget_library_copies(
    definitions: &mut BTreeMap<String, Definition>,
    clips: &BTreeMap<String, Clip>,
    canonical: &BTreeMap<String, Definition>,
) -> (
    BTreeMap<String, &'static Pattern>,
    BTreeMap<String, BTreeSet<String>>,
) {
    let copies: BTreeMap<String, &'static Pattern> = definitions
        .iter()
        .filter_map(|(id, definition)| {
            PATTERNS
                .iter()
                .find(|(effect, _)| canonical.get(&format!("{effect}/signals")) == Some(definition))
                .map(|(_, pattern)| (id.clone(), pattern))
        })
        .collect();
    let mut unused: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if copies.is_empty() {
        return (copies, unused);
    }
    // Every library copy, including the helpers the effects above called,
    // as they are before this pass rewires their insides.
    let disposable: BTreeSet<String> = definitions
        .iter()
        .filter(|(_, definition)| canonical.values().any(|copy| copy == *definition))
        .map(|(id, _)| id.clone())
        .collect();
    let library = standard_library();
    let callers: Vec<String> = definitions.keys().cloned().collect();
    for caller in &callers {
        let Body::Graph(graph) = &mut definitions.get_mut(caller).unwrap().body else {
            continue;
        };
        for node in graph.nodes.values_mut() {
            let Some(pattern) = copies.get(&node.definition) else {
                continue;
            };
            let target = &library.definitions[pattern.id];
            if !target.inputs.contains_key("trigger") {
                match node.inputs.get("trigger") {
                    None => {}
                    Some(Binding::Value {
                        value: Value::Events(Events::Automatic),
                    }) => {
                        node.inputs.remove("trigger");
                    }
                    // Real events keep the copy; its kernel becomes the
                    // trigger-driven graph below.
                    Some(_) => continue,
                }
            }
            node.definition = pattern.id.into();
            for (from, to) in pattern.renames {
                if let Some(binding) = node.inputs.remove(*from) {
                    node.inputs.insert((*to).into(), binding);
                }
            }
            for key in pattern.dropped {
                if let Some(Binding::Input { input }) = node.inputs.remove(*key) {
                    unused.entry(caller.clone()).or_default().insert(input);
                }
            }
        }
        // A dimmer that was only ever a mask is read as one.
        let masks: BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|(_, node)| {
                library
                    .definitions
                    .get(&node.definition)
                    .is_some_and(|definition| {
                        definition.outputs.contains_key("mask")
                            && !definition.outputs.contains_key("dimmer")
                    })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for binding in graph
            .nodes
            .values_mut()
            .flat_map(|node| node.inputs.values_mut())
            .chain(graph.outputs.values_mut())
        {
            if let Binding::Connection { node, output } = binding {
                if output == "dimmer" && masks.contains(node) {
                    *output = "mask".into();
                }
            }
        }
    }
    // Brightness rides in a pattern's color. An Apply, or a graph interface,
    // that took the pair from one node takes the color alone, which may in
    // turn fold its callers; any other reader of the old dimmer gets the
    // peak, and the color's readers the hue, as before.
    loop {
        let mut changed = false;
        for caller in &callers {
            let interface = |id: &str| {
                definitions
                    .get(id)
                    .or_else(|| library.definitions.get(id))
                    .map(|definition| &definition.outputs)
            };
            let Body::Graph(graph) = &definitions[caller].body else {
                continue;
            };
            let carrying: BTreeSet<String> = graph
                .nodes
                .iter()
                .filter(|(_, node)| {
                    interface(&node.definition).is_some_and(|outputs| {
                        outputs.contains_key("color") && !outputs.contains_key("dimmer")
                    })
                })
                .map(|(id, _)| id.clone())
                .collect();
            let reads = |binding: &Binding, output: &str| match binding {
                Binding::Connection { node, output: port } if port == output => {
                    carrying.contains(node).then(|| node.clone())
                }
                _ => None,
            };
            let mut folds: BTreeMap<String, (bool, Vec<String>)> = BTreeMap::new();
            for (id, node) in &graph.nodes {
                for (key, binding) in &node.inputs {
                    let Some(source) = reads(binding, "dimmer") else {
                        continue;
                    };
                    let same = Binding::Connection {
                        node: source.clone(),
                        output: "color".into(),
                    };
                    // White over a pattern left at its own color is that color.
                    let white = |bound: &Binding| {
                        *bound == Binding::from(Value::Color([1.0; 3]))
                            && !graph.nodes[&source].inputs.contains_key("color")
                    };
                    let foldable = match node.definition.as_str() {
                        "output" => {
                            key == "dimmer"
                                && node.inputs.get("color").is_none_or(|bound| *bound == same)
                        }
                        "mask_color" => {
                            key == "mask"
                                && node
                                    .inputs
                                    .get("color")
                                    .is_some_and(|bound| *bound == same || white(bound))
                        }
                        _ => false,
                    };
                    let entry = folds.entry(source).or_insert((true, Vec::new()));
                    entry.0 &= foldable;
                    entry.1.push(id.clone());
                }
            }
            for (key, binding) in &graph.outputs {
                let Some(source) = reads(binding, "dimmer") else {
                    continue;
                };
                let foldable = key == "dimmer"
                    && graph.outputs.get("color")
                        == Some(&Binding::Connection {
                            node: source.clone(),
                            output: "color".into(),
                        });
                let entry = folds.entry(source).or_insert((true, Vec::new()));
                entry.0 &= foldable;
            }
            if folds.is_empty() {
                continue;
            }
            changed = true;
            let definition = definitions.get_mut(caller).unwrap();
            let Body::Graph(graph) = &mut definition.body else {
                unreachable!()
            };
            let mut taken: BTreeSet<String> = graph.nodes.keys().cloned().collect();
            let mut added = BTreeMap::new();
            for (source, (foldable, readers)) in folds {
                let color = Binding::Connection {
                    node: source.clone(),
                    output: "color".into(),
                };
                if foldable {
                    for reader in readers {
                        if graph.nodes[&reader].definition == "output" {
                            let apply = graph.nodes.get_mut(&reader).unwrap();
                            apply.inputs.remove("dimmer");
                            apply.inputs.insert("color".into(), color.clone());
                            continue;
                        }
                        // A Mask color that re-applied the pattern's own
                        // brightness was the pattern's color all along.
                        graph.nodes.remove(&reader);
                        for binding in graph
                            .nodes
                            .values_mut()
                            .flat_map(|node| node.inputs.values_mut())
                            .chain(graph.outputs.values_mut())
                        {
                            if matches!(binding, Binding::Connection { node, .. } if *node == reader)
                            {
                                *binding = color.clone();
                            }
                        }
                    }
                    if graph.outputs.remove("dimmer").is_some() {
                        definition.outputs.remove("dimmer");
                    }
                    continue;
                }
                let brightness = super::unique(&mut taken, &format!("{source}/brightness"));
                let lit = super::unique(&mut taken, &format!("{source}/lit"));
                let ratio = super::unique(&mut taken, &format!("{source}/ratio"));
                let hue = super::unique(&mut taken, &format!("{source}/hue"));
                let wire = |node: &str, output: &str| Binding::Connection {
                    node: node.into(),
                    output: output.into(),
                };
                let node = |definition: &str, inputs: Vec<(&str, Binding)>| Node {
                    position: None,
                    definition: definition.into(),
                    inputs: inputs
                        .into_iter()
                        .map(|(key, binding)| (key.into(), binding))
                        .collect(),
                };
                added.insert(
                    brightness.clone(),
                    node("core/channel_maximum", vec![("value", color.clone())]),
                );
                added.insert(
                    lit.clone(),
                    node(
                        "core/greater",
                        vec![
                            ("a", wire(&brightness, "value")),
                            ("b", Value::Number(1e-5).into()),
                            ("tolerance", Value::Number(0.0).into()),
                        ],
                    ),
                );
                added.insert(
                    ratio.clone(),
                    node(
                        "core/divide",
                        vec![("a", color.clone()), ("b", wire(&brightness, "value"))],
                    ),
                );
                added.insert(
                    hue.clone(),
                    node(
                        "core/choose",
                        vec![
                            ("condition", wire(&lit, "mask")),
                            ("yes", wire(&ratio, "value")),
                            ("no", Value::Color([0.0; 3]).into()),
                        ],
                    ),
                );
                let split = |binding: &mut Binding| {
                    if let Binding::Connection { node, output } = binding {
                        if *node == source {
                            *node = if output == "dimmer" {
                                &brightness
                            } else {
                                &hue
                            }
                            .clone();
                            *output = "value".into();
                        }
                    }
                };
                graph
                    .nodes
                    .values_mut()
                    .flat_map(|node| node.inputs.values_mut())
                    .chain(graph.outputs.values_mut())
                    .for_each(split);
            }
            graph.nodes.extend(added);
        }
        if !changed {
            break;
        }
    }
    // The copies, and the copies only they called, are unreferenced now.
    loop {
        let referenced: BTreeSet<String> = definitions
            .values()
            .filter_map(|definition| match &definition.body {
                Body::Graph(graph) => Some(graph.nodes.values()),
                Body::Primitive(_) => None,
            })
            .flatten()
            .map(|node| node.definition.clone())
            .chain(clips.values().map(|clip| clip.graph.clone()))
            .collect();
        let before = definitions.len();
        definitions.retain(|id, _| referenced.contains(id) || !disposable.contains(id));
        if definitions.len() == before {
            break;
        }
    }
    (copies, unused)
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
                            definition: "mask_color".into(),
                            inputs: BTreeMap::from([
                                ("color".into(), color),
                                ("mask".into(), dimmer),
                            ]),
                        },
                    );
                    node.inputs.insert(
                        "color".into(),
                        Binding::Connection {
                            node: brightness,
                            output: "color".into(),
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
