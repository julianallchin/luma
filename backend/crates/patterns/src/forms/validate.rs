//! Source validation happens before graph lowering.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};
fn children(value: &Value) -> Vec<(&str, &Value)> {
    match value {
        Value::Time(t) => {
            let mut out = vec![("phase", t.phase.as_ref()), ("gain", t.gain.as_ref())];
            if let Some(Events::Own(e)) = &t.events {
                out.push(("every", &e.every));
                if let Some(life) = &e.life {
                    out.push(("life", life));
                }
            }
            out
        }
        Value::Space(s) => {
            let mut out = vec![("width", s.width.as_ref()), ("gain", s.gain.as_ref())];
            if let Some(offset) = &s.offset {
                out.push(("offset", offset));
            }
            out
        }
        Value::Noise(n) => {
            let mut out = vec![
                ("speed", n.speed.as_ref()),
                ("contrast", n.contrast.as_ref()),
                ("low", n.range[0].as_ref()),
                ("high", n.range[1].as_ref()),
            ];
            if let Some(scale) = &n.scale {
                out.push(("scale", scale));
            }
            out
        }
        Value::Random(r) => {
            let mut out = vec![
                ("coverage", r.coverage.as_ref()),
                ("level", r.level.as_ref()),
            ];
            if let Some(Events::Own(e)) = &r.events {
                out.push(("every", &e.every));
                if let Some(life) = &e.life {
                    out.push(("life", life));
                }
            }
            out
        }
        Value::Audio(a) => vec![("gain", a.gain.as_ref())],
        _ => vec![],
    }
}
fn events(value: &Value) -> Option<&Events> {
    match value {
        Value::Time(t) => t.events.as_ref(),
        Value::Random(r) => r.events.as_ref(),
        _ => None,
    }
}
fn owner<'a>(
    path: String,
    value: &'a Value,
    inputs: &'a BTreeMap<String, Value>,
    seen: &mut BTreeSet<String>,
) -> Result<(String, &'a Value)> {
    if !seen.insert(path.clone()) {
        return Err(Error(format!("event reference cycle at {path}")));
    }
    if let Some(Events::SameAs { same_as }) = events(value) {
        let source = inputs
            .get(same_as)
            .ok_or_else(|| Error(format!("unknown event input {same_as}")))?;
        return owner(same_as.clone(), source, inputs, seen);
    }
    if let Value::Space(space) = value {
        if let Some(offset) = &space.offset {
            return owner(format!("{path}/offset"), offset, inputs, seen);
        }
    }
    if !matches!(value, Value::Time(_) | Value::Random(_)) {
        return Err(Error(format!("{path} has no event clock")));
    }
    Ok((path, value))
}
pub(super) fn validate(
    value: &Value,
    inputs: &BTreeMap<String, Value>,
    path: &str,
    depth: usize,
) -> Result<()> {
    if depth > 24 {
        return Err(Error("sources may nest at most 24 levels".into()));
    }
    value.validate()?;
    match value {
        Value::Time(t) => {
            bounds(&t.phase)?;
            bounds(&t.gain)?;
        }
        Value::Space(s) => {
            bounded(&s.width, 0., super::MAX_WIDTH, "width")?;
            bounds(&s.gain)?;
            if s.axis.reverse || s.axis.per_group {
                return Err(Error(
                    "an axis has no reverse or per_group; choose a backward curve or group span"
                        .into(),
                ));
            }
            if let Some(offset) = &s.offset {
                bounds(offset)?;
            }
        }
        Value::Random(r) => {
            bounded(&r.coverage, 0., 1., "coverage")?;
            bounds(&r.level)?;
        }
        Value::Noise(n) => {
            let (low, _) = bounds(&n.speed)?;
            if low <= 0. {
                return Err(Error("noise speed must stay positive".into()));
            }
            bounded(&n.contrast, 0., 1., "contrast")?;
            bounds(&n.range[0])?;
            bounds(&n.range[1])?;
            if let Some(scale) = &n.scale {
                bounded(scale, 0., f64::INFINITY, "scale")?;
            }
        }
        Value::Audio(a) => {
            bounds(&a.gain)?;
        }
        _ => {}
    }
    if let Some(Events::Own(clock)) = events(value) {
        for (name, period) in [
            ("every", Some(clock.every.as_ref())),
            ("life", clock.life.as_deref()),
        ] {
            if let Some(period) = period {
                let (low, _) = bounds(period)?;
                if low < 0. || (low == 0. && period.scalar_value() != Some(0.)) {
                    return Err(Error(format!("{name}: a period source must stay positive")));
                }
            }
        }
    }
    if let Some(Events::SameAs { .. }) = events(value) {
        owner(path.into(), value, inputs, &mut BTreeSet::new())?;
    }
    for (name, child) in children(value) {
        validate(child, inputs, &format!("{path}/{name}"), depth + 1)?;
    }
    Ok(())
}
/// Check clock references without needing a venue or track analysis. A period
/// source cannot read the event clock that it is itself computing.
pub(super) fn validate_clock_dependencies(inputs: &BTreeMap<String, Value>) -> Result<()> {
    fn clock(
        path: String,
        value: &Value,
        inputs: &BTreeMap<String, Value>,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<()> {
        if done.contains(&path) {
            return Ok(());
        }
        if !active.insert(path.clone()) {
            return Err(Error(format!("clock dependency cycle at {path}")));
        }
        sample(&path, value, inputs, active, done)?;
        active.remove(&path);
        done.insert(path);
        Ok(())
    }
    fn sample(
        path: &str,
        value: &Value,
        inputs: &BTreeMap<String, Value>,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<()> {
        if matches!(value, Value::Time(_) | Value::Random(_)) {
            let (name, leader) = owner(path.into(), value, inputs, &mut BTreeSet::new())?;
            if let Some(Events::Own(events)) = events(leader) {
                clock(format!("{name}/every"), &events.every, inputs, active, done)?;
                if let Some(life) = &events.life {
                    clock(format!("{name}/life"), life, inputs, active, done)?;
                }
            }
        }
        if let Value::Noise(noise) = value {
            clock(format!("{path}/speed"), &noise.speed, inputs, active, done)?;
        }
        for (name, child) in children(value) {
            if !matches!(name, "every" | "life" | "speed") {
                sample(&format!("{path}/{name}"), child, inputs, active, done)?;
            }
        }
        Ok(())
    }
    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    for (name, value) in inputs {
        sample(name, value, inputs, &mut active, &mut done)?;
    }
    Ok(())
}

/// Bounds and component checks belong to the source tree, so the UI, Python
/// and direct score writes all enforce the same numeric contract.
pub(super) fn bounds(value: &Value) -> Result<(f64, f64)> {
    if let Some(v) = value.scalar_value() {
        return Ok((v, v));
    }
    let product = |(a, b): (f64, f64), (c, d): (f64, f64)| {
        let values = [a * c, a * d, b * c, b * d];
        (
            values.into_iter().fold(f64::INFINITY, f64::min),
            values.into_iter().fold(f64::NEG_INFINITY, f64::max),
        )
    };
    match value {
        Value::Time(t) => {
            if t.curve.is_color() {
                return Err(Error("a numeric input needs a number curve".into()));
            }
            let values: Vec<_> = t.curve.values().collect();
            Ok(product(
                (
                    values.iter().copied().fold(f64::INFINITY, f64::min),
                    values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                ),
                bounds(&t.gain)?,
            ))
        }
        Value::Space(s) => {
            let c = s
                .curve
                .as_ref()
                .ok_or_else(|| Error("a number input needs a spatial curve".into()))?;
            Ok(product(
                (
                    c.points
                        .iter()
                        .map(|p| p.value)
                        .fold(f64::INFINITY, f64::min),
                    c.points
                        .iter()
                        .map(|p| p.value)
                        .fold(f64::NEG_INFINITY, f64::max),
                ),
                bounds(&s.gain)?,
            ))
        }
        Value::Random(r) => {
            let (a, b) = bounds(&r.level)?;
            if bounds(&r.coverage)? == (1., 1.) {
                Ok((a, b))
            } else {
                Ok((a.min(0.), b.max(0.)))
            }
        }
        Value::Noise(n) => {
            let (a, b) = bounds(&n.range[0])?;
            let (c, d) = bounds(&n.range[1])?;
            Ok((a.min(c), b.max(d)))
        }
        Value::Audio(a) => Ok(product(
            (if a.threshold == 0. { a.floor } else { 0. }, 1.),
            bounds(&a.gain)?,
        )),
        _ => Err(Error("expected a number or numeric source".into())),
    }
}
fn bounded(value: &Value, low: f64, high: f64, name: &str) -> Result<()> {
    let (a, b) = bounds(value)?;
    if a < low || b > high {
        return Err(Error(format!("{name}: values must stay in {low}..{high}")));
    }
    Ok(())
}
