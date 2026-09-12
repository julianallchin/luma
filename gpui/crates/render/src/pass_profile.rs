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
}

impl<'a> PassQueries<'a> {
    pub fn new(queries: Option<&'a wgpu::QuerySet>) -> Self {
        Self {
            queries,
            count: 10,
            spans: Vec::new(),
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
