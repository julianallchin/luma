//! Opt-in pass brackets. These overlap on Metal and must never be summed as
//! exclusive GPU costs. Keep the existing completion-cut frame totals too.
/// A pass's stage bracket relative to the first recorded GPU timestamp.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GpuPassTiming {
    /// Command encoder label; repeated passes have separate records.
    pub name: &'static str,
    /// Beginning of the pass, including overlap with earlier passes.
    pub start_ms: f64,
    /// Completion of the pass; the bracket includes dependency waits.
    pub end_ms: f64,
}

pub(crate) const QUERY_CAPACITY: u32 = 2048;

pub(crate) struct PassQueries<'a> {
    queries: Option<&'a wgpu::QuerySet>,
    pub count: u32,
    pub spans: Vec<(&'static str, u32, u32)>,
    /// `(base, sample)`: a base sample a profiled compute pass wrote at a
    /// sample of its own bracket instead (see [`Self::compute`]). The
    /// readback copies `sample` into `base`.
    pub moved: Vec<(u32, u32)>,
}

impl<'a> PassQueries<'a> {
    pub fn new(queries: Option<&'a wgpu::QuerySet>) -> Self {
        Self {
            queries,
            count: 10,
            spans: Vec::new(),
            moved: Vec::new(),
        }
    }

    pub fn render(
        &mut self,
        name: &'static str,
        original: Option<wgpu::RenderPassTimestampWrites<'a>>,
    ) -> Option<wgpu::RenderPassTimestampWrites<'a>> {
        let Some(queries) = self.queries else {
            return original;
        };
        let mut index = || {
            assert!(self.count < QUERY_CAPACITY, "too many profiled passes");
            let i = self.count;
            self.count += 1;
            i
        };
        let begin = original
            .as_ref()
            .and_then(|p| p.beginning_of_pass_write_index)
            .unwrap_or_else(&mut index);
        let end = original
            .as_ref()
            .and_then(|p| p.end_of_pass_write_index)
            .unwrap_or_else(&mut index);
        // A base sample written in place supersedes an earlier pass's move.
        self.moved
            .retain(|(base, _)| *base != begin && *base != end);
        self.spans.push((name, begin, end));
        Some(wgpu::RenderPassTimestampWrites {
            query_set: queries,
            beginning_of_pass_write_index: Some(begin),
            end_of_pass_write_index: Some(end),
        })
    }

    pub fn compute(
        &mut self,
        name: &'static str,
        original: Option<wgpu::ComputePassTimestampWrites<'a>>,
    ) -> Option<wgpu::ComputePassTimestampWrites<'a>> {
        if self.queries.is_none() {
            return original;
        }
        // wgpu on Vulkan resets every query from a compute pass's begin index
        // through its end index. A bracket from a new index to a fixed base
        // index would reset every sample written between them, and the resolve
        // then waits for ever (the GPU faults). So a compute pass that writes
        // one base sample gets a fresh bracket of two adjacent samples, and
        // the base sample is read from the matching end of it.
        if let Some(p) = original.as_ref().filter(|p| {
            p.beginning_of_pass_write_index.is_none() || p.end_of_pass_write_index.is_none()
        }) {
            let base = p
                .beginning_of_pass_write_index
                .or(p.end_of_pass_write_index)
                .expect("a pass writes at least one sample");
            let at_begin = p.beginning_of_pass_write_index.is_some();
            let fresh = self.compute(name, None)?;
            let (begin, end) = (
                fresh
                    .beginning_of_pass_write_index
                    .expect("a fresh bracket"),
                fresh.end_of_pass_write_index.expect("a fresh bracket"),
            );
            self.moved.push((base, if at_begin { begin } else { end }));
            return Some(fresh);
        }
        self.render(
            name,
            original.map(|p| wgpu::RenderPassTimestampWrites {
                query_set: p.query_set,
                beginning_of_pass_write_index: p.beginning_of_pass_write_index,
                end_of_pass_write_index: p.end_of_pass_write_index,
            }),
        )
        .map(|p| wgpu::ComputePassTimestampWrites {
            query_set: p.query_set,
            beginning_of_pass_write_index: p.beginning_of_pass_write_index,
            end_of_pass_write_index: p.end_of_pass_write_index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::PassQueries;
    use crate::device::DeviceContext;

    #[test]
    fn a_compute_pass_writing_one_base_sample_still_gets_a_span() {
        let context = DeviceContext::shared().unwrap();
        if !context
            .device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
        {
            return;
        }
        let queries = context.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: super::QUERY_CAPACITY,
        });
        let base_end = |index| wgpu::ComputePassTimestampWrites {
            query_set: &queries,
            beginning_of_pass_write_index: None,
            end_of_pass_write_index: Some(index),
        };
        let mut profile = PassQueries::new(Some(&queries));
        profile.compute("earlier", None);
        let writes = profile.compute("haze", Some(base_end(2))).unwrap();
        let (begin, end) = (
            writes.beginning_of_pass_write_index.unwrap(),
            writes.end_of_pass_write_index.unwrap(),
        );
        // Its own adjacent pair, past every sample already handed out, so a
        // compute pass's begin-to-end reset clears nothing else.
        assert_eq!(end, begin + 1);
        assert!(profile
            .spans
            .iter()
            .all(|&(name, b, e)| name == "haze" || (b < begin && e < begin)));
        assert!(profile.spans.contains(&("haze", begin, end)));
        // The base sample is read from the end of that pair...
        assert_eq!(profile.moved, vec![(2, end)]);
        // ...until a later pass writes the base sample itself.
        profile.render(
            "composite",
            Some(wgpu::RenderPassTimestampWrites {
                query_set: &queries,
                beginning_of_pass_write_index: None,
                end_of_pass_write_index: Some(2),
            }),
        );
        assert!(profile.moved.is_empty());
    }
}
