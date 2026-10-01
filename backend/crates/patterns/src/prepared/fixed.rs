use super::*;

impl PreparedGraph {
    /// Bakes every value that does not change over the clip, in topological
    /// order: constants, folded kernels and clock tables. Rebinding features
    /// rebuilds every value.
    pub(super) fn prepare_fixed(
        &self,
        features: Option<&dyn crate::FeatureSource>,
    ) -> Result<Vec<Option<EvaluatedValue>>> {
        let mut baked = vec![None; self.slots];
        for (index, step) in self.steps.iter().enumerate() {
            let values =
                if step.primitive == Primitive::Kernel(crate::clip_graph::Kernel::ClockTable) {
                    self.prepare_clock(index, features, &baked)?
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
                    self.run_step(step, &[self.clip_start], features, &baked)?
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

    fn prepare_clock(
        &self,
        index: usize,
        features: Option<&dyn crate::FeatureSource>,
        baked: &[Option<EvaluatedValue>],
    ) -> Result<BTreeMap<String, EvaluatedValue>> {
        let step = &self.steps[index];
        let source = &step.inputs["period"];
        let dependencies = self.live_steps(index, baked, [source]);
        let beats: Vec<_> = if dependencies.is_empty() {
            vec![self.clip_start]
        } else {
            (0..crate::clip_graph::clock_table::SAMPLES)
                .map(|i| {
                    self.clip_start
                        + self.clip_duration * i as f64
                            / (crate::clip_graph::clock_table::SAMPLES - 1) as f64
                })
                .collect()
        };
        let mut slots = baked.to_vec();
        for parent in dependencies {
            let step = &self.steps[parent];
            for (key, value) in self.run_step(step, &beats, features, &slots)? {
                slots[step.outputs[&key]] = Some(value);
            }
        }
        self.run_step(step, &beats, features, &slots)
    }
}
