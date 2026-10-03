//! `benchlab trace export`: writes one table of a trace as CSV, one row per event, the way WPA's
//! "Export" does for a table. Times are microseconds since the start of the trace.

use std::fmt::Write as _;

use super::model::{ImageMap, Trace};
use super::sys::{thread_state, wait_reason, SysEvents};

pub const TABLES: &[(&str, &str)] = &[
    ("processes", "pid, parent, name, start and end, command line"),
    ("threads", "tid, pid, name, start and end"),
    ("cswitch", "every context switch: time, CPU, new and old thread, old thread's state and wait reason"),
    ("ready", "every ready-thread event"),
    ("samples", "every CPU sample: time, CPU, thread, instruction pointer, module"),
    ("hard-faults", "every hard page fault: start, duration, thread, file, offset, size"),
    ("disk", "every disk request: completion time, service time, disk, kind, size, file"),
    ("presents", "every Present call: start, duration, process, thread, runtime, swap chain"),
    ("dpc", "every DPC and ISR: start, duration, CPU, kind, driver"),
];

fn us(ns: u64) -> String {
    format!("{:.3}", ns as f64 / 1000.0)
}

/// Quotes a CSV field when it needs it.
pub fn field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn opt(t: Option<u64>) -> String {
    t.map(us).unwrap_or_default()
}

fn pname(sys: &SysEvents, pid: u32) -> String {
    if pid == u32::MAX {
        String::new()
    } else {
        field(&sys.process_name(pid))
    }
}

pub fn export(trace: &Trace, table: &str) -> Result<String, String> {
    let sys = &trace.sys;
    let mut out = String::new();
    match table {
        "processes" => {
            out.push_str("pid,parent,name,start_us,end_us,command_line\n");
            for p in &sys.processes {
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{}",
                    p.pid,
                    p.parent,
                    field(&p.name),
                    opt(p.start_ns),
                    opt(p.end_ns),
                    field(&p.command_line)
                );
            }
        }
        "threads" => {
            out.push_str("tid,pid,process,name,start_us,end_us\n");
            for t in &sys.threads {
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{}",
                    t.tid,
                    t.pid,
                    pname(sys, t.pid),
                    field(&t.name),
                    opt(t.start_ns),
                    opt(t.end_ns)
                );
            }
        }
        "cswitch" => {
            out.push_str("time_us,cpu,new_tid,new_pid,new_process,new_priority,old_tid,old_pid,old_process,old_priority,old_state,wait_reason\n");
            for s in &sys.switches {
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{},{},{},{},{},{},{}",
                    us(s.t),
                    s.cpu,
                    s.new_tid,
                    s.new_pid as i64,
                    pname(sys, s.new_pid),
                    s.new_prio,
                    s.old_tid,
                    s.old_pid as i64,
                    pname(sys, s.old_pid),
                    s.old_prio,
                    thread_state(s.old_state),
                    if s.old_state == super::sys::STATE_WAITING { wait_reason(s.wait_reason) } else { "" }
                );
            }
        }
        "ready" => {
            out.push_str("time_us,cpu,tid,pid,process,readied_by_tid\n");
            for r in &sys.readies {
                let _ =
                    writeln!(out, "{},{},{},{},{},{}", us(r.t), r.cpu, r.tid, r.pid as i64, pname(sys, r.pid), r.by_tid as i32);
            }
        }
        "samples" => {
            out.push_str("time_us,cpu,tid,pid,process,ip,module\n");
            let kernel = ImageMap::new(&trace.images);
            let users = super::analysis::UserImages::new(sys);
            for s in &sys.samples {
                let module = kernel.find(s.ip).or_else(|| users.find(s.pid, s.ip)).map(|i| i.name.as_str()).unwrap_or("unknown");
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},0x{:x},{}",
                    us(s.t),
                    s.cpu,
                    s.tid,
                    s.pid as i64,
                    pname(sys, s.pid),
                    s.ip,
                    field(module)
                );
            }
        }
        "hard-faults" => {
            out.push_str("start_us,elapsed_us,tid,pid,process,offset,bytes,file\n");
            for h in &sys.hard_faults {
                let file = sys.file_names.get(&h.file).map(|s| s.as_str()).unwrap_or("");
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{},{},{}",
                    us(h.t),
                    us(h.elapsed_ns),
                    h.tid,
                    h.pid as i64,
                    pname(sys, h.pid),
                    h.offset,
                    h.bytes,
                    field(file)
                );
            }
        }
        "disk" => {
            out.push_str("complete_us,service_us,disk,kind,bytes,offset,tid,pid,process,file\n");
            for d in &sys.disk_ios {
                let file = sys.file_names.get(&d.file).map(|s| s.as_str()).unwrap_or("");
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{},{},{},{},{}",
                    us(d.t),
                    us(d.elapsed_ns),
                    d.disk,
                    d.kind.label(),
                    d.bytes,
                    d.offset,
                    d.tid,
                    d.pid as i64,
                    pname(sys, d.pid),
                    field(file)
                );
            }
        }
        "presents" => {
            out.push_str("start_us,duration_us,pid,process,tid,runtime,swap_chain,sync_interval,flags\n");
            for p in &sys.presents {
                let dur = if p.end >= p.t && p.end != 0 { us(p.end - p.t) } else { String::new() };
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{},0x{:x},{},0x{:x}",
                    us(p.t),
                    dur,
                    p.pid,
                    pname(sys, p.pid),
                    p.tid,
                    p.runtime.label(),
                    p.swap_chain,
                    p.sync_interval,
                    p.flags
                );
            }
        }
        "dpc" => {
            out.push_str("start_us,elapsed_us,cpu,kind,driver,routine\n");
            let map = ImageMap::new(&trace.images);
            for e in &trace.events {
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},0x{:x}",
                    us(e.start_ns),
                    us(e.elapsed_ns),
                    e.cpu,
                    e.kind.label(),
                    field(&map.module(e.routine)),
                    e.routine
                );
            }
        }
        other => {
            let names: Vec<&str> = TABLES.iter().map(|t| t.0).collect();
            return Err(format!("unknown table `{other}` (one of: {})", names.join(", ")));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::sys::{Process, Switch};

    #[test]
    fn fields_are_quoted_only_when_needed() {
        assert_eq!(field("plain"), "plain");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn every_table_has_a_header_and_unknown_tables_are_errors() {
        let mut t = Trace::default();
        t.sys.processes.push(Process { pid: 5, name: "a, b.exe".into(), ..Default::default() });
        t.sys.switches.push(Switch {
            t: 1500,
            cpu: 1,
            new_tid: 7,
            new_pid: 5,
            old_state: 5,
            wait_reason: 13,
            ..Default::default()
        });
        for (name, _) in TABLES {
            let csv = export(&t, name).unwrap();
            assert!(csv.lines().next().unwrap().contains('_') || csv.starts_with("pid") || csv.starts_with("tid"), "{name}");
        }
        let cs = export(&t, "cswitch").unwrap();
        assert!(cs.contains("1.500,1,7,5,\"a, b.exe\""), "{cs}");
        assert!(cs.contains("Waiting,WrUserRequest"));
        assert!(export(&t, "nope").unwrap_err().contains("cswitch"));
    }
}
