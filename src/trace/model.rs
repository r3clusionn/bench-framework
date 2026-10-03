//! The data model for DPC and ISR analysis and the statistics computed from it. No platform code
//! lives here, so everything is testable with constructed events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::stats::percentile;

/// What kind of kernel callback an event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Kind {
    Isr,
    Dpc,
    TimerDpc,
    ThreadDpc,
}

impl Kind {
    pub fn is_isr(self) -> bool {
        self == Kind::Isr
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Isr => "ISR",
            Kind::Dpc => "DPC",
            Kind::TimerDpc => "DPC (timer)",
            Kind::ThreadDpc => "DPC (thread)",
        }
    }
}

/// One interrupt service routine or deferred procedure call that ran to completion.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub kind: Kind,
    pub cpu: u16,
    /// Start time in nanoseconds since the first event of the trace.
    pub start_ns: u64,
    pub elapsed_ns: u64,
    /// Address of the routine that ran.
    pub routine: u64,
    /// Interrupt vector (ISRs only, otherwise 0).
    pub vector: u16,
}

/// A loaded kernel image, used to turn a routine address into a driver name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub base: u64,
    pub size: u64,
    pub name: String,
    /// Path as the kernel reports it, e.g. `\SystemRoot\System32\drivers\ndis.sys`.
    #[serde(default)]
    pub path: String,
    /// `TimeDateStamp` of the image's PE header, to tell whether the file on disk is the one that ran.
    #[serde(default)]
    pub timestamp: u32,
}

/// Everything read from a trace file that the analysis needs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Trace {
    pub events: Vec<Event>,
    pub images: Vec<Image>,
    /// Length of the trace in nanoseconds.
    pub duration_ns: u64,
    pub cpu_count: usize,
    pub events_lost: u32,
    pub buffers_lost: u32,
    /// Scheduling, sampling, I/O and present events (empty for a DPC-only trace).
    #[serde(skip)]
    pub sys: super::sys::SysEvents,
}

/// Maps an address to the image that contains it.
pub struct ImageMap {
    sorted: Vec<Image>,
}

impl ImageMap {
    pub fn new(images: &[Image]) -> ImageMap {
        let mut sorted: Vec<Image> = images.iter().filter(|i| i.size > 0).cloned().collect();
        sorted.sort_by_key(|i| i.base);
        sorted.dedup_by(|a, b| a.base == b.base);
        ImageMap { sorted }
    }

    pub fn find(&self, addr: u64) -> Option<&Image> {
        let idx = self.sorted.partition_point(|i| i.base <= addr);
        let img = self.sorted.get(idx.checked_sub(1)?)?;
        (addr - img.base < img.size).then_some(img)
    }

    /// `driver.sys` for a known address, `unknown` otherwise.
    pub fn module(&self, addr: u64) -> String {
        self.find(addr).map(|i| i.name.clone()).unwrap_or_else(|| "unknown".to_string())
    }

    /// `driver.sys+0x1234` for a known address.
    pub fn function(&self, addr: u64) -> String {
        match self.find(addr) {
            Some(i) => format!("{}+0x{:x}", i.name, addr - i.base),
            None => format!("unknown+0x{addr:x}"),
        }
    }
}

/// The last path component of an image path, whichever separator it uses.
pub fn base_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Distribution of a set of values (microseconds or milliseconds, the caller says which).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dist {
    pub count: usize,
    pub min: f64,
    pub mean: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
}

impl Dist {
    pub fn of(values: &mut [f64]) -> Dist {
        if values.is_empty() {
            return Dist::default();
        }
        values.sort_by(|a, b| a.total_cmp(b));
        let n = values.len();
        Dist {
            count: n,
            min: values[0],
            mean: values.iter().sum::<f64>() / n as f64,
            p50: percentile(values, 50.0),
            p90: percentile(values, 90.0),
            p99: percentile(values, 99.0),
            p999: percentile(values, 99.9),
            max: values[n - 1],
        }
    }
}

/// Per-CPU totals.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuUsage {
    pub cpu: u16,
    pub isr_count: u64,
    pub isr_us: f64,
    pub dpc_count: u64,
    pub dpc_us: f64,
    /// ISR plus DPC time as a share of the trace duration, in percent.
    pub busy_pct: f64,
}

/// Statistics of every event of one driver (or function) and one family (ISR or DPC).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupStats {
    pub name: String,
    pub isr: bool,
    pub count: u64,
    pub per_second: f64,
    pub total_us: f64,
    /// Share of one CPU's time spent here, in percent of the whole machine's CPU time.
    pub machine_pct: f64,
    /// Execution time of one call, microseconds.
    pub elapsed_us: Dist,
    /// Time between the starts of consecutive calls, milliseconds.
    pub interval_ms: Dist,
    pub cpus: Vec<u16>,
}

/// A bucket of the log2 execution-time histogram.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    /// Exclusive lower bound in microseconds (0 for the first bucket).
    pub above_us: u64,
    pub up_to_us: u64,
    pub isr: u64,
    pub dpc: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Worst {
    pub kind: Kind,
    pub cpu: u16,
    pub at_ms: f64,
    pub elapsed_us: f64,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Analysis {
    pub duration_s: f64,
    pub cpu_count: usize,
    pub isr_events: u64,
    pub dpc_events: u64,
    pub events_lost: u32,
    pub buffers_lost: u32,
    pub per_cpu: Vec<CpuUsage>,
    pub groups: Vec<GroupStats>,
    pub histogram: Vec<Bucket>,
    pub worst: Vec<Worst>,
}

#[derive(Clone, Debug)]
pub struct AnalysisOptions {
    /// Group by function (`driver.sys+0xoffset`) instead of by driver.
    pub by_function: bool,
    /// Keep only events on this CPU.
    pub cpu: Option<u16>,
    /// Length of the list of longest events.
    pub worst: usize,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        AnalysisOptions { by_function: false, cpu: None, worst: 10 }
    }
}

/// Computes every statistic of a trace. `resolve` names the group an event belongs to; the default
/// is the image that contains its routine.
pub fn analyze(trace: &Trace, opts: &AnalysisOptions, resolve: Option<&dyn Fn(u64) -> String>) -> Analysis {
    let map = ImageMap::new(&trace.images);
    let name_of = |addr: u64| -> String {
        match resolve {
            Some(f) => f(addr),
            None if opts.by_function => map.function(addr),
            None => map.module(addr),
        }
    };
    let events: Vec<&Event> = trace.events.iter().filter(|e| opts.cpu.is_none_or(|c| e.cpu == c)).collect();
    let duration_ns = trace.duration_ns.max(events.iter().map(|e| e.start_ns + e.elapsed_ns).max().unwrap_or(0)).max(1);
    let duration_s = duration_ns as f64 / 1e9;
    let cpu_count = trace.cpu_count.max(events.iter().map(|e| e.cpu as usize + 1).max().unwrap_or(0)).max(1);

    let mut per_cpu: Vec<CpuUsage> = (0..cpu_count).map(|c| CpuUsage { cpu: c as u16, ..Default::default() }).collect();
    let mut by_group: BTreeMap<(String, bool), Vec<&Event>> = BTreeMap::new();
    let (mut isr_events, mut dpc_events) = (0u64, 0u64);
    let mut buckets: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    let mut worst: Vec<(&Event, String)> = Vec::new();

    for e in &events {
        let us = e.elapsed_ns as f64 / 1e3;
        let c = &mut per_cpu[e.cpu as usize];
        if e.kind.is_isr() {
            c.isr_count += 1;
            c.isr_us += us;
            isr_events += 1;
        } else {
            c.dpc_count += 1;
            c.dpc_us += us;
            dpc_events += 1;
        }
        by_group.entry((name_of(e.routine), e.kind.is_isr())).or_default().push(e);
        // Bucket b holds elapsed times in (2^(b-1), 2^b] microseconds, bucket 0 holds (0, 1].
        let b = if us <= 1.0 { 0 } else { (us.log2().ceil() as u32).min(40) };
        let slot = buckets.entry(b).or_default();
        if e.kind.is_isr() {
            slot.0 += 1;
        } else {
            slot.1 += 1;
        }
    }
    for c in &mut per_cpu {
        c.busy_pct = (c.isr_us + c.dpc_us) / (duration_ns as f64 / 1e3) * 100.0;
    }

    let mut groups: Vec<GroupStats> = by_group
        .into_iter()
        .map(|((name, isr), mut evs)| {
            evs.sort_by_key(|e| e.start_ns);
            let mut elapsed: Vec<f64> = evs.iter().map(|e| e.elapsed_ns as f64 / 1e3).collect();
            let total_us: f64 = elapsed.iter().sum();
            let mut gaps: Vec<f64> = evs.windows(2).map(|w| (w[1].start_ns - w[0].start_ns) as f64 / 1e6).collect();
            let mut cpus: Vec<u16> = evs.iter().map(|e| e.cpu).collect();
            cpus.sort_unstable();
            cpus.dedup();
            GroupStats {
                name,
                isr,
                count: evs.len() as u64,
                per_second: evs.len() as f64 / duration_s,
                total_us,
                machine_pct: total_us / (duration_ns as f64 / 1e3 * cpu_count as f64) * 100.0,
                elapsed_us: Dist::of(&mut elapsed),
                interval_ms: Dist::of(&mut gaps),
                cpus,
            }
        })
        .collect();
    groups.sort_by(|a, b| b.total_us.total_cmp(&a.total_us).then_with(|| a.name.cmp(&b.name)));

    let mut top = events.clone();
    top.sort_by(|a, b| b.elapsed_ns.cmp(&a.elapsed_ns).then(a.start_ns.cmp(&b.start_ns)));
    for e in top.into_iter().take(opts.worst) {
        worst.push((e, name_of(e.routine)));
    }

    let last = buckets.keys().next_back().copied().unwrap_or(0);
    let histogram = (0..=last)
        .map(|b| {
            let (isr, dpc) = buckets.get(&b).copied().unwrap_or_default();
            Bucket { above_us: if b == 0 { 0 } else { 1u64 << (b - 1) }, up_to_us: 1u64 << b, isr, dpc }
        })
        .collect();

    Analysis {
        duration_s,
        cpu_count,
        isr_events,
        dpc_events,
        events_lost: trace.events_lost,
        buffers_lost: trace.buffers_lost,
        per_cpu,
        groups,
        histogram,
        worst: worst
            .into_iter()
            .map(|(e, name)| Worst {
                kind: e.kind,
                cpu: e.cpu,
                at_ms: e.start_ns as f64 / 1e6,
                elapsed_us: e.elapsed_ns as f64 / 1e3,
                name,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: Kind, cpu: u16, start_us: u64, elapsed_us: u64, routine: u64) -> Event {
        Event { kind, cpu, start_ns: start_us * 1000, elapsed_ns: elapsed_us * 1000, routine, vector: 0 }
    }

    fn images() -> Vec<Image> {
        vec![
            Image { base: 0x1000, size: 0x1000, name: "a.sys".into(), ..Default::default() },
            Image { base: 0x2000, size: 0x800, name: "b.sys".into(), ..Default::default() },
        ]
    }

    #[test]
    fn address_lookup_respects_image_bounds() {
        let m = ImageMap::new(&images());
        assert_eq!(m.module(0x1000), "a.sys");
        assert_eq!(m.module(0x1fff), "a.sys");
        assert_eq!(m.module(0x2000), "b.sys");
        assert_eq!(m.module(0x27ff), "b.sys");
        assert_eq!(m.module(0x2800), "unknown");
        assert_eq!(m.module(0x0fff), "unknown");
        assert_eq!(m.function(0x2010), "b.sys+0x10");
    }

    #[test]
    fn base_name_handles_both_separators() {
        assert_eq!(base_name(r"\SystemRoot\System32\drivers\ndis.sys"), "ndis.sys");
        assert_eq!(base_name("C:/x/y.dll"), "y.dll");
        assert_eq!(base_name("plain"), "plain");
    }

    #[test]
    fn per_cpu_totals_and_busy_share() {
        let trace = Trace {
            events: vec![ev(Kind::Isr, 0, 0, 10, 0x1100), ev(Kind::Dpc, 0, 100, 30, 0x1100), ev(Kind::Dpc, 1, 200, 50, 0x2100)],
            images: images(),
            duration_ns: 1_000_000,
            cpu_count: 2,
            ..Default::default()
        };
        let a = analyze(&trace, &AnalysisOptions::default(), None);
        assert_eq!((a.isr_events, a.dpc_events), (1, 2));
        assert_eq!((a.per_cpu[0].isr_count, a.per_cpu[0].dpc_count), (1, 1));
        assert_eq!(a.per_cpu[0].isr_us, 10.0);
        assert_eq!(a.per_cpu[0].dpc_us, 30.0);
        // 40 us of 1000 us.
        assert!((a.per_cpu[0].busy_pct - 4.0).abs() < 1e-9);
        assert!((a.per_cpu[1].busy_pct - 5.0).abs() < 1e-9);
    }

    #[test]
    fn groups_split_by_driver_and_family_and_sort_by_time() {
        let trace = Trace {
            events: vec![
                ev(Kind::Dpc, 0, 0, 5, 0x1100),
                ev(Kind::TimerDpc, 0, 100, 5, 0x1100),
                ev(Kind::Isr, 0, 150, 1, 0x1100),
                ev(Kind::Dpc, 1, 200, 50, 0x2100),
                ev(Kind::Dpc, 1, 300, 0, 0x9000),
            ],
            images: images(),
            duration_ns: 1_000_000,
            cpu_count: 2,
            ..Default::default()
        };
        let a = analyze(&trace, &AnalysisOptions::default(), None);
        let names: Vec<(&str, bool, u64)> = a.groups.iter().map(|g| (g.name.as_str(), g.isr, g.count)).collect();
        assert_eq!(names, vec![("b.sys", false, 1), ("a.sys", false, 2), ("a.sys", true, 1), ("unknown", false, 1)]);
    }

    #[test]
    fn intervals_are_between_consecutive_starts_regardless_of_cpu() {
        let trace = Trace {
            events: vec![ev(Kind::Dpc, 0, 0, 1, 0x1100), ev(Kind::Dpc, 3, 1000, 1, 0x1100), ev(Kind::Dpc, 1, 3000, 1, 0x1100)],
            images: images(),
            duration_ns: 10_000_000,
            cpu_count: 4,
            ..Default::default()
        };
        let g = &analyze(&trace, &AnalysisOptions::default(), None).groups[0];
        assert_eq!(g.interval_ms.count, 2);
        assert_eq!(g.interval_ms.min, 1.0);
        assert_eq!(g.interval_ms.max, 2.0);
        assert_eq!(g.interval_ms.mean, 1.5);
        assert_eq!(g.cpus, vec![0, 1, 3]);
        assert!((g.per_second - 300.0).abs() < 1e-9);
    }

    #[test]
    fn histogram_buckets_are_powers_of_two() {
        let trace = Trace {
            events: vec![
                ev(Kind::Dpc, 0, 0, 0, 0x1100),
                ev(Kind::Dpc, 0, 10, 1, 0x1100),
                ev(Kind::Dpc, 0, 20, 2, 0x1100),
                ev(Kind::Isr, 0, 30, 3, 0x1100),
                ev(Kind::Isr, 0, 40, 4, 0x1100),
                ev(Kind::Dpc, 0, 50, 5, 0x1100),
            ],
            images: images(),
            duration_ns: 1_000_000,
            cpu_count: 1,
            ..Default::default()
        };
        let h = analyze(&trace, &AnalysisOptions::default(), None).histogram;
        let row = |i: usize| (h[i].above_us, h[i].up_to_us, h[i].isr, h[i].dpc);
        assert_eq!(row(0), (0, 1, 0, 2)); // 0 and 1
        assert_eq!(row(1), (1, 2, 0, 1)); // 2
        assert_eq!(row(2), (2, 4, 2, 0)); // 3 and 4
        assert_eq!(row(3), (4, 8, 0, 1)); // 5
        assert_eq!(h.len(), 4);
    }

    #[test]
    fn worst_events_are_longest_first_and_named() {
        let trace = Trace {
            events: vec![ev(Kind::Dpc, 0, 0, 5, 0x1100), ev(Kind::Isr, 2, 10, 90, 0x2100), ev(Kind::Dpc, 1, 20, 40, 0x1100)],
            images: images(),
            duration_ns: 1_000_000,
            cpu_count: 3,
            ..Default::default()
        };
        let a = analyze(&trace, &AnalysisOptions { worst: 2, ..Default::default() }, None);
        assert_eq!(a.worst.len(), 2);
        assert_eq!((a.worst[0].elapsed_us, a.worst[0].cpu, a.worst[0].name.as_str()), (90.0, 2, "b.sys"));
        assert_eq!(a.worst[1].elapsed_us, 40.0);
    }

    #[test]
    fn cpu_filter_and_function_grouping() {
        let trace = Trace {
            events: vec![ev(Kind::Dpc, 0, 0, 5, 0x1100), ev(Kind::Dpc, 1, 10, 5, 0x1200), ev(Kind::Dpc, 1, 20, 5, 0x1200)],
            images: images(),
            duration_ns: 1_000_000,
            cpu_count: 2,
            ..Default::default()
        };
        let a = analyze(&trace, &AnalysisOptions { by_function: true, cpu: Some(1), worst: 5 }, None);
        assert_eq!(a.groups.len(), 1);
        assert_eq!(a.groups[0].name, "a.sys+0x200");
        assert_eq!(a.groups[0].count, 2);
        assert_eq!(a.dpc_events, 2);
    }

    #[test]
    fn an_empty_trace_does_not_divide_by_zero() {
        let a = analyze(&Trace::default(), &AnalysisOptions::default(), None);
        assert!(a.groups.is_empty() && a.worst.is_empty() && a.histogram.len() == 1);
        assert!(a.duration_s > 0.0);
    }

    #[test]
    fn distribution_of_known_values() {
        let mut v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        let d = Dist::of(&mut v);
        assert_eq!((d.count, d.min, d.max, d.mean), (100, 1.0, 100.0, 50.5));
        assert!((d.p50 - 50.5).abs() < 1e-9);
        assert!(Dist::of(&mut []).count == 0);
    }
}
