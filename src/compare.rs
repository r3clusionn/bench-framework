//! Regression detection between two reports.

use std::fmt::Write as _;

use crate::report::{fmt_env, fmt_ns, Report};
use crate::stats::mann_whitney;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Slower by more than the threshold, and the difference is statistically significant.
    Regressed,
    /// Faster by more than the threshold, and the difference is statistically significant.
    Improved,
    /// Moved by more than the threshold but the samples overlap too much to be sure.
    Inconclusive,
    /// Within the threshold, or no significant difference.
    Unchanged,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub old_median: f64,
    pub new_median: f64,
    /// (new - old) / old, so +0.10 is 10 percent slower.
    pub change: f64,
    /// Two-sided Mann-Whitney p-value on the raw samples.
    pub p: f64,
    pub verdict: Verdict,
}

#[derive(Clone, Debug)]
pub struct Comparison {
    pub rows: Vec<Row>,
    pub only_in_base: Vec<String>,
    pub only_in_new: Vec<String>,
    pub warnings: Vec<String>,
}

impl Comparison {
    pub fn regressions(&self) -> usize {
        self.rows.iter().filter(|r| r.verdict == Verdict::Regressed).count()
    }
}

/// Compares every benchmark that appears in both reports. `threshold` is a fraction (0.05 is 5
/// percent) and `alpha` the significance level of the Mann-Whitney test.
pub fn compare(base: &Report, new: &Report, threshold: f64, alpha: f64) -> Comparison {
    let mut rows = Vec::new();
    let mut only_in_base = Vec::new();
    for b in &base.results {
        let Some(n) = new.results.iter().find(|n| n.name == b.name) else {
            only_in_base.push(b.name.clone());
            continue;
        };
        let (old, newm) = (b.summary.median, n.summary.median);
        let change = if old > 0.0 { (newm - old) / old } else { 0.0 };
        let p = mann_whitney(&n.samples_ns, &b.samples_ns).p;
        let significant = p < alpha;
        let verdict = if change.abs() <= threshold {
            Verdict::Unchanged
        } else if !significant {
            Verdict::Inconclusive
        } else if change > 0.0 {
            Verdict::Regressed
        } else {
            Verdict::Improved
        };
        rows.push(Row { name: b.name.clone(), old_median: old, new_median: newm, change, p, verdict });
    }
    let only_in_new = new.results.iter().filter(|n| !base.results.iter().any(|b| b.name == n.name)).map(|n| n.name.clone()).collect();
    let mut warnings = Vec::new();
    if base.environment.cpu != new.environment.cpu || base.environment.logical_cpus != new.environment.logical_cpus {
        warnings.push(format!("the reports come from different CPUs:\n    base: {}\n    new:  {}", fmt_env(&base.environment), fmt_env(&new.environment)));
    }
    if base.environment.pinned_cpu != new.environment.pinned_cpu {
        warnings.push("one report was pinned to a CPU and the other was not or used a different one".to_string());
    }
    Comparison { rows, only_in_base, only_in_new, warnings }
}

/// Benchmarks listed by name in each of the "only in" lists before the rest are counted.
const MAX_LISTED: usize = 5;

pub fn render(c: &Comparison, threshold: f64, alpha: f64) -> String {
    let w = c.rows.iter().map(|r| r.name.len()).max().unwrap_or(9).max(9);
    let mut o = String::new();
    for warn in &c.warnings {
        let _ = writeln!(o, "warning: {warn}");
    }
    let _ = writeln!(o, "{:<w$}  {:>10}  {:>10}  {:>8}  {:>9}  verdict", "benchmark", "base", "new", "change", "p");
    for r in &c.rows {
        let verdict = match r.verdict {
            Verdict::Regressed => "REGRESSED",
            Verdict::Improved => "improved",
            Verdict::Inconclusive => "inconclusive (not significant)",
            Verdict::Unchanged => "unchanged",
        };
        let _ = writeln!(
            o,
            "{:<w$}  {:>10}  {:>10}  {:>+7.1}%  {:>9.2e}  {verdict}",
            r.name,
            fmt_ns(r.old_median),
            fmt_ns(r.new_median),
            r.change * 100.0,
            r.p
        );
    }
    for (label, names) in [("only in base", &c.only_in_base), ("only in new", &c.only_in_new)] {
        for n in names.iter().take(MAX_LISTED) {
            let _ = writeln!(o, "{label}: {n}");
        }
        if names.len() > MAX_LISTED {
            let _ = writeln!(o, "{label}: and {} more", names.len() - MAX_LISTED);
        }
    }
    let improved = c.rows.iter().filter(|r| r.verdict == Verdict::Improved).count();
    let _ = writeln!(
        o,
        "\n{} compared, {} regressed, {} improved (threshold {:.1}%, significance p < {alpha})",
        c.rows.len(),
        c.regressions(),
        improved,
        threshold * 100.0
    );
    o
}
