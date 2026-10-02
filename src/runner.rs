//! Calibration and sampling.

use std::hint::black_box;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::stats::{summarize, OutlierPolicy, Summary};

/// How much work one call of the benchmark closure does, for rates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "per_iter", rename_all = "kebab-case")]
pub enum Throughput {
    /// Bytes processed per call; reported in GB/s (10^9 bytes).
    Bytes(u64),
    /// Operations per call (loads, additions, items); reported per operation and as a rate.
    Elements(u64),
}

/// Measurement settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Time spent running the closure before sampling, also used to size a sample.
    pub warmup_ms: u64,
    /// Target duration of one sample. The number of calls per sample is chosen to reach it.
    pub sample_time_ms: u64,
    /// Number of samples to take.
    pub samples: usize,
    /// Stop sampling after this long (but never before 10 samples) when each call is slow.
    pub max_time_ms: u64,
    pub outliers: OutlierPolicy,
}

impl Default for Config {
    fn default() -> Self {
        Config { warmup_ms: 200, sample_time_ms: 10, samples: 50, max_time_ms: 5000, outliers: OutlierPolicy::Keep }
    }
}

/// One measured benchmark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchResult {
    pub name: String,
    /// Calls of the closure inside one timed sample.
    pub iters_per_sample: u64,
    /// Time per call, in nanoseconds, for every sample taken (before any trimming).
    pub samples_ns: Vec<f64>,
    pub throughput: Option<Throughput>,
    pub summary: Summary,
}

impl BenchResult {
    /// Median nanoseconds per operation for `Elements` benchmarks.
    pub fn ns_per_op(&self) -> Option<f64> {
        match self.throughput {
            Some(Throughput::Elements(n)) if n > 0 => Some(self.summary.median / n as f64),
            _ => None,
        }
    }

    /// Median throughput: GB/s for `Bytes`, giga-operations per second for `Elements`.
    pub fn rate(&self) -> Option<f64> {
        match self.throughput {
            Some(Throughput::Bytes(n)) | Some(Throughput::Elements(n)) if self.summary.median > 0.0 => {
                Some(n as f64 / self.summary.median)
            }
            _ => None,
        }
    }
}

fn time_batch<R>(f: &mut impl FnMut() -> R, iters: u64) -> Duration {
    let start = Instant::now();
    for _ in 0..iters {
        black_box(f());
    }
    start.elapsed()
}

/// Average cost of one `Instant::now()` call, in nanoseconds.
pub fn timer_overhead_ns() -> f64 {
    const CALLS: u32 = 200_000;
    let start = Instant::now();
    for _ in 0..CALLS {
        black_box(Instant::now());
    }
    start.elapsed().as_nanos() as f64 / CALLS as f64
}

/// The step of the clock: the median gap between two different readings, in nanoseconds. A sample
/// much shorter than this cannot be timed, which is why samples are sized to milliseconds.
pub fn timer_resolution_ns() -> f64 {
    let mut gaps: Vec<f64> = (0..200)
        .map(|_| {
            let a = Instant::now();
            loop {
                let b = Instant::now();
                if b > a {
                    return b.duration_since(a).as_nanos() as f64;
                }
            }
        })
        .collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    gaps[gaps.len() / 2]
}

/// Runs `f` for the warm-up period and returns how many calls fit in one sample.
fn calibrate<R>(f: &mut impl FnMut() -> R, cfg: &Config) -> u64 {
    let warmup = Duration::from_millis(cfg.warmup_ms);
    let target = Duration::from_millis(cfg.sample_time_ms.max(1)).as_secs_f64();
    let start = Instant::now();
    let mut iters = 1u64;
    let mut per_call;
    loop {
        let t = time_batch(f, iters).as_secs_f64();
        per_call = t / iters as f64;
        if start.elapsed() >= warmup {
            break;
        }
        // Grow the batch until it is long enough to time reliably, then stay near the target.
        iters = if t < target / 4.0 { iters.saturating_mul(2) } else { ((target / per_call.max(1e-12)) as u64).max(1) };
    }
    ((target / per_call.max(1e-12)).round() as u64).clamp(1, 1 << 40)
}

/// Measures `f`: warm-up, calibration, then `cfg.samples` timed samples.
pub fn measure<R>(name: &str, cfg: &Config, throughput: Option<Throughput>, mut f: impl FnMut() -> R) -> BenchResult {
    let iters = calibrate(&mut f, cfg);
    let max = Duration::from_millis(cfg.max_time_ms);
    let started = Instant::now();
    let mut samples = Vec::with_capacity(cfg.samples);
    while samples.len() < cfg.samples.max(1) {
        let t = time_batch(&mut f, iters);
        samples.push(t.as_nanos() as f64 / iters as f64);
        if samples.len() >= 10 && started.elapsed() > max {
            break;
        }
    }
    let summary = summarize(&samples, cfg.outliers);
    BenchResult { name: name.to_string(), iters_per_sample: iters, samples_ns: samples, throughput, summary }
}
