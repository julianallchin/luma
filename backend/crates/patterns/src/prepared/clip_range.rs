use super::*;

impl PreparedGraph {
    /// Topological preparation means a range downstream of another range sees
    /// the completed upstream estimate. Rebinding features rebuilds every value.
    pub(super) fn prepare_fixed(
        &self,
        features: Option<&dyn crate::FeatureSource>,
    ) -> Result<Vec<Option<EvaluatedValue>>> {
        let mut baked = vec![None; self.slots];
        let clock = crate::Signal::scalar(self.clip_start, crate::Unit::Number)?;
        for (index, step) in self.steps.iter().enumerate() {
            let values = if step.primitive == Primitive::ClipRange {
                self.prepare_range(index, features, &baked)?
            } else if (!step.primitive.reads_time()
                || step
                    .output_types
                    .values()
                    .any(|output| output.rate == crate::Rate::Fixed))
                && step.inputs.values().all(|source| match source {
                    Source::Constant(_) => true,
                    Source::Slot(index) => baked[*index].is_some(),
                })
            {
                self.run_step(step, &[self.clip_start], &clock, features, &baked)?
            } else {
                continue;
            };
            for (key, value) in values {
                if !step.primitive.reads_time()
                    || step.output_types[&key].rate == crate::Rate::Fixed
                {
                    baked[step.outputs[&key]] = Some(value);
                }
            }
        }
        Ok(baked)
    }

    fn prepare_range(
        &self,
        index: usize,
        features: Option<&dyn crate::FeatureSource>,
        baked: &[Option<EvaluatedValue>],
    ) -> Result<BTreeMap<String, EvaluatedValue>> {
        let step = &self.steps[index];
        let count =
            crate::clip_range::sample_count(step.inputs["samples"].read(baked)?.fixed_scalar()?)?;
        let source = &step.inputs["value"];
        // Sample only this input's dependencies. An unrelated output must not
        // be evaluated here, especially one that needs additional analysis.
        let dependencies = self.live_steps(index, baked, [source]);
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        let mut unit = None;
        // Bound temporary tensors independently of the requested resolution.
        for first in (0..count).step_by(128) {
            let beats: Vec<_> = (first..(first + 128).min(count))
                .map(|i| self.clip_start + self.clip_duration * i as f64 / (count - 1) as f64)
                .collect();
            let clock = crate::Signal::series(&beats, crate::Unit::Number)?;
            let mut slots = baked.to_vec();
            for parent in &dependencies {
                let step = &self.steps[*parent];
                let values = self.run_step(step, &beats, &clock, features, &slots)?;
                for (key, value) in values {
                    slots[step.outputs[&key]] = Some(value);
                }
            }
            let value = source.read(&slots)?;
            let signal = value.numeric();
            if unit.is_some_and(|unit| unit != signal.unit()) {
                return Err(Error("range input changed units across the clip".into()));
            }
            unit = Some(signal.unit());
            let (lo, hi) = crate::clip_range::bounds(signal);
            minimum = minimum.min(lo);
            maximum = maximum.max(hi);
        }
        crate::clip_range::result(unit.unwrap(), minimum, maximum)
            .map_err(|e| Error(format!("{}: {e}", step.label)))
    }
}
