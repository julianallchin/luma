//! Millisecond distributions, and the one place a percentile is defined.
//!
//! Every acceptance number this crate publishes — GPU pass timings, CPU encode
//! spans, the presentation interval — is a distribution rather than a mean,
//! because the thing a show notices is the worst frame and an average cannot
//! express one. Having a single definition of "p95" matters more than which
//! definition it is: two nearest-rank conventions differing by one sample look
//! like a regression when the code has not changed.

/// Nearest-rank summary of a set of millisecond samples.
///
/// `max_ms` is not decoration. p95 over 600 samples hides the worst thirty, and
/// a dropped frame is exactly what hides there.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct MetricSummary {
    /// Median sample.
    pub p50_ms: f64,
    /// 95th percentile, nearest-rank.
    pub p95_ms: f64,
    /// Worst sample observed.
    pub max_ms: f64,
}

impl MetricSummary {
    /// Summarise `samples`, or `None` when there were none.
    ///
    /// `None` rather than a zeroed summary: no samples and a run of perfectly
    /// free frames are different facts, and a caller that prints one as the
    /// other is publishing a number nothing measured.
    #[must_use]
    pub fn of(samples: impl IntoIterator<Item = f64>) -> Option<Self> {
        let mut samples: Vec<f64> = samples.into_iter().collect();
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(f64::total_cmp);
        let rank = |quantile: f64| {
            ((quantile * samples.len() as f64).ceil() as usize)
                .saturating_sub(1)
                .min(samples.len() - 1)
        };
        Some(Self {
            p50_ms: samples[rank(0.50)],
            p95_ms: samples[rank(0.95)],
            max_ms: samples[samples.len() - 1],
        })
    }
}

#[cfg(test)]
// Nearest rank returns a sample unmodified rather than interpolating between
// two, so these comparisons are exact by construction. An epsilon here would
// hide the one bug the tests exist to catch: an off-by-one in the rank.
#[allow(clippy::float_cmp)]
mod tests {
    use super::MetricSummary;

    #[test]
    fn nearest_rank_picks_a_real_sample_and_never_interpolates() {
        let summary = MetricSummary::of((1..=100).map(f64::from)).expect("100 samples");
        assert_eq!(summary.p50_ms, 50.0);
        assert_eq!(summary.p95_ms, 95.0);
        assert_eq!(summary.max_ms, 100.0);
    }

    #[test]
    fn one_sample_is_its_own_every_percentile_and_none_is_none() {
        let one = MetricSummary::of([7.5]).expect("one sample");
        assert_eq!((one.p50_ms, one.p95_ms, one.max_ms), (7.5, 7.5, 7.5));
        assert!(MetricSummary::of(std::iter::empty()).is_none());
    }
}
