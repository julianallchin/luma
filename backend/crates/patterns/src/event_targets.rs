//! Immutable event addressing. Explicit weights accompany recorded timestamps;
//! seeded subsets also address unbounded periodic streams. Queries materialize
//! only the event columns they need, with no playback state or clip-sized table.
use crate::*;
use ndarray::Array2;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TargetData", into = "TargetData")]
pub struct EventTargets(Arc<TargetData>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TargetData {
    Weights {
        fixtures: Arc<[String]>,
        weights: Array2<f64>,
    },
    Random {
        fixtures: Arc<[String]>,
        proportion: f64,
        seed: u64,
        #[serde(default)]
        cycle: bool,
    },
}
impl EventTargets {
    pub fn weights(fixtures: Vec<String>, weights: Array2<f64>) -> Result<Self> {
        TargetData::Weights {
            fixtures: fixtures.into(),
            weights,
        }
        .try_into()
    }
    /// Independent event draws, or successive windows of a seeded shuffled
    /// order. A cycle minimizes adjacent overlap for a stable eligible domain.
    pub fn random(fixtures: Vec<String>, proportion: f64, seed: u64, cycle: bool) -> Result<Self> {
        TargetData::Random {
            fixtures: fixtures.into(),
            proportion,
            seed,
            cycle,
        }
        .try_into()
    }
    pub fn fixtures(&self) -> &[String] {
        match &*self.0 {
            TargetData::Weights { fixtures, .. } | TargetData::Random { fixtures, .. } => fixtures,
        }
    }
    pub(crate) fn validate_source(&self, events: &Events) -> Result<()> {
        if let TargetData::Weights { weights, .. } = &*self.0 {
            let Events::Beats { times } = events.schedule() else {
                return Err(Error(
                    "explicit event weights require recorded timestamps".into(),
                ));
            };
            if weights.ncols() != times.as_slice().len() {
                return Err(Error(
                    "event weight columns must match event timestamps".into(),
                ));
            }
        }
        Ok(())
    }
}
impl TryFrom<TargetData> for EventTargets {
    type Error = Error;
    fn try_from(value: TargetData) -> Result<Self> {
        let result = Self(Arc::new(value));
        let domain = result.fixtures();
        if domain.iter().any(String::is_empty)
            || domain.iter().collect::<BTreeSet<_>>().len() != domain.len()
        {
            return Err(Error(
                "event targets require unique, nonempty head identities".into(),
            ));
        }
        match &*result.0 {
            TargetData::Weights { weights, .. } => {
                if weights.nrows() != domain.len()
                    || weights.len() > 16_777_216
                    || weights
                        .iter()
                        .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
                {
                    return Err(Error(
                        "event weights require one row per head and finite weights in 0–1".into(),
                    ));
                }
            }
            TargetData::Random { proportion, .. } => {
                if !proportion.is_finite() || !(0. ..=1.).contains(proportion) {
                    return Err(Error("selected proportion must be in 0–1".into()));
                }
            }
        }
        Ok(result)
    }
}
impl From<EventTargets> for TargetData {
    fn from(value: EventTargets) -> Self {
        (*value.0).clone()
    }
}

/// Columns are distinct queried event identities. A second selector filters
/// eligible heads from the first. Counts round to the nearest whole head.
pub(crate) fn weights(
    events: &Events,
    fixtures: &[String],
    indices: &[i64],
) -> Result<Option<Array2<f64>>> {
    let mut layers = Vec::new();
    let mut source = events;
    while let Events::Targeted { events, targets } = source {
        layers.push(targets);
        source = events;
    }
    if layers.is_empty() {
        return Ok(None);
    }
    if fixtures
        .len()
        .checked_mul(indices.len())
        .is_none_or(|n| n > 16_777_216)
    {
        return Err(Error(
            "event target tensor exceeds 16,777,216 elements".into(),
        ));
    }
    let mut result = Array2::ones((fixtures.len(), indices.len()));
    for targets in layers.into_iter().rev() {
        let domain: BTreeMap<_, _> = targets
            .fixtures()
            .iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        if fixtures.len() != domain.len() || fixtures.iter().any(|id| !domain.contains_key(id)) {
            return Err(Error(
                "event targets and signal fixture domains differ".into(),
            ));
        }
        match &*targets.0 {
            TargetData::Weights { weights, .. } => {
                for ((n, e), value) in result.indexed_iter_mut() {
                    *value *= usize::try_from(indices[e])
                        .ok()
                        .and_then(|e| weights.get((domain[&fixtures[n]], e)))
                        .copied()
                        .unwrap_or(0.);
                }
            }
            TargetData::Random {
                proportion,
                seed,
                cycle,
                ..
            } => {
                for (column, event) in indices.iter().enumerate() {
                    let key = crate::spatial::epoch_seed(*seed, if *cycle { 0 } else { *event });
                    let mut ranked: Vec<_> = fixtures
                        .iter()
                        .enumerate()
                        .filter(|(n, _)| result[[*n, column]] > 0.)
                        .map(|(n, id)| (n, crate::spatial::threshold(id, key)))
                        .collect();
                    ranked.sort_by(|(a, x), (b, y)| {
                        x.total_cmp(y).then_with(|| fixtures[*a].cmp(&fixtures[*b]))
                    });
                    let count = (proportion * ranked.len() as f64).round() as usize;
                    if *cycle && !ranked.is_empty() {
                        let offset = (i128::from(*event) * count as i128)
                            .rem_euclid(ranked.len() as i128)
                            as usize;
                        ranked.rotate_left(offset);
                    }
                    for (n, _) in ranked.into_iter().skip(count) {
                        result[[n, column]] = 0.;
                    }
                }
            }
        }
    }
    Ok(Some(result))
}

pub(crate) fn definition() -> Definition {
    let fixed = |name: &str, value: Value| Input {
        optional: false,
        name: name.into(),
        description: String::new(),
        value_type: value.value_type(),
        rate: Rate::Fixed,
        default: Some(value),
        author: None,
    };
    Definition {
        name: "Random subset".into(),
        inputs: BTreeMap::from([
            (
                "events".into(),
                fixed(
                    "Events",
                    Value::Events(Events::Periodic {
                        repeat: 1.,
                        grid_aligned: false,
                        delay: 0.,
                    }),
                ),
            ),
            (
                "proportion".into(),
                fixed("Proportion", Value::Proportion(0.25)),
            ),
            ("seed".into(), fixed("Seed", Value::Seed(0))),
            ("cycle".into(), Input {
                description: "Cycle through a seeded shuffled order. Minimizes consecutive overlap when the eligible selection stays the same.".into(),
                ..fixed("Shuffled cycle", Value::Boolean(false))
            }),
        ]),
        outputs: BTreeMap::from([(
            "events".into(),
            Output {
                value_type: ValueType::Events,
                rate: Rate::Fixed,
            },
        )]),
        body: Body::Primitive(Primitive::RandomEventTargets),
    }
}
