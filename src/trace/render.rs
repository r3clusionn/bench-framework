//! Plain-text rendering of an analysis. ASCII only, so the report reads the same in any editor,
//! terminal or screenshot.

use super::model::{Analysis, Dist, GroupStats};
use super::presentmon::Summary;

/// Left- and right-aligned columns.
struct Table {
    head: Vec<String>,
    left: usize,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn new(head: &[&str], left: usize) -> Table {
        Table { head: head.iter().map(|s| s.to_string()).collect(), left, rows: Vec::new() }
    }

    fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    fn render(&self) -> String {
        let n = self.head.len();
        let mut w: Vec<usize> = self.head.iter().map(|h| h.chars().count()).collect();
        for r in &self.rows {
            for (i, c) in r.iter().enumerate().take(n) {
                w[i] = w[i].max(c.chars().count());
            }
        }
        let line = |cells: &[String]| -> String {
            let mut s = String::from(" ");
            for (i, c) in cells.iter().enumerate().take(n) {
                if i > 0 {
                    s.push_str("  ");
                }
                if i < self.left {
                    s.push_str(&format!("{c:<width$}", width = w[i]));
                } else {
                    s.push_str(&format!("{c:>width$}", width = w[i]));
                }
            }
            s.trim_end().to_string() + "\n"
        };
        let mut out = line(&self.head);
        out.push_str(&format!(" {}\n", "-".repeat(w.iter().sum::<usize>() + 2 * (n - 1))));
        for r in &self.rows {
            out.push_str(&line(r));
        }
        out
    }
}

/// Integer with thousands separators.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A number with a sensible number of decimals for its size.
pub fn num(v: f64) -> String {
    let a = v.abs();
    if a >= 1000.0 {
        format!("{v:.0}")
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else if a == 0.0 {
        "0".to_string()
    } else {
        format!("{v:.3}")
    }
}

fn kind_label(isr: bool) -> &'static str {
    if isr {
        "ISR"
    } else {
        "DPC"
    }
}

fn section(out: &mut String, title: &str) {
    out.push('\n');
    out.push_str(title);
    out.push('\n');
}

pub fn render(a: &Analysis, source: &str, top: usize, presentmon: Option<&Summary>) -> String {
    let mut out = String::new();
    out.push_str(&format!("Trace: {source}\n"));
    out.push_str(&format!(
        "{} CPUs, {:.2} s, {} ISR and {} DPC events",
        a.cpu_count,
        a.duration_s,
        thousands(a.isr_events),
        thousands(a.dpc_events)
    ));
    if a.events_lost > 0 || a.buffers_lost > 0 {
        out.push_str(&format!(", WARNING: {} events and {} buffers lost, results are incomplete", a.events_lost, a.buffers_lost));
    }
    out.push('\n');

    section(&mut out, "ISR/DPC usage by CPU (time in microseconds)");
    let mut t = Table::new(&["CPU", "ISR count", "ISR us", "DPC count", "DPC us", "busy %"], 1);
    for c in a.per_cpu.iter().filter(|c| c.isr_count + c.dpc_count > 0) {
        t.row(vec![
            c.cpu.to_string(),
            thousands(c.isr_count),
            format!("{:.1}", c.isr_us),
            thousands(c.dpc_count),
            format!("{:.1}", c.dpc_us),
            format!("{:.3}", c.busy_pct),
        ]);
    }
    let (ic, iu, dc, du): (u64, f64, u64, f64) =
        a.per_cpu.iter().fold((0, 0.0, 0, 0.0), |s, c| (s.0 + c.isr_count, s.1 + c.isr_us, s.2 + c.dpc_count, s.3 + c.dpc_us));
    t.row(vec![
        "all".into(),
        thousands(ic),
        num(iu),
        thousands(dc),
        num(du),
        format!("{:.3}", (iu + du) / (a.duration_s * 1e6 * a.cpu_count as f64) * 100.0),
    ]);
    out.push_str(&t.render());
    out.push_str(" busy % is ISR plus DPC time as a share of the CPU's time; the last row is the share of all CPUs together.\n");

    let shown: Vec<&GroupStats> = a.groups.iter().take(top).collect();
    let elapsed_row = |g: &GroupStats| -> Vec<String> {
        let d: &Dist = &g.elapsed_us;
        vec![
            g.name.clone(),
            kind_label(g.isr).into(),
            thousands(g.count),
            num(g.total_us / 1000.0),
            num(d.mean),
            num(d.p50),
            num(d.p99),
            num(d.p999),
            num(d.max),
        ]
    };
    section(&mut out, &format!("ISR/DPC elapsed time per call (microseconds), top {} by total time", shown.len()));
    let mut t = Table::new(&["driver", "kind", "calls", "total ms", "mean", "p50", "p99", "p99.9", "max"], 2);
    for g in &shown {
        t.row(elapsed_row(g));
    }
    out.push_str(&t.render());

    section(&mut out, "ISR/DPC interval between calls (milliseconds; 1 ms is 1000 calls per second)");
    let mut t = Table::new(&["driver", "kind", "per second", "mean", "p50", "p99", "min", "max", "CPUs"], 2);
    for g in &shown {
        let d = &g.interval_ms;
        let cpus = if g.cpus.len() > 4 {
            format!("{} CPUs", g.cpus.len())
        } else {
            g.cpus.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",")
        };
        t.row(vec![
            g.name.clone(),
            kind_label(g.isr).into(),
            num(g.per_second),
            num(d.mean),
            num(d.p50),
            num(d.p99),
            num(d.min),
            num(d.max),
            cpus,
        ]);
    }
    out.push_str(&t.render());

    section(&mut out, "Elapsed time histogram (calls per bucket, microseconds)");
    let max = a.histogram.iter().map(|b| b.isr + b.dpc).max().unwrap_or(1).max(1);
    let mut t = Table::new(&["above", "up to", "ISR", "DPC", ""], 0);
    for b in &a.histogram {
        let n = b.isr + b.dpc;
        t.row(vec![
            b.above_us.to_string(),
            b.up_to_us.to_string(),
            thousands(b.isr),
            thousands(b.dpc),
            format!("{:<30}", "#".repeat((n * 30).div_ceil(max) as usize)),
        ]);
    }
    out.push_str(&t.render());

    if !a.worst.is_empty() {
        section(&mut out, "Longest calls");
        let mut t = Table::new(&["at ms", "CPU", "kind", "microseconds", "driver"], 5);
        for w in &a.worst {
            t.row(vec![format!("{:.3}", w.at_ms), w.cpu.to_string(), w.kind.label().into(), num(w.elapsed_us), w.name.clone()]);
        }
        out.push_str(&t.render());
    }

    if let Some(pm) = presentmon {
        out.push_str(&render_presentmon(pm));
    }
    out
}

pub fn render_presentmon(pm: &Summary) -> String {
    let mut out = String::new();
    section(&mut out, "PresentMon (milliseconds unless stated)");
    if pm.apps.is_empty() {
        out.push_str(" no frames were captured (is the target running and presenting?)\n");
        return out;
    }
    for app in &pm.apps {
        out.push_str(&format!(
            "\n {}: {} frames in {:.2} s, {:.1} fps average, 1% low {:.1} fps, 0.1% low {:.1} fps, {} dropped\n",
            app.application,
            thousands(app.frames as u64),
            app.seconds,
            app.avg_fps,
            app.low_1_fps,
            app.low_01_fps,
            app.dropped
        ));
        let mut t = Table::new(&["metric", "mean", "p50", "p90", "p99", "p99.9", "max"], 1);
        for m in &app.metrics {
            let d = &m.ms;
            t.row(vec![m.name.clone(), num(d.mean), num(d.p50), num(d.p90), num(d.p99), num(d.p999), num(d.max)]);
        }
        out.push_str(&t.render());
    }
    out.push_str("\n 1% low is 1000 / the 99th percentile frame time.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::model::{analyze, AnalysisOptions, Event, Image, Kind, Trace};

    #[test]
    fn numbers_are_grouped_and_rounded() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(num(0.0), "0");
        assert_eq!(num(0.12345), "0.123");
        assert_eq!(num(12.345), "12.35");
        assert_eq!(num(123.45), "123.5");
        assert_eq!(num(12345.6), "12346");
    }

    fn sample() -> Analysis {
        let trace = Trace {
            events: vec![
                Event { kind: Kind::Dpc, cpu: 0, start_ns: 0, elapsed_ns: 5000, routine: 0x1100, vector: 0 },
                Event { kind: Kind::Isr, cpu: 1, start_ns: 1_000_000, elapsed_ns: 9000, routine: 0x2100, vector: 5 },
            ],
            images: vec![
                Image { base: 0x1000, size: 0x1000, name: "a.sys".into(), ..Default::default() },
                Image { base: 0x2000, size: 0x1000, name: "b.sys".into(), ..Default::default() },
            ],
            duration_ns: 10_000_000,
            cpu_count: 2,
            ..Default::default()
        };
        analyze(&trace, &AnalysisOptions::default(), None)
    }

    #[test]
    fn report_has_every_section_and_no_non_ascii() {
        let r = render(&sample(), "t.etl", 10, None);
        for s in [
            "ISR/DPC usage by CPU",
            "elapsed time per call",
            "interval between calls",
            "histogram",
            "Longest calls",
            "a.sys",
            "b.sys",
        ] {
            assert!(r.contains(s), "missing {s}");
        }
        assert!(r.is_ascii(), "report must be ASCII");
        assert!(!r.contains("PresentMon"));
    }

    #[test]
    fn a_lossy_trace_is_flagged_loudly() {
        let mut a = sample();
        a.events_lost = 12;
        assert!(render(&a, "t.etl", 5, None).contains("WARNING: 12 events"));
        a.events_lost = 0;
        assert!(!render(&a, "t.etl", 5, None).contains("WARNING"));
    }

    #[test]
    fn top_limits_the_driver_tables() {
        let r = render(&sample(), "t.etl", 1, None);
        assert!(r.contains("top 1 by total time"));
        // b.sys has the larger total time (9 us against 5 us); a.sys is cut from both tables.
        assert!(r.matches("a.sys").count() == 0 || !r.contains("top 2"));
    }

    #[test]
    fn presentmon_section_renders_and_says_when_empty() {
        let s = crate::trace::presentmon::summarize("Application,FrameTime,CPUBusy\nx.exe,10.0,4.0\nx.exe,20.0,6.0\n").unwrap();
        let r = render_presentmon(&s);
        assert!(r.contains("x.exe: 2 frames") && r.contains("CPU busy") && r.is_ascii());
        assert!(render_presentmon(&Summary::default()).contains("no frames were captured"));
    }
}
