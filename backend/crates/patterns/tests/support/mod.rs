#![allow(dead_code)]
use luma_patterns::*;
use std::collections::BTreeMap;

pub fn field(value: &Value) -> BTreeMap<String, f64> {
    let Value::Signal(signal) = value else {
        panic!("expected a tensor, got {value:?}")
    };
    assert_eq!(*signal.channels(), Channels::Value);
    assert_eq!(signal.values().dim().1, 1);
    signal
        .fixtures()
        .expect("fixture axis")
        .iter()
        .enumerate()
        .map(|(n, id)| (id.clone(), signal.values()[[n, 0, 0]]))
        .collect()
}

/// Test numerical recipes at their explicit Output boundary, just as insertion
/// does. The reusable recipe's own outputs remain tensors.
pub fn prepare_effect(
    library: &Library,
    id: &str,
    args: &BTreeMap<String, Value>,
    frame: Frame<'_>,
) -> Result<PreparedGraph> {
    let definition = &library.definitions[id];
    if definition.placeable() && !definition.playable() {
        let mut library = library.clone();
        library
            .definitions
            .insert("test/terminal".into(), definition.clip_instance(id)?);
        PreparedGraph::new(&library, "test/terminal", args, frame)
    } else {
        PreparedGraph::new(library, id, args, frame)
    }
}

pub trait EvaluateEffect {
    fn evaluate_effect(
        &self,
        id: &str,
        args: &BTreeMap<String, Value>,
        frame: Frame<'_>,
    ) -> Result<BTreeMap<String, Value>>;
}
impl EvaluateEffect for Library {
    fn evaluate_effect(
        &self,
        id: &str,
        args: &BTreeMap<String, Value>,
        frame: Frame<'_>,
    ) -> Result<BTreeMap<String, Value>> {
        prepare_effect(self, id, args, frame)?.evaluate(frame.beat)
    }
}
