//! Summary statistics, outlier classification and the Mann-Whitney U test.

use serde::{Deserialize, Serialize};

/// How many samples fall outside Tukey's fences (1.5 and 3 interquartile ranges).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outliers {
    pub low_severe: usize,
    pub low_mild: usize,
    pub high_mild: usize,
    pub high_severe: usize,
}

impl Outliers {
    pub fn total(&self) -> usize {
        self.low_severe + self.low_mild + self.high_mild + self.high_severe
    }
}

/// What to do with outliers before the summary is computed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutlierPolicy {
    /// Keep every sample and only report the counts.
    #[default]
    Keep,
    /// Drop mild and severe outliers (outside 1.5 IQR).
    Trim,
    /// Drop only severe outliers (outside 3 IQR).
    TrimSevere,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Number of samples the summary is computed from (after any trimming).
    pub n: usize,
    pub mean: f64,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    /// Sample standard deviation (n - 1).
    pub stddev: f64,
    /// Median absolute deviation, unscaled.
    pub mad: f64,
    pub p1: f64,
    pub p5: f64,
    pub p25: f64,
    pub p75: f64,
    pub p95: f64,
    pub p99: f64,
    pub iqr: f64,
    /// Standard deviation divided by the mean.
    pub cv: f64,
    /// Outliers among the samples that were passed in, before any trimming.
    pub outliers: Outliers,
    /// How many samples the policy dropped.
    pub trimmed: usize,
}

/// The `p`-th percentile (0 to 100) of sorted data by linear interpolation between the closest
/// ranks, the same definition as NumPy's default.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    assert!(!sorted.is_empty(), "percentile of no data");
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p.clamp(0.0, 100.0) / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
}

fn sorted_copy(samples: &[f64]) -> Vec<f64> {
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

/// Counts outliers with Tukey's fences on the first and third quartile.
pub fn classify_outliers(sorted: &[f64]) -> Outliers {
    let (q1, q3) = (percentile(sorted, 25.0), percentile(sorted, 75.0));
    let iqr = q3 - q1;
    let (mild_lo, severe_lo) = (q1 - 1.5 * iqr, q1 - 3.0 * iqr);
    let (mild_hi, severe_hi) = (q3 + 1.5 * iqr, q3 + 3.0 * iqr);
    let mut o = Outliers::default();
    for &x in sorted {
        if x < severe_lo {
            o.low_severe += 1;
        } else if x < mild_lo {
            o.low_mild += 1;
        } else if x > severe_hi {
            o.high_severe += 1;
        } else if x > mild_hi {
            o.high_mild += 1;
        }
    }
    o
}

/// Applies `policy` and returns the samples that are kept, in their original order.
pub fn apply_policy(samples: &[f64], policy: OutlierPolicy) -> Vec<f64> {
    if policy == OutlierPolicy::Keep || samples.len() < 4 {
        return samples.to_vec();
    }
    let sorted = sorted_copy(samples);
    let (q1, q3) = (percentile(&sorted, 25.0), percentile(&sorted, 75.0));
    let k = if policy == OutlierPolicy::Trim { 1.5 } else { 3.0 };
    let (lo, hi) = (q1 - k * (q3 - q1), q3 + k * (q3 - q1));
    samples.iter().copied().filter(|&x| x >= lo && x <= hi).collect()
}

pub fn summarize(samples: &[f64], policy: OutlierPolicy) -> Summary {
    assert!(!samples.is_empty(), "summary of no samples");
    let outliers = classify_outliers(&sorted_copy(samples));
    let kept = apply_policy(samples, policy);
    let s = sorted_copy(&kept);
    let n = s.len();
    let mean = s.iter().sum::<f64>() / n as f64;
    let stddev = if n > 1 { (s.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1) as f64).sqrt() } else { 0.0 };
    let median = percentile(&s, 50.0);
    let mut dev: Vec<f64> = s.iter().map(|x| (x - median).abs()).collect();
    dev.sort_by(|a, b| a.total_cmp(b));
    let (p25, p75) = (percentile(&s, 25.0), percentile(&s, 75.0));
    Summary {
        n,
        mean,
        median,
        min: s[0],
        max: s[n - 1],
        stddev,
        mad: percentile(&dev, 50.0),
        p1: percentile(&s, 1.0),
        p5: percentile(&s, 5.0),
        p25,
        p75,
        p95: percentile(&s, 95.0),
        p99: percentile(&s, 99.0),
        iqr: p75 - p25,
        cv: if mean != 0.0 { stddev / mean } else { 0.0 },
        outliers,
        trimmed: samples.len() - n,
    }
}

/// Result of a two-sided Mann-Whitney U test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MannWhitney {
    /// U statistic of the first sample.
    pub u: f64,
    pub z: f64,
    pub p: f64,
}

/// Two-sided Mann-Whitney U test with the normal approximation, tie correction and continuity
/// correction (what `scipy.stats.mannwhitneyu(..., method="asymptotic")` computes). It asks
/// whether values from one sample tend to be larger than values from the other, without assuming
/// the timings are normally distributed.
pub fn mann_whitney(a: &[f64], b: &[f64]) -> MannWhitney {
    let (n1, n2) = (a.len() as f64, b.len() as f64);
    assert!(n1 > 0.0 && n2 > 0.0, "Mann-Whitney needs two non-empty samples");
    let mut all: Vec<(f64, bool)> = a.iter().map(|&x| (x, true)).chain(b.iter().map(|&x| (x, false))).collect();
    all.sort_by(|x, y| x.0.total_cmp(&y.0));
    let n = all.len();
    let mut rank_sum_a = 0.0;
    let mut tie_term = 0.0;
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && all[j + 1].0 == all[i].0 {
            j += 1;
        }
        let t = (j - i + 1) as f64;
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        rank_sum_a += all[i..=j].iter().filter(|x| x.1).count() as f64 * avg_rank;
        tie_term += t * t * t - t;
        i = j + 1;
    }
    let u1 = rank_sum_a - n1 * (n1 + 1.0) / 2.0;
    let mu = n1 * n2 / 2.0;
    let nn = n as f64;
    let var = n1 * n2 / 12.0 * ((nn + 1.0) - tie_term / (nn * (nn - 1.0)));
    if var <= 0.0 {
        // Every value is identical: nothing to distinguish the samples.
        return MannWhitney { u: u1, z: 0.0, p: 1.0 };
    }
    let diff = u1 - mu;
    // Continuity correction moves U half a unit toward the mean.
    let z = (diff.abs() - 0.5).max(0.0) / var.sqrt();
    let p = libm::erfc(z / std::f64::consts::SQRT_2).min(1.0);
    MannWhitney { u: u1, z: if diff < 0.0 { -z } else { z }, p }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_interpolates_like_numpy() {
        let d = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&d, 0.0), 1.0);
        assert_eq!(percentile(&d, 50.0), 2.5);
        assert_eq!(percentile(&d, 100.0), 4.0);
        assert!((percentile(&d, 25.0) - 1.75).abs() < 1e-12);
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
    }

    #[test]
    fn classifies_planted_outliers() {
        let mut v: Vec<f64> = (0..100).map(|i| 100.0 + (i % 10) as f64).collect();
        v.push(500.0); // far above: severe
        v.push(1.0); // far below: severe
        let o = classify_outliers(&sorted_copy(&v));
        assert_eq!((o.high_severe, o.low_severe), (1, 1));
        assert_eq!(o.total(), 2);
    }

    #[test]
    fn trim_policies_drop_the_right_samples() {
        let mut v: Vec<f64> = (0..50).map(|i| 100.0 + (i % 5) as f64).collect();
        v.push(500.0);
        assert_eq!(apply_policy(&v, OutlierPolicy::Keep).len(), 51);
        assert_eq!(apply_policy(&v, OutlierPolicy::Trim).len(), 50);
        assert_eq!(apply_policy(&v, OutlierPolicy::TrimSevere).len(), 50);
        let s = summarize(&v, OutlierPolicy::Trim);
        assert_eq!((s.n, s.trimmed, s.max), (50, 1, 104.0));
        assert_eq!(s.outliers.high_severe, 1);
    }

    #[test]
    fn identical_samples_are_not_different() {
        let a = [5.0; 20];
        let m = mann_whitney(&a, &a);
        assert_eq!(m.p, 1.0);
    }

    #[test]
    fn separated_samples_are_very_different() {
        let a: Vec<f64> = (0..30).map(|i| 100.0 + i as f64 * 0.1).collect();
        let b: Vec<f64> = (0..30).map(|i| 120.0 + i as f64 * 0.1).collect();
        let m = mann_whitney(&a, &b);
        assert!(m.p < 1e-9 && m.z < 0.0 && m.u == 0.0, "{m:?}");
    }
}
