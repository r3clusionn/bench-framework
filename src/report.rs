//! The JSON report, the suite that collects results, and the text tables.

use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::runner::{measure, timer_overhead_ns, timer_resolution_ns, BenchResult, Config, Throughput};

pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub logical_cpus: usize,
    /// The logical CPU the measurements were pinned to, if any.
    pub pinned_cpu: Option<usize>,
    /// Average cost of reading the clock, in nanoseconds.
    pub timer_overhead_ns: f64,
    /// Smallest step the clock reports, in nanoseconds.
    pub timer_resolution_ns: f64,
    pub benchlab_version: String,
}

impl Environment {
    pub fn capture(pinned_cpu: Option<usize>) -> Environment {
        Environment {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu: cpu_name(),
            logical_cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
            pinned_cpu,
            timer_overhead_ns: timer_overhead_ns(),
            timer_resolution_ns: timer_resolution_ns(),
            benchlab_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

fn cpu_name() -> String {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("reg")
            .args(["query", r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0", "/v", "ProcessorNameString"])
            .output();
        if let Ok(o) = out {
            let text = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = text.lines().find(|l| l.contains("ProcessorNameString")) {
                if let Some((_, name)) = line.split_once("REG_SZ") {
                    return name.trim().to_string();
                }
            }
        }
        std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".to_string())
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|t| t.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string()))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    /// Seconds since the Unix epoch when the run finished.
    pub created_unix: u64,
    pub environment: Environment,
    pub config: Config,
    pub results: Vec<BenchResult>,
}

impl Report {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serializes")
    }

    pub fn from_json(text: &str) -> Result<Report, String> {
        let r: Report = serde_json::from_str(text).map_err(|e| format!("not a benchlab report: {e}"))?;
        if r.schema != SCHEMA {
            return Err(format!("unsupported report schema {} (this build reads {SCHEMA})", r.schema));
        }
        Ok(r)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        std::fs::write(path, self.to_json())
    }

    pub fn load(path: &Path) -> Result<Report, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Report::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Collects benchmarks measured with one configuration.
pub struct Suite {
    config: Config,
    pinned_cpu: Option<usize>,
    results: Vec<BenchResult>,
}

impl Suite {
    pub fn new(config: Config) -> Suite {
        Suite { config, pinned_cpu: None, results: Vec::new() }
    }

    /// Pins the calling thread to `cpu` for every benchmark in this suite.
    pub fn pin(mut self, cpu: usize) -> io::Result<Suite> {
        crate::affinity::pin_current_thread(cpu)?;
        self.pinned_cpu = Some(cpu);
        Ok(self)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Measures `f`; its return value is passed through `black_box` so the work is not optimised away.
    pub fn bench<R>(&mut self, name: &str, f: impl FnMut() -> R) -> &BenchResult {
        self.push(measure(name, &self.config, None, f))
    }

    /// Like [`bench`](Self::bench), with the amount of work per call so rates can be reported.
    pub fn bench_throughput<R>(&mut self, name: &str, throughput: Throughput, f: impl FnMut() -> R) -> &BenchResult {
        self.push(measure(name, &self.config, Some(throughput), f))
    }

    fn push(&mut self, r: BenchResult) -> &BenchResult {
        self.results.push(r);
        self.results.last().unwrap()
    }

    pub fn results(&self) -> &[BenchResult] {
        &self.results
    }

    pub fn into_report(self) -> Report {
        Report {
            schema: SCHEMA,
            created_unix: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            environment: Environment::capture(self.pinned_cpu),
            config: self.config,
            results: self.results,
        }
    }
}

/// Formats nanoseconds with a unit that keeps three or four significant digits.
pub fn fmt_ns(ns: f64) -> String {
    if !ns.is_finite() {
        "-".to_string()
    } else if ns < 1_000.0 {
        format!("{ns:.2} ns")
    } else if ns < 1_000_000.0 {
        format!("{:.2} us", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.3} s", ns / 1e9)
    }
}

fn fmt_rate(r: &BenchResult) -> String {
    match (r.throughput, r.rate()) {
        (Some(Throughput::Bytes(_)), Some(gbs)) => format!("{gbs:.2} GB/s"),
        (Some(Throughput::Elements(_)), Some(g)) => format!("{g:.2} Gop/s"),
        _ => String::new(),
    }
}

pub fn fmt_env(e: &Environment) -> String {
    let pin = e.pinned_cpu.map_or("not pinned".to_string(), |c| format!("pinned to CPU {c}"));
    format!(
        "{}, {} logical CPUs, {} {}, {pin}, clock step {:.0} ns, {:.0} ns per read",
        e.cpu, e.logical_cpus, e.os, e.arch, e.timer_resolution_ns, e.timer_overhead_ns
    )
}

/// A table with one row per benchmark.
pub fn render_table(results: &[BenchResult]) -> String {
    let w = results.iter().map(|r| r.name.len()).max().unwrap_or(9).max(9);
    let mut o = String::new();
    let _ = writeln!(
        o,
        "{:<w$}  {:>10}  {:>10}  {:>9}  {:>10}  {:>10}  {:>5}  {:>9}  {:>12}",
        "benchmark", "median", "p5", "cv", "p95", "p99", "outl", "per op", "rate"
    );
    for r in results {
        let s = &r.summary;
        let per_op = r.ns_per_op().map_or(String::new(), fmt_ns);
        let _ = writeln!(
            o,
            "{:<w$}  {:>10}  {:>10}  {:>8.1}%  {:>10}  {:>10}  {:>5}  {:>9}  {:>12}",
            r.name,
            fmt_ns(s.median),
            fmt_ns(s.p5),
            s.cv * 100.0,
            fmt_ns(s.p95),
            fmt_ns(s.p99),
            s.outliers.total(),
            per_op,
            fmt_rate(r)
        );
    }
    o
}
