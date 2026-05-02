//! Generic topological sort over an arbitrary node type.
//!
//! Two callers: `sync::registry` orders tables for FK-safe push/pull;
//! `preprocessing::scheduler` orders DAG nodes for layered execution.
//! Both peel layers via Kahn's algorithm — the only difference is whether
//! you want a flat order or grouped layers, which is what [`flat`] vs
//! [`layers`] return.
//!
//! Cycles or unknown parents panic with the offending nodes named. Both are
//! programming errors caught at registry-construction time, not runtime
//! conditions.

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;

/// Topologically sort `nodes` into a flat order: parents before children.
pub fn flat<'a, T, K>(
    nodes: &'a [T],
    key: impl Fn(&T) -> K,
    parents: impl Fn(&T) -> Vec<K>,
) -> Vec<&'a T>
where
    K: Eq + Hash + Copy + Debug,
{
    layers(nodes, key, parents).into_iter().flatten().collect()
}

/// Topologically sort `nodes` into parallel layers: every node in `layers[i]`
/// has all its parents in `layers[0..i]`. Within a layer, nodes appear in
/// declaration order so the result is stable.
pub fn layers<'a, T, K>(
    nodes: &'a [T],
    key: impl Fn(&T) -> K,
    parents: impl Fn(&T) -> Vec<K>,
) -> Vec<Vec<&'a T>>
where
    K: Eq + Hash + Copy + Debug,
{
    let known: HashSet<K> = nodes.iter().map(&key).collect();
    for n in nodes {
        for parent in parents(n) {
            assert!(
                known.contains(&parent),
                "Topological sort: node {:?} declares unknown parent {:?}",
                key(n),
                parent
            );
        }
    }

    let mut remaining: HashMap<K, HashSet<K>> = nodes
        .iter()
        .map(|n| (key(n), parents(n).into_iter().collect()))
        .collect();
    let mut out: Vec<Vec<&'a T>> = Vec::new();

    while !remaining.is_empty() {
        let layer: Vec<&'a T> = nodes
            .iter()
            .filter(|n| remaining.get(&key(n)).is_some_and(|d| d.is_empty()))
            .collect();
        assert!(
            !layer.is_empty(),
            "Cycle in topological sort; unresolved: {:?}",
            remaining.keys().collect::<Vec<_>>()
        );
        let layer_keys: Vec<K> = layer.iter().map(|n| key(n)).collect();
        for k in &layer_keys {
            remaining.remove(k);
        }
        for deps in remaining.values_mut() {
            for k in &layer_keys {
                deps.remove(k);
            }
        }
        out.push(layer);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct N {
        name: &'static str,
        parents: Vec<&'static str>,
    }
    fn n(name: &'static str, parents: &[&'static str]) -> N {
        N {
            name,
            parents: parents.to_vec(),
        }
    }

    #[test]
    fn flat_orders_parents_before_children() {
        let nodes = vec![n("c", &["b"]), n("a", &[]), n("b", &["a"])];
        let out = flat(&nodes, |n| n.name, |n| n.parents.clone());
        assert_eq!(
            out.iter().map(|n| n.name).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn layers_groups_parallel_siblings() {
        let nodes = vec![
            n("root", &[]),
            n("left", &["root"]),
            n("right", &["root"]),
            n("leaf", &["left", "right"]),
        ];
        let out = layers(&nodes, |n| n.name, |n| n.parents.clone());
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0].iter().map(|n| n.name).collect::<Vec<_>>(),
            vec!["root"]
        );
        assert_eq!(
            out[1].iter().map(|n| n.name).collect::<Vec<_>>(),
            vec!["left", "right"]
        );
        assert_eq!(
            out[2].iter().map(|n| n.name).collect::<Vec<_>>(),
            vec!["leaf"]
        );
    }

    #[test]
    #[should_panic(expected = "Cycle")]
    fn cycle_panics() {
        let nodes = vec![n("a", &["b"]), n("b", &["a"])];
        let _ = flat(&nodes, |n| n.name, |n| n.parents.clone());
    }

    #[test]
    #[should_panic(expected = "unknown parent")]
    fn unknown_parent_panics() {
        let nodes = vec![n("a", &["ghost"])];
        let _ = flat(&nodes, |n| n.name, |n| n.parents.clone());
    }
}
