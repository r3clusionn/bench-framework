//! A benchmarking harness.
//!
//! ```
//! use benchlab::{Config, Suite};
//!
//! let mut suite = Suite::new(Config { warmup_ms: 20, sample_time_ms: 2, samples: 12, ..Config::default() });
//! let r = suite.bench("sum 1..1000", || (1..1000u64).sum::<u64>());
//! assert!(r.summary.median > 0.0);
//! let json = suite.into_report().to_json();
//! assert!(json.contains("sum 1..1000"));
//! ```
//!
//! A benchmark is a closure. [`Suite::bench`] runs it for a warm-up period while it works out how
//! many calls fit in one sample, then times `samples` samples and summarises them (median,
//! percentiles, spread, outliers). A [`Report`] holds every raw sample, so two reports can be
//! compared later with [`compare::compare`], which uses a Mann-Whitney U test on the raw samples
//! rather than comparing two means.

pub mod affinity;
pub mod compare;
pub mod micro;
pub mod report;
pub mod runner;
pub mod stats;

pub use report::{Environment, Report, Suite};
pub use runner::{measure, BenchResult, Config, Throughput};
pub use stats::{OutlierPolicy, Summary};
