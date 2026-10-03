//! Plain-text rendering of the full analysis ([`super::analysis::Report`]), section by section.
//! ASCII only, like the DPC report.

use super::analysis::{IoRow, NameRow, Report};
use super::model::Dist;
use super::render::{num, section, thousands, Table};

/// The sections of `trace analyze`, in report order.
pub const SECTIONS: &[(&str, &str)] = &[
    ("overview", "trace length, event counts, focus process, warnings"),
    ("frames", "FPS, min FPS, 1% and 0.1% lows, stutters and frame time percentiles per swap chain"),
    ("presentmon", "PresentMon's metrics (GPU busy, displayed time, latency) when it ran over the trace"),
    ("explain", "the slowest frames of the focus process, each with what its render thread was doing"),
    ("processes", "CPU usage (precise) per process, with samples, ready time, hard faults and disk bytes"),
    ("cpus", "busy and DPC/ISR share per logical CPU"),
    ("threads", "the focus process's threads: CPU, ready time, waits"),
    ("sampled", "CPU usage (sampled): by module, by function, and inclusive by function from stacks"),
    ("ready", "ready time (ready to running) per process"),
    ("waits", "why the focus process's threads waited, where, and who preempted them"),
    ("faults", "hard page faults by process and by file"),
    ("disk", "disk I/O by process and file, service time"),
    ("dpc", "the DPC/ISR report (usage by CPU, elapsed time and interval per driver)"),
];

pub fn parse_sections(list: Option<&str>) -> Result<Vec<String>, String> {
    let Some(list) = list else {
        return Ok(SECTIONS.iter().map(|s| s.0.to_string()).collect());
    };
    let mut out = Vec::new();
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let p = part.to_ascii_lowercase();
        if p == "all" {
            return parse_sections(None);
        }
        if !SECTIONS.iter().any(|s| s.0 == p) {
            let names: Vec<&str> = SECTIONS.iter().map(|s| s.0).collect();
            return Err(format!("unknown section `{part}` (one of: {})", names.join(", ")));
        }
        out.push(p);
    }
    if out.is_empty() {
        return Err("no sections given".to_string());
    }
    Ok(out)
}

fn names_table(rows: &[NameRow], name: &str, count: &str, with_ms: bool) -> String {
    let mut head = vec![name, count];
    if with_ms {
        head.push("est. ms");
    }
    head.push("%");
    let mut t = Table::new(&head, 1);
    for r in rows {
        let mut cells = vec![r.name.clone(), thousands(r.count)];
        if with_ms {
            cells.push(num(r.ms));
        }
        cells.push(format!("{:.1}", r.pct));
        t.row(cells);
    }
    t.render()
}

fn io_table(rows: &[IoRow], name: &str) -> String {
    let mut t = Table::new(&[name, "count", "MB", "total ms", "max ms"], 1);
    for r in rows {
        t.row(vec![r.name.clone(), thousands(r.count), format!("{:.2}", r.bytes as f64 / 1e6), num(r.total_ms), num(r.max_ms)]);
    }
    t.render()
}

fn dist_cells(d: &Dist) -> Vec<String> {
    vec![num(d.mean), num(d.p50), num(d.p90), num(d.p99), num(d.p999), num(d.max)]
}

fn fps(v: f64) -> String {
    format!("{v:.1}")
}

pub fn render(r: &Report, source: &str, sections: &[String], presentmon: Option<&str>, dpc: Option<&str>) -> String {
    let want = |s: &str| sections.iter().any(|x| x == s);
    let o = &r.overview;
    let mut out = String::new();
    if want("overview") {
        out.push_str(&format!("Trace: {source}\n"));
        out.push_str(&format!(
            "{} CPUs, {:.2} s recorded, analysing {:.3} s to {:.3} s\n",
            o.cpu_count, o.duration_s, o.from_s, o.to_s
        ));
        let interval = o.sample_interval_ms.map(|i| format!(" every {} ms", num(i))).unwrap_or_default();
        out.push_str(&format!(
            "{} context switches, {} ready events, {} CPU samples{interval} ({:.0}% with stacks), {} DPC/ISR, {} hard faults, {} disk I/Os, {} presents\n",
            thousands(o.switches),
            thousands(o.readies),
            thousands(o.samples),
            o.samples_with_stacks_pct,
            thousands(o.dpc_isr),
            thousands(o.hard_faults),
            thousands(o.disk_ios),
            thousands(o.presents)
        ));
        out.push_str(&format!("{} processes, {} threads", o.processes, o.threads));
        if let Some(f) = &o.focus {
            out.push_str(&format!("; focus process: {f}"));
        }
        out.push('\n');
        for n in &r.notes {
            out.push_str(&format!("note: {n}\n"));
        }
    }

    if want("frames") && !r.frames.is_empty() {
        section(&mut out, "Frames per swap chain (from the Present calls in the trace)");
        let mut t = Table::new(
            &[
                "process", "API", "frames", "seconds", "avg fps", "min fps", "1% low", "0.1% low", "1% avg", "0.1% avg",
                "stutters",
            ],
            2,
        );
        for f in &r.frames {
            t.row(vec![
                format!("{} ({})", f.process, f.pid),
                f.runtime.clone(),
                thousands(f.frames as u64),
                format!("{:.2}", f.seconds),
                fps(f.avg_fps),
                fps(f.min_fps),
                fps(f.low_1_fps),
                fps(f.low_01_fps),
                fps(f.low_1_avg_fps),
                fps(f.low_01_avg_fps),
                thousands(f.stutters as u64),
            ]);
        }
        out.push_str(&t.render());
        out.push_str(
            " min fps is 1000 over the longest frame. 1% and 0.1% low are 1000 over the 99th and 99.9th percentile frame time;\n",
        );
        out.push_str(
            " 1% and 0.1% avg are 1000 over the average of the slowest 1% and 0.1% of frames. A stutter is a frame longer than\n",
        );
        out.push_str(&format!(
            " {} times the median.\n",
            r.frames
                .first()
                .map(|f| if f.frame_ms.p50 > 0.0 { num(f.stutter_threshold_ms / f.frame_ms.p50) } else { "2".into() })
                .unwrap_or_default()
        ));
        section(&mut out, "Frame time and time inside Present (milliseconds)");
        let mut t = Table::new(&["process", "metric", "mean", "p50", "p90", "p99", "p99.9", "max"], 2);
        for f in &r.frames {
            let name = format!("{} ({})", f.process, f.pid);
            let mut cells = vec![name.clone(), "frame time".into()];
            cells.extend(dist_cells(&f.frame_ms));
            t.row(cells);
            if f.present_ms.count > 0 {
                let mut cells = vec![name, "in Present".into()];
                cells.extend(dist_cells(&f.present_ms));
                t.row(cells);
            }
        }
        out.push_str(&t.render());
    }

    if want("presentmon") {
        if let Some(pm) = presentmon {
            out.push_str(pm);
        }
    }

    if want("explain") && !r.explained.is_empty() {
        section(&mut out, &format!("The {} slowest frames of the focus process, in time order", r.explained.len()));
        for x in &r.explained {
            out.push_str(&format!(
                "\n at {:.1} ms: {:.2} ms frame ({:.1}x the median {:.2} ms), render thread {}\n",
                x.at_ms,
                x.frame_ms,
                if x.median_ms > 0.0 { x.frame_ms / x.median_ms } else { 0.0 },
                x.median_ms,
                x.tid
            ));
            out.push_str(&format!("   likely cause: {}\n", x.verdict));
            let waits: Vec<String> = x.waits.iter().take(3).map(|w| format!("{} {:.2}", w.reason, w.total_ms)).collect();
            out.push_str(&format!(
                "   render thread: running {:.2} ms, ready {:.2} ms, waiting {:.2} ms{}\n",
                x.running_ms,
                x.ready_ms,
                x.waiting_ms,
                if waits.is_empty() { String::new() } else { format!(" ({})", waits.join(", ")) }
            ));
            out.push_str(&format!("   whole process on CPU: {:.2} ms", x.process_cpu_ms));
            if let Some(g) = x.gpu_busy_ms {
                out.push_str(&format!("; GPU busy (PresentMon): {g:.2} ms"));
            }
            out.push('\n');
            out.push_str(&format!(
                "   DPC/ISR: {:.3} ms in total, {:.3} ms on the render thread's CPUs",
                x.dpc_isr_ms, x.dpc_isr_on_its_cpus_ms
            ));
            if let Some((name, us, cpu)) = &x.longest_dpc_isr {
                out.push_str(&format!(", longest {} us ({name}, CPU {cpu})", num(*us)));
            }
            out.push('\n');
            if x.hard_faults > 0 || x.disk_ios > 0 {
                out.push_str(&format!(
                    "   hard faults: {} ({:.2} ms), disk I/Os: {}\n",
                    x.hard_faults, x.hard_fault_ms, x.disk_ios
                ));
            }
            if !x.preempted_by.is_empty() {
                let p: Vec<String> = x.preempted_by.iter().map(|p| format!("{} x{}", p.name, p.count)).collect();
                out.push_str(&format!("   preempted by: {}\n", p.join(", ")));
            }
            if !x.top_functions.is_empty() {
                let f: Vec<String> = x.top_functions.iter().map(|p| format!("{} ({})", p.name, p.count)).collect();
                out.push_str(&format!("   render thread samples: {}\n", f.join(", ")));
            }
        }
    }

    if want("processes") && !r.processes.is_empty() {
        section(&mut out, "Processes: CPU usage (precise, from context switches)");
        let mut t = Table::new(
            &["process", "pid", "CPU ms", "CPU %", "samples", "switch-ins", "threads", "ready ms", "hard faults", "disk MB"],
            1,
        );
        for p in r.processes.iter().filter(|p| p.pid != 0).take(r.processes.len().min(30)) {
            t.row(vec![
                p.name.clone(),
                p.pid.to_string(),
                num(p.cpu_ms),
                format!("{:.2}", p.cpu_pct),
                thousands(p.samples),
                thousands(p.switches_in),
                p.threads.to_string(),
                num(p.ready_ms),
                thousands(p.hard_faults),
                format!("{:.2}", p.disk_bytes as f64 / 1e6),
            ]);
        }
        out.push_str(&t.render());
        out.push_str(" CPU % is the share of all CPUs' time. Without context switch events the CPU columns are 0; use the sampled tables.\n");
    }

    if want("cpus") && !r.cpus.is_empty() {
        section(&mut out, "Logical CPUs");
        let mut t = Table::new(&["CPU", "busy %", "switches", "DPC/ISR %"], 0);
        for c in &r.cpus {
            t.row(vec![
                c.cpu.to_string(),
                if c.busy_pct.is_nan() { "n/a".into() } else { format!("{:.1}", c.busy_pct) },
                thousands(c.switches),
                format!("{:.3}", c.dpc_isr_pct),
            ]);
        }
        out.push_str(&t.render());
    }

    if want("threads") && !r.threads.is_empty() {
        section(&mut out, "Threads of the focus process");
        let mut t = Table::new(
            &["tid", "name", "CPU ms", "samples", "switch-ins", "ready ms", "ready p99 us", "wait ms", "most waited on"],
            2,
        );
        for th in &r.threads {
            t.row(vec![
                th.tid.to_string(),
                th.name.clone(),
                num(th.cpu_ms),
                thousands(th.samples),
                thousands(th.switches_in),
                num(th.ready_ms),
                num(th.ready_us.p99),
                num(th.wait_ms),
                th.top_wait.clone(),
            ]);
        }
        out.push_str(&t.render());
    }

    if want("sampled") {
        if !r.sampled_modules.is_empty() {
            section(&mut out, "CPU usage (sampled) by process and module, idle excluded");
            out.push_str(&names_table(&r.sampled_modules, "process module", "samples", true));
        }
        if !r.sampled_functions.is_empty() {
            section(&mut out, "CPU usage (sampled) by function (exclusive: where the instruction pointer was)");
            out.push_str(&names_table(&r.sampled_functions, "function", "samples", true));
        }
        if !r.inclusive_functions.is_empty() {
            section(&mut out, "Focus process, inclusive by function (on the call stack; % of its samples with stacks)");
            out.push_str(&names_table(&r.inclusive_functions, "function", "samples", true));
        }
    }

    if want("ready") && !r.ready.is_empty() {
        section(&mut out, "Ready time per process (from ready to running, microseconds)");
        let mut t = Table::new(&["process", "count", "total ms", "mean", "p50", "p90", "p99", "p99.9", "max"], 1);
        for x in &r.ready {
            let mut cells = vec![x.name.clone(), thousands(x.count), num(x.total_ms)];
            cells.extend(dist_cells(&x.us));
            t.row(cells);
        }
        out.push_str(&t.render());
        out.push_str(
            " Ready time is how long a runnable thread waited for a CPU: high values mean CPU contention or priority problems.\n",
        );
    }

    if want("waits") {
        if !r.waits.is_empty() {
            section(&mut out, "Why the focus process's threads waited (wait reason)");
            let mut t = Table::new(&["reason", "waits", "total ms", "longest ms"], 1);
            for w in &r.waits {
                t.row(vec![w.reason.clone(), thousands(w.count), num(w.total_ms), num(w.max_ms)]);
            }
            out.push_str(&t.render());
            out.push_str(" Totals add up the waits of every thread, so they can exceed the length of the trace.
");
        }
        if !r.wait_sites.is_empty() {
            section(&mut out, "Where they waited (first frame past the system's wait code, from switch-in stacks)");
            out.push_str(&names_table(&r.wait_sites, "function", "switch-ins", false));
        }
        if !r.preempted_by.is_empty() {
            section(&mut out, "Processes that preempted the focus process's threads");
            out.push_str(&names_table(&r.preempted_by, "process", "times", false));
        }
    }

    if want("faults") && !r.faults_by_process.is_empty() {
        section(&mut out, "Hard page faults by process");
        out.push_str(&io_table(&r.faults_by_process, "process"));
        section(&mut out, "Hard page faults by file");
        out.push_str(&io_table(&r.faults_by_file, "file"));
    }

    if want("disk") && !r.disk_by_process.is_empty() {
        section(&mut out, "Disk I/O by process (total ms is disk service time)");
        out.push_str(&io_table(&r.disk_by_process, "process"));
        section(&mut out, "Disk I/O by file");
        out.push_str(&io_table(&r.disk_by_file, "file"));
        let d = &r.disk_ms;
        out.push_str(&format!(
            " service time: mean {} ms, p50 {} ms, p99 {} ms, max {} ms over {} requests\n",
            num(d.mean),
            num(d.p50),
            num(d.p99),
            num(d.max),
            thousands(d.count as u64)
        ));
    }

    if want("dpc") {
        if let Some(d) = dpc {
            section(&mut out, "DPC/ISR report");
            out.push_str(d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::analysis::{FrameExplain, FrameStats, ProcessRow, Report};

    #[test]
    fn sections_parse_and_reject_unknown_names() {
        assert_eq!(parse_sections(None).unwrap().len(), SECTIONS.len());
        assert_eq!(parse_sections(Some("frames, explain")).unwrap(), vec!["frames", "explain"]);
        assert_eq!(parse_sections(Some("ALL")).unwrap().len(), SECTIONS.len());
        assert!(parse_sections(Some("frames,bogus")).unwrap_err().contains("bogus"));
        assert!(parse_sections(Some(",")).is_err());
    }

    #[test]
    fn only_the_chosen_sections_are_printed_and_output_is_ascii() {
        let r = Report {
            frames: vec![FrameStats { pid: 5, process: "game.exe".into(), frames: 10, avg_fps: 144.0, ..Default::default() }],
            processes: vec![ProcessRow { pid: 5, name: "game.exe".into(), cpu_ms: 12.5, ..Default::default() }],
            explained: vec![FrameExplain {
                frame_ms: 30.0,
                median_ms: 7.0,
                verdict: "CPU bound: x".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let all = render(&r, "t.etl", &parse_sections(None).unwrap(), Some("\nPresentMon section\n"), Some("dpc text\n"));
        for s in [
            "Trace: t.etl",
            "Frames per swap chain",
            "144.0",
            "likely cause: CPU bound",
            "Processes:",
            "PresentMon section",
            "dpc text",
        ] {
            assert!(all.contains(s), "missing {s}");
        }
        assert!(all.is_ascii());
        let few = render(&r, "t.etl", &["frames".to_string()], None, None);
        assert!(few.contains("Frames per swap chain") && !few.contains("Trace:") && !few.contains("Processes:"));
    }
}
