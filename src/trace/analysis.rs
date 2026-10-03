//! The full trace analysis behind `benchlab trace analyze`: what WPA shows in its CPU Usage
//! (Precise and Sampled), Ready Thread, Hard Faults, Disk Usage and frame tables, computed from
//! [`SysEvents`], plus an explanation of individual slow frames.
//!
//! Everything here is platform independent and works on constructed events, so it is unit tested
//! without a trace file.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::model::{Dist, Image, ImageMap, Trace};
use super::sys::{wait_reason, SysEvents, NO_STACK, STATE_READY, STATE_WAITING};

/// Which process the per-thread sections are about.
#[derive(Clone, Debug, PartialEq)]
pub enum Focus {
    Pid(u32),
    /// An executable name, matched without case (`game.exe` or `game`).
    Name(String),
}

impl Focus {
    pub fn parse(s: &str) -> Focus {
        match s.trim().parse::<u32>() {
            Ok(pid) => Focus::Pid(pid),
            Err(_) => Focus::Name(s.trim().to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Window to analyse, nanoseconds since the start of the trace.
    pub from_ns: u64,
    pub to_ns: u64,
    pub focus: Option<Focus>,
    /// Rows per table.
    pub top: usize,
    /// A frame is a stutter when it is longer than this many times the median frame.
    pub stutter_factor: f64,
    /// How many of the slowest frames to explain.
    pub explain: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { from_ns: 0, to_ns: u64::MAX, focus: None, top: 15, stutter_factor: 2.0, explain: 5 }
    }
}

/// Names addresses: `module+0xoffset` by default, `module!Function` with symbols.
pub trait Namer {
    fn module(&mut self, pid: u32, addr: u64) -> String;
    fn function(&mut self, pid: u32, addr: u64) -> String;
}

/// User-mode images of every process, for address lookups.
pub struct UserImages {
    by_pid: HashMap<u32, ImageMap>,
}

impl UserImages {
    pub fn new(sys: &SysEvents) -> UserImages {
        let mut lists: HashMap<u32, Vec<Image>> = HashMap::new();
        for (pid, img) in &sys.user_images {
            lists.entry(*pid).or_default().push(img.clone());
        }
        UserImages { by_pid: lists.into_iter().map(|(pid, l)| (pid, ImageMap::new(&l))).collect() }
    }

    pub fn find(&self, pid: u32, addr: u64) -> Option<&Image> {
        self.by_pid.get(&pid)?.find(addr)
    }
}

/// Module and `module+offset` names from the images in the trace, no symbols.
pub struct PlainNamer {
    kernel: ImageMap,
    user: UserImages,
}

impl PlainNamer {
    pub fn new(trace: &Trace) -> PlainNamer {
        PlainNamer { kernel: ImageMap::new(&trace.images), user: UserImages::new(&trace.sys) }
    }

    pub fn image(&self, pid: u32, addr: u64) -> Option<&Image> {
        self.kernel.find(addr).or_else(|| self.user.find(pid, addr))
    }
}

impl Namer for PlainNamer {
    fn module(&mut self, pid: u32, addr: u64) -> String {
        self.image(pid, addr).map(|i| i.name.clone()).unwrap_or_else(|| "unknown".to_string())
    }

    fn function(&mut self, pid: u32, addr: u64) -> String {
        match self.image(pid, addr) {
            Some(i) => format!("{}+0x{:x}", i.name, addr - i.base),
            None => format!("unknown+0x{addr:x}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    pub duration_s: f64,
    pub from_s: f64,
    pub to_s: f64,
    pub cpu_count: usize,
    pub events_lost: u32,
    pub buffers_lost: u32,
    pub processes: usize,
    pub threads: usize,
    pub switches: u64,
    pub readies: u64,
    pub samples: u64,
    pub sample_interval_ms: Option<f64>,
    /// Share of samples that carry a call stack.
    pub samples_with_stacks_pct: f64,
    pub dpc_isr: u64,
    pub hard_faults: u64,
    pub disk_ios: u64,
    pub presents: u64,
    pub focus: Option<String>,
    /// Performance-counter value of time zero and counter ticks per second: an external QPC
    /// timestamp `q` is at `(q - clock_zero) / clock_freq` seconds in this report.
    pub clock_zero: i64,
    pub clock_freq: u64,
}

/// One process with every per-process number of the report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessRow {
    pub pid: u32,
    pub name: String,
    /// Running time from context switches, milliseconds.
    pub cpu_ms: f64,
    /// `cpu_ms` as a share of all CPUs' time, percent.
    pub cpu_pct: f64,
    pub samples: u64,
    pub switches_in: u64,
    pub threads: usize,
    /// Time threads spent ready but not running, milliseconds.
    pub ready_ms: f64,
    pub hard_faults: u64,
    pub disk_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuRow {
    pub cpu: u16,
    /// Time not spent in the idle thread, percent.
    pub busy_pct: f64,
    pub switches: u64,
    /// DPC and ISR time, percent.
    pub dpc_isr_pct: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadRow {
    pub tid: u32,
    pub name: String,
    pub cpu_ms: f64,
    pub samples: u64,
    pub switches_in: u64,
    pub ready_ms: f64,
    /// Ready time per switch-in, microseconds.
    pub ready_us: Dist,
    pub wait_ms: f64,
    pub top_wait: String,
}

/// A name with a count and an amount (samples and milliseconds, preemptions and so on).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NameRow {
    pub name: String,
    pub count: u64,
    pub ms: f64,
    /// Share of the table's total, percent.
    pub pct: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadyRow {
    pub name: String,
    pub count: u64,
    pub total_ms: f64,
    /// Microseconds from becoming ready to running.
    pub us: Dist,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WaitRow {
    pub reason: String,
    pub count: u64,
    pub total_ms: f64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IoRow {
    pub name: String,
    pub count: u64,
    pub bytes: u64,
    /// Time spent: fault duration or disk service time, milliseconds.
    pub total_ms: f64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameStats {
    pub pid: u32,
    pub process: String,
    pub runtime: String,
    pub swap_chain: u64,
    /// The thread that presents most often (the render thread).
    pub present_tid: u32,
    pub sync_interval: i32,
    pub frames: usize,
    pub seconds: f64,
    pub avg_fps: f64,
    /// 1000 over the longest frame.
    pub min_fps: f64,
    /// 1000 over the 99th and 99.9th percentile frame time.
    pub low_1_fps: f64,
    pub low_01_fps: f64,
    /// 1000 over the average of the slowest 1% and 0.1% of frames.
    pub low_1_avg_fps: f64,
    pub low_01_avg_fps: f64,
    pub stutters: usize,
    pub stutter_threshold_ms: f64,
    /// Time between presents, milliseconds.
    pub frame_ms: Dist,
    /// Time spent inside Present, milliseconds.
    pub present_ms: Dist,
}

/// Why one frame took long.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameExplain {
    pub at_ms: f64,
    pub frame_ms: f64,
    pub median_ms: f64,
    pub tid: u32,
    pub running_ms: f64,
    pub ready_ms: f64,
    pub waiting_ms: f64,
    pub waits: Vec<WaitRow>,
    /// Running time of all the process's threads in the frame.
    pub process_cpu_ms: f64,
    pub dpc_isr_ms: f64,
    /// DPC and ISR time on the CPUs the render thread ran on during the frame.
    pub dpc_isr_on_its_cpus_ms: f64,
    pub longest_dpc_isr: Option<(String, f64, u16)>,
    pub hard_faults: u64,
    pub hard_fault_ms: f64,
    pub disk_ios: u64,
    pub preempted_by: Vec<NameRow>,
    pub top_functions: Vec<NameRow>,
    /// From PresentMon, when its CSV was matched to this frame.
    pub gpu_busy_ms: Option<f64>,
    pub verdict: String,
}

/// Series for charts, one value per bucket.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub bucket_ms: f64,
    pub start_ms: f64,
    /// Share of all CPUs' time not idle, percent.
    pub cpu_busy_pct: Vec<f64>,
    /// Share of all CPUs' time used by the focus process, percent.
    pub focus_cpu_pct: Vec<f64>,
    /// Share of all CPUs' time in DPCs and ISRs, percent.
    pub dpc_isr_pct: Vec<f64>,
    pub hard_faults: Vec<u32>,
    pub disk_mb: Vec<f64>,
    /// (time ms, frame ms) of the focus swap chain.
    pub frames: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub overview: Overview,
    pub processes: Vec<ProcessRow>,
    pub cpus: Vec<CpuRow>,
    pub threads: Vec<ThreadRow>,
    pub sampled_modules: Vec<NameRow>,
    pub sampled_functions: Vec<NameRow>,
    /// Functions on the stacks of the focus process's samples (inclusive).
    pub inclusive_functions: Vec<NameRow>,
    pub ready: Vec<ReadyRow>,
    pub waits: Vec<WaitRow>,
    /// Where the focus process's threads waited, by the first frame outside the system's wait code.
    pub wait_sites: Vec<NameRow>,
    pub preempted_by: Vec<NameRow>,
    pub faults_by_process: Vec<IoRow>,
    pub faults_by_file: Vec<IoRow>,
    pub disk_by_process: Vec<IoRow>,
    pub disk_by_file: Vec<IoRow>,
    /// Disk service time, milliseconds.
    pub disk_ms: Dist,
    pub frames: Vec<FrameStats>,
    pub explained: Vec<FrameExplain>,
    pub timeline: Timeline,
    pub notes: Vec<String>,
}

/// What a thread was doing during an interval.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Running(u16),
    Ready,
    Waiting(u8),
}

#[derive(Clone, Copy, Debug)]
struct Span {
    start: u64,
    end: u64,
    state: State,
}

fn overlap(a0: u64, a1: u64, b0: u64, b1: u64) -> u64 {
    a1.min(b1).saturating_sub(a0.max(b0))
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn dist(mut v: Vec<f64>) -> Dist {
    Dist::of(&mut v)
}

/// Swap chains of other processes with this many presents or fewer are left out of the frame table.
pub const MIN_FRAMES: usize = 10;

/// Frames of system modules that only implement waiting; a wait site is the first frame after them.
const WAIT_PLUMBING: &[&str] = &[
    "ntoskrnl.exe",
    "ntdll.dll",
    "kernelbase.dll",
    "kernel32.dll",
    "win32u.dll",
    "win32kfull.sys",
    "win32kbase.sys",
    "win32k.sys",
    "user32.dll",
    "combase.dll",
];

fn top_rows(map: HashMap<String, (u64, f64)>, by_ms: bool, top: usize) -> Vec<NameRow> {
    let total: f64 = if by_ms { map.values().map(|v| v.1).sum() } else { map.values().map(|v| v.0 as f64).sum() };
    let mut rows: Vec<NameRow> = map
        .into_iter()
        .map(|(name, (count, ms))| {
            let part = if by_ms { ms } else { count as f64 };
            NameRow { name, count, ms, pct: if total > 0.0 { part / total * 100.0 } else { 0.0 } }
        })
        .collect();
    rows.sort_by(|a, b| {
        let (x, y) = if by_ms { (a.ms, b.ms) } else { (a.count as f64, b.count as f64) };
        y.total_cmp(&x).then_with(|| a.name.cmp(&b.name))
    });
    rows.truncate(top);
    rows
}

fn io_rows(map: HashMap<String, IoRow>, top: usize) -> Vec<IoRow> {
    let mut rows: Vec<IoRow> = map.into_values().collect();
    rows.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms).then_with(|| b.bytes.cmp(&a.bytes)).then_with(|| a.name.cmp(&b.name)));
    rows.truncate(top);
    rows
}

fn add_io(map: &mut HashMap<String, IoRow>, name: String, bytes: u64, ns: u64) {
    let r = map.entry(name.clone()).or_insert_with(|| IoRow { name, ..Default::default() });
    r.count += 1;
    r.bytes += bytes;
    r.total_ms += ms(ns);
    r.max_ms = r.max_ms.max(ms(ns));
}

/// Picks the focus process: the one asked for, else the process presenting the most frames (not
/// the desktop compositor), else the busiest process.
fn pick_focus(sys: &SysEvents, focus: &Option<Focus>, cpu_by_pid: &HashMap<u32, u64>) -> Option<u32> {
    let names = |pid: u32| sys.process_name(pid);
    match focus {
        Some(Focus::Pid(p)) => return Some(*p),
        Some(Focus::Name(n)) => {
            let want = n.to_ascii_lowercase();
            let want_exe = if want.ends_with(".exe") { want.clone() } else { format!("{want}.exe") };
            // The busiest process with that name.
            let mut best: Option<(u32, u64)> = None;
            for p in &sys.processes {
                let name = p.name.to_ascii_lowercase();
                if name == want || name == want_exe {
                    let c = cpu_by_pid.get(&p.pid).copied().unwrap_or(0) + 1;
                    if best.is_none_or(|b| c > b.1) {
                        best = Some((p.pid, c));
                    }
                }
            }
            return best.map(|b| b.0);
        }
        None => {}
    }
    let mut presents: HashMap<u32, usize> = HashMap::new();
    for p in &sys.presents {
        *presents.entry(p.pid).or_default() += 1;
    }
    if let Some((pid, _)) = presents
        .iter()
        .filter(|(pid, _)| !names(**pid).eq_ignore_ascii_case("dwm.exe"))
        .max_by_key(|(pid, n)| (**n, std::cmp::Reverse(**pid)))
    {
        return Some(*pid);
    }
    cpu_by_pid
        .iter()
        .filter(|(pid, _)| **pid != 0 && **pid != 4)
        .max_by_key(|(pid, c)| (**c, std::cmp::Reverse(**pid)))
        .map(|(p, _)| *p)
}

/// Frame statistics of one swap chain from its presents' start times.
pub fn frame_stats(times_ns: &[u64], present_ns: &[u64], stutter_factor: f64) -> FrameStats {
    let ft: Vec<f64> = times_ns.windows(2).map(|w| ms(w[1].saturating_sub(w[0]))).collect();
    let n = ft.len();
    let mut s = FrameStats { frames: n, ..Default::default() };
    if n == 0 {
        return s;
    }
    s.seconds = ms(times_ns[times_ns.len() - 1] - times_ns[0]) / 1000.0;
    s.avg_fps = if s.seconds > 0.0 { n as f64 / s.seconds } else { 0.0 };
    let mut sorted = ft.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let fps = |m: f64| if m > 0.0 { 1000.0 / m } else { 0.0 };
    let d = Dist::of(&mut sorted.clone());
    s.min_fps = fps(d.max);
    s.low_1_fps = fps(d.p99);
    s.low_01_fps = fps(d.p999);
    let avg_of_slowest = |share: f64| -> f64 {
        let k = ((n as f64 * share).ceil() as usize).clamp(1, n);
        sorted[n - k..].iter().sum::<f64>() / k as f64
    };
    s.low_1_avg_fps = fps(avg_of_slowest(0.01));
    s.low_01_avg_fps = fps(avg_of_slowest(0.001));
    s.stutter_threshold_ms = d.p50 * stutter_factor;
    s.stutters = ft.iter().filter(|f| **f > s.stutter_threshold_ms).count();
    s.frame_ms = d;
    s.present_ms = dist(present_ns.iter().map(|p| ms(*p)).collect());
    s
}

pub fn analyze(trace: &Trace, opts: &Options, namer: &mut dyn Namer, pm: Option<&PresentMonFrames>) -> Report {
    let sys = &trace.sys;
    let from = opts.from_ns;
    let end_of_trace = trace
        .duration_ns
        .max(sys.switches.last().map(|s| s.t).unwrap_or(0))
        .max(trace.events.last().map(|e| e.start_ns + e.elapsed_ns).unwrap_or(0));
    let to = opts.to_ns.min(end_of_trace).max(from + 1);
    let span = to - from;
    let cpu_count = trace.cpu_count.max(sys.switches.iter().map(|s| s.cpu as usize + 1).max().unwrap_or(0)).max(1);
    let machine_ns = span as f64 * cpu_count as f64;
    let inside = |t: u64| t >= from && t < to;
    let mut notes = Vec::new();

    // Bucketed series for the timeline.
    let bucket_ns = (span / 600).max(1_000_000);
    let buckets = span.div_ceil(bucket_ns) as usize;
    let mut tl_busy = vec![0u64; buckets];
    let mut tl_focus = vec![0u64; buckets];
    let mut tl_dpc = vec![0u64; buckets];
    let mut tl_faults = vec![0u32; buckets];
    let mut tl_disk = vec![0u64; buckets];
    let spread = |series: &mut Vec<u64>, a: u64, b: u64| {
        let (a, b) = (a.max(from), b.min(to));
        if b <= a {
            return;
        }
        let mut t = a;
        while t < b {
            let i = ((t - from) / bucket_ns) as usize;
            let edge = from + (i as u64 + 1) * bucket_ns;
            let piece = edge.min(b) - t;
            if let Some(v) = series.get_mut(i) {
                *v += piece;
            }
            t += piece;
        }
    };

    // Pass 1: precise CPU usage per thread and per CPU from context switches. Like xperf's
    // `-a cswitch` and WPA, only complete intervals count: from a thread's switch-in to its
    // switch-out on the same CPU, so nothing before a CPU's first switch or after its last.
    let mut on_cpu: HashMap<u16, (u32, u32, u64)> = HashMap::new();
    let mut cpu_by_tid: HashMap<u32, u64> = HashMap::new();
    let mut cpu_by_pid: HashMap<u32, u64> = HashMap::new();
    let mut busy_by_cpu: HashMap<u16, u64> = HashMap::new();
    let mut switches_by_cpu: HashMap<u16, u64> = HashMap::new();
    let mut switch_ins_tid: HashMap<u32, u64> = HashMap::new();
    let mut tid_pid: HashMap<u32, u32> = HashMap::new();
    let mut running: Vec<(u32, u32, u16, u64, u64)> = Vec::new(); // tid, pid, cpu, start, end
    for s in &sys.switches {
        if let Some((tid, pid, since)) = on_cpu.get(&s.cpu).copied() {
            if s.t > since {
                running.push((tid, pid, s.cpu, since, s.t));
            }
        }
        on_cpu.insert(s.cpu, (s.new_tid, s.new_pid, s.t));
        if inside(s.t) {
            *switches_by_cpu.entry(s.cpu).or_default() += 1;
            *switch_ins_tid.entry(s.new_tid).or_default() += 1;
        }
        tid_pid.insert(s.new_tid, s.new_pid);
        tid_pid.insert(s.old_tid, s.old_pid);
    }
    for &(tid, pid, cpu, a, b) in &running {
        let o = overlap(a, b, from, to);
        if o == 0 {
            continue;
        }
        *cpu_by_tid.entry(tid).or_default() += o;
        *cpu_by_pid.entry(pid).or_default() += o;
        if tid != 0 {
            *busy_by_cpu.entry(cpu).or_default() += o;
            spread(&mut tl_busy, a, b);
        }
    }

    let focus = pick_focus(sys, &opts.focus, &cpu_by_pid);
    if opts.focus.is_some() && focus.is_none() {
        notes.push("the requested process was not found in the trace".to_string());
    }
    let focus_name = focus.map(|p| sys.process_name(p));
    if let Some(f) = focus {
        for &(_, pid, _, a, b) in &running {
            if pid == f {
                spread(&mut tl_focus, a, b);
            }
        }
    }

    // Pass 2: ready and wait time, from ready events and switches in time order.
    let mut ready_since: HashMap<u32, u64> = HashMap::new();
    let mut wait_since: HashMap<u32, (u64, u8)> = HashMap::new();
    let mut ready_by_pid: HashMap<u32, Vec<f64>> = HashMap::new();
    let mut ready_by_tid: HashMap<u32, Vec<f64>> = HashMap::new();
    let mut waits: HashMap<u8, WaitRow> = HashMap::new();
    let mut wait_by_tid: HashMap<u32, HashMap<u8, f64>> = HashMap::new();
    let mut preempt: HashMap<String, (u64, f64)> = HashMap::new();
    let mut spans: HashMap<u32, Vec<Span>> = HashMap::new();
    let mut wait_sites: HashMap<String, (u64, f64)> = HashMap::new();
    let is_focus = |pid: u32| focus == Some(pid);
    let (mut ri, mut si) = (0usize, 0usize);
    let (readies, switches) = (&sys.readies, &sys.switches);
    let mut on_cpu_since: HashMap<u16, (u32, u32, u64)> = HashMap::new();
    while ri < readies.len() || si < switches.len() {
        let take_ready = si >= switches.len() || (ri < readies.len() && readies[ri].t <= switches[si].t);
        if take_ready {
            let r = &readies[ri];
            ri += 1;
            if let Some((w, reason)) = wait_since.remove(&r.tid) {
                if is_focus(r.pid) && r.t > w {
                    spans.entry(r.tid).or_default().push(Span { start: w, end: r.t, state: State::Waiting(reason) });
                    let o = overlap(w, r.t, from, to);
                    if o > 0 {
                        let row = waits
                            .entry(reason)
                            .or_insert_with(|| WaitRow { reason: wait_reason(reason).to_string(), ..Default::default() });
                        row.count += 1;
                        row.total_ms += ms(o);
                        row.max_ms = row.max_ms.max(ms(r.t - w));
                        *wait_by_tid.entry(r.tid).or_default().entry(reason).or_default() += ms(o);
                    }
                }
            }
            ready_since.entry(r.tid).or_insert(r.t);
            continue;
        }
        let s = &switches[si];
        si += 1;
        // The old thread stops running.
        if let Some((_, _, since)) = on_cpu_since.insert(s.cpu, (s.new_tid, s.new_pid, s.t)) {
            if is_focus(s.old_pid) && s.t > since {
                spans.entry(s.old_tid).or_default().push(Span { start: since, end: s.t, state: State::Running(s.cpu) });
            }
        }
        if s.old_tid != 0 {
            match s.old_state {
                STATE_WAITING => {
                    wait_since.insert(s.old_tid, (s.t, s.wait_reason));
                }
                STATE_READY => {
                    // Preempted: ready again at once.
                    ready_since.insert(s.old_tid, s.t);
                    if is_focus(s.old_pid) && !is_focus(s.new_pid) && inside(s.t) {
                        let e = preempt.entry(sys.process_name(s.new_pid)).or_default();
                        e.0 += 1;
                    }
                }
                _ => {}
            }
        }
        // The new thread starts running.
        if let Some(r) = ready_since.remove(&s.new_tid) {
            if s.t >= r {
                if is_focus(s.new_pid) && s.t > r {
                    spans.entry(s.new_tid).or_default().push(Span { start: r, end: s.t, state: State::Ready });
                }
                if inside(s.t) {
                    let us = (s.t - r) as f64 / 1e3;
                    ready_by_pid.entry(s.new_pid).or_default().push(us);
                    ready_by_tid.entry(s.new_tid).or_default().push(us);
                }
            }
        } else if let Some((w, reason)) = wait_since.remove(&s.new_tid) {
            // Woken without a ready event in the trace: count the wait up to now.
            if is_focus(s.new_pid) && s.t > w {
                spans.entry(s.new_tid).or_default().push(Span { start: w, end: s.t, state: State::Waiting(reason) });
            }
        }
        if is_focus(s.new_pid) && s.stack != NO_STACK && inside(s.t) {
            // Where it had been waiting: the first frame past the wait plumbing.
            if let Some(stack) = sys.stacks.get(s.stack as usize) {
                let site = stack.iter().map(|a| (namer.module(s.new_pid, *a), *a)).find(|(m, _)| {
                    let m = m.to_ascii_lowercase();
                    !WAIT_PLUMBING.contains(&m.as_str()) && m != "unknown"
                });
                if let Some((_, addr)) = site {
                    let e = wait_sites.entry(namer.function(s.new_pid, addr)).or_default();
                    e.0 += 1;
                }
            }
        }
    }

    // Samples.
    let interval_ns = sys.sample_interval_ns.unwrap_or(1_000_000);
    let mut samples_by_pid: HashMap<u32, u64> = HashMap::new();
    let mut samples_by_tid: HashMap<u32, u64> = HashMap::new();
    let mut modules: HashMap<String, (u64, f64)> = HashMap::new();
    let mut functions: HashMap<String, (u64, f64)> = HashMap::new();
    let mut inclusive: HashMap<String, (u64, f64)> = HashMap::new();
    let (mut n_samples, mut with_stack) = (0u64, 0u64);
    let mut focus_samples = 0u64;
    for s in sys.samples.iter().filter(|s| inside(s.t)) {
        n_samples += 1;
        if s.stack != NO_STACK {
            with_stack += 1;
        }
        *samples_by_pid.entry(s.pid).or_default() += 1;
        *samples_by_tid.entry(s.tid).or_default() += 1;
        if s.pid == 0 {
            continue;
        }
        let pname = sys.process_name(s.pid);
        let m = modules.entry(format!("{pname} {}", namer.module(s.pid, s.ip))).or_default();
        m.0 += 1;
        m.1 += ms(interval_ns);
        let keep = focus.is_none_or(|f| f == s.pid);
        if keep {
            let label =
                if focus.is_some() { namer.function(s.pid, s.ip) } else { format!("{pname} {}", namer.function(s.pid, s.ip)) };
            let f = functions.entry(label).or_default();
            f.0 += 1;
            f.1 += ms(interval_ns);
        }
        if focus == Some(s.pid) && s.stack != NO_STACK {
            focus_samples += 1;
            let mut seen: HashSet<String> = HashSet::new();
            for a in &sys.stacks[s.stack as usize] {
                let name = namer.function(s.pid, *a);
                if seen.insert(name.clone()) {
                    let e = inclusive.entry(name).or_default();
                    e.0 += 1;
                    e.1 += ms(interval_ns);
                }
            }
        }
    }

    // Hard faults and disk.
    let mut faults_by_process: HashMap<String, IoRow> = HashMap::new();
    let mut faults_by_file: HashMap<String, IoRow> = HashMap::new();
    let mut faults_by_pid: HashMap<u32, u64> = HashMap::new();
    for h in sys.hard_faults.iter().filter(|h| inside(h.t)) {
        add_io(&mut faults_by_process, sys.process_name(h.pid), h.bytes as u64, h.elapsed_ns);
        let file = sys.file_names.get(&h.file).cloned().unwrap_or_else(|| format!("file object 0x{:x}", h.file));
        add_io(&mut faults_by_file, file, h.bytes as u64, h.elapsed_ns);
        *faults_by_pid.entry(h.pid).or_default() += 1;
        if let Some(v) = tl_faults.get_mut(((h.t - from) / bucket_ns) as usize) {
            *v += 1;
        }
    }
    let mut disk_by_process: HashMap<String, IoRow> = HashMap::new();
    let mut disk_by_file: HashMap<String, IoRow> = HashMap::new();
    let mut disk_bytes_pid: HashMap<u32, u64> = HashMap::new();
    let mut disk_lat = Vec::new();
    for d in sys.disk_ios.iter().filter(|d| inside(d.t)) {
        let who = if d.pid == u32::MAX { "unknown".to_string() } else { sys.process_name(d.pid) };
        add_io(&mut disk_by_process, who, d.bytes as u64, d.elapsed_ns);
        let file = sys.file_names.get(&d.file).cloned().unwrap_or_else(|| {
            if d.file == 0 {
                "(no file)".into()
            } else {
                format!("file object 0x{:x}", d.file)
            }
        });
        add_io(&mut disk_by_file, file, d.bytes as u64, d.elapsed_ns);
        *disk_bytes_pid.entry(d.pid).or_default() += d.bytes as u64;
        disk_lat.push(ms(d.elapsed_ns));
        if let Some(v) = tl_disk.get_mut(((d.t - from) / bucket_ns) as usize) {
            *v += d.bytes as u64;
        }
    }

    // DPC/ISR per CPU and on the timeline.
    let mut dpc_by_cpu: HashMap<u16, u64> = HashMap::new();
    for e in &trace.events {
        let o = overlap(e.start_ns, e.start_ns + e.elapsed_ns, from, to);
        if o > 0 {
            *dpc_by_cpu.entry(e.cpu).or_default() += o;
            spread(&mut tl_dpc, e.start_ns, e.start_ns + e.elapsed_ns);
        }
    }

    // Process table.
    let mut threads_of: HashMap<u32, HashSet<u32>> = HashMap::new();
    for (tid, pid) in &tid_pid {
        threads_of.entry(*pid).or_default().insert(*tid);
    }
    let mut pids: HashSet<u32> = cpu_by_pid.keys().copied().collect();
    pids.extend(samples_by_pid.keys());
    pids.extend(faults_by_pid.keys());
    pids.extend(disk_bytes_pid.keys().filter(|p| **p != u32::MAX));
    let mut processes: Vec<ProcessRow> = pids
        .into_iter()
        .filter(|p| *p != u32::MAX)
        .map(|pid| {
            let cpu = cpu_by_pid.get(&pid).copied().unwrap_or(0);
            let ready: f64 = ready_by_pid.get(&pid).map(|v| v.iter().sum::<f64>() / 1e3).unwrap_or(0.0);
            let ths = threads_of.get(&pid);
            ProcessRow {
                pid,
                name: sys.process_name(pid),
                cpu_ms: ms(cpu),
                cpu_pct: cpu as f64 / machine_ns * 100.0,
                samples: samples_by_pid.get(&pid).copied().unwrap_or(0),
                switches_in: ths.map(|t| t.iter().map(|t| switch_ins_tid.get(t).copied().unwrap_or(0)).sum()).unwrap_or(0),
                threads: ths.map(|t| t.len()).unwrap_or(0),
                ready_ms: ready,
                hard_faults: faults_by_pid.get(&pid).copied().unwrap_or(0),
                disk_bytes: disk_bytes_pid.get(&pid).copied().unwrap_or(0),
            }
        })
        .collect();
    processes
        .sort_by(|a, b| b.cpu_ms.total_cmp(&a.cpu_ms).then_with(|| b.samples.cmp(&a.samples)).then_with(|| a.pid.cmp(&b.pid)));

    let mut cpus: Vec<CpuRow> = (0..cpu_count as u16)
        .map(|c| CpuRow {
            cpu: c,
            busy_pct: busy_by_cpu.get(&c).copied().unwrap_or(0) as f64 / span as f64 * 100.0,
            switches: switches_by_cpu.get(&c).copied().unwrap_or(0),
            dpc_isr_pct: dpc_by_cpu.get(&c).copied().unwrap_or(0) as f64 / span as f64 * 100.0,
        })
        .collect();
    if sys.switches.is_empty() {
        cpus.iter_mut().for_each(|c| c.busy_pct = f64::NAN);
    }

    // Threads of the focus process.
    let mut threads: Vec<ThreadRow> = Vec::new();
    if let Some(f) = focus {
        if let Some(tids) = threads_of.get(&f) {
            for tid in tids {
                let ready = ready_by_tid.get(tid).cloned().unwrap_or_default();
                let waits = wait_by_tid.get(tid);
                let top_wait = waits
                    .and_then(|w| {
                        w.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(r, m)| format!("{} {:.1} ms", wait_reason(*r), m))
                    })
                    .unwrap_or_default();
                threads.push(ThreadRow {
                    tid: *tid,
                    name: sys.thread_name(*tid).unwrap_or("").to_string(),
                    cpu_ms: ms(cpu_by_tid.get(tid).copied().unwrap_or(0)),
                    samples: samples_by_tid.get(tid).copied().unwrap_or(0),
                    switches_in: switch_ins_tid.get(tid).copied().unwrap_or(0),
                    ready_ms: ready.iter().sum::<f64>() / 1e3,
                    ready_us: dist(ready),
                    wait_ms: waits.map(|w| w.values().sum()).unwrap_or(0.0),
                    top_wait,
                });
            }
        }
        threads.sort_by(|a, b| b.cpu_ms.total_cmp(&a.cpu_ms).then_with(|| a.tid.cmp(&b.tid)));
        threads.truncate(opts.top);
    }

    let mut ready: Vec<ReadyRow> = ready_by_pid
        .into_iter()
        .filter(|(pid, _)| *pid != 0 && *pid != u32::MAX)
        .map(|(pid, v)| ReadyRow {
            name: format!("{} ({pid})", sys.process_name(pid)),
            count: v.len() as u64,
            total_ms: v.iter().sum::<f64>() / 1e3,
            us: dist(v),
        })
        .collect();
    ready.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms).then_with(|| a.name.cmp(&b.name)));
    ready.truncate(opts.top);

    let mut wait_rows: Vec<WaitRow> = waits.into_values().collect();
    wait_rows.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms));

    // Frames.
    let mut chains: BTreeMap<(u32, u64), Vec<&super::sys::Present>> = BTreeMap::new();
    for p in sys.presents.iter().filter(|p| inside(p.t)) {
        chains.entry((p.pid, p.swap_chain)).or_default().push(p);
    }
    let mut frames: Vec<FrameStats> = Vec::new();
    for ((pid, chain), ps) in &chains {
        // Swap chains that presented only a handful of times (a window that redrew once) are noise.
        if ps.len() < 2 || (ps.len() <= MIN_FRAMES && focus != Some(*pid)) {
            continue;
        }
        let times: Vec<u64> = ps.iter().map(|p| p.t).collect();
        let inside_present: Vec<u64> = ps.iter().filter(|p| p.end >= p.t && p.end != 0).map(|p| p.end - p.t).collect();
        let mut st = frame_stats(&times, &inside_present, opts.stutter_factor);
        let mut by_tid: HashMap<u32, usize> = HashMap::new();
        for p in ps {
            *by_tid.entry(p.tid).or_default() += 1;
        }
        st.pid = *pid;
        st.process = sys.process_name(*pid);
        st.runtime = ps[0].runtime.label().to_string();
        st.swap_chain = *chain;
        st.present_tid = by_tid.into_iter().max_by_key(|(t, n)| (*n, std::cmp::Reverse(*t))).map(|x| x.0).unwrap_or(0);
        st.sync_interval = ps[ps.len() - 1].sync_interval;
        frames.push(st);
    }
    frames.sort_by(|a, b| {
        let fa = focus == Some(a.pid);
        let fb = focus == Some(b.pid);
        fb.cmp(&fa).then_with(|| b.frames.cmp(&a.frames)).then_with(|| a.swap_chain.cmp(&b.swap_chain))
    });

    // Explanations for the focus process's busiest swap chain.
    let mut explained = Vec::new();
    let mut tl_frames = Vec::new();
    if let Some(main) = frames.iter().find(|f| Some(f.pid) == focus) {
        let ps = &chains[&(main.pid, main.swap_chain)];
        let mut ft: Vec<(usize, f64)> = ps.windows(2).enumerate().map(|(i, w)| (i + 1, ms(w[1].t - w[0].t))).collect();
        tl_frames = ps.windows(2).map(|w| (ms(w[1].t), ms(w[1].t - w[0].t))).collect();
        ft.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        for &(i, frame_ms) in ft.iter().take(opts.explain) {
            let (t0, t1) = (ps[i - 1].t, ps[i].t);
            explained.push(explain(trace, &spans, focus.unwrap_or(0), ps[i].tid, t0, t1, frame_ms, main.frame_ms.p50, namer, pm));
        }
        explained.sort_by(|a, b| a.at_ms.total_cmp(&b.at_ms));
    }

    // Notes on what the trace could not answer.
    if sys.switches.is_empty() {
        notes.push("no context switch events: record with the `game` or `cpu` preset (CSWITCH) for CPU usage (precise), ready time and waits".into());
    }
    if sys.samples.is_empty() {
        notes.push("no CPU samples: record with PROFILE for the sampled CPU tables".into());
    } else if with_stack == 0 {
        notes.push("CPU samples have no stacks: record with `--stackwalk Profile` for inclusive function times".into());
    }
    if sys.presents.is_empty() {
        notes.push("no Present events: record with the `game` preset (graphics providers) for frame times".into());
    }
    if trace.events_lost > 0 || trace.buffers_lost > 0 {
        notes.push(format!(
            "{} events and {} buffers were lost while recording: numbers are incomplete",
            trace.events_lost, trace.buffers_lost
        ));
    }

    let to_pct =
        |v: &Vec<u64>| -> Vec<f64> { v.iter().map(|ns| *ns as f64 / (bucket_ns as f64 * cpu_count as f64) * 100.0).collect() };
    let timeline = Timeline {
        bucket_ms: ms(bucket_ns),
        start_ms: ms(from),
        cpu_busy_pct: if sys.switches.is_empty() { Vec::new() } else { to_pct(&tl_busy) },
        focus_cpu_pct: if sys.switches.is_empty() || focus.is_none() { Vec::new() } else { to_pct(&tl_focus) },
        dpc_isr_pct: to_pct(&tl_dpc),
        hard_faults: tl_faults,
        disk_mb: tl_disk.iter().map(|b| *b as f64 / 1e6).collect(),
        frames: tl_frames,
    };

    let threads_total: HashSet<u32> = tid_pid.keys().copied().collect();
    Report {
        overview: Overview {
            duration_s: trace.duration_ns as f64 / 1e9,
            from_s: from as f64 / 1e9,
            to_s: to as f64 / 1e9,
            cpu_count,
            events_lost: trace.events_lost,
            buffers_lost: trace.buffers_lost,
            processes: sys.processes.len(),
            threads: sys.threads.len().max(threads_total.len()),
            switches: sys.switches.iter().filter(|s| inside(s.t)).count() as u64,
            readies: sys.readies.iter().filter(|s| inside(s.t)).count() as u64,
            samples: n_samples,
            sample_interval_ms: sys.sample_interval_ns.map(ms),
            samples_with_stacks_pct: if n_samples > 0 { with_stack as f64 / n_samples as f64 * 100.0 } else { 0.0 },
            dpc_isr: trace.events.iter().filter(|e| inside(e.start_ns)).count() as u64,
            hard_faults: sys.hard_faults.iter().filter(|h| inside(h.t)).count() as u64,
            disk_ios: sys.disk_ios.iter().filter(|d| inside(d.t)).count() as u64,
            presents: sys.presents.iter().filter(|p| inside(p.t)).count() as u64,
            focus: focus.map(|p| format!("{} ({p})", focus_name.clone().unwrap_or_default())),
            clock_zero: sys.zero_ticks,
            clock_freq: sys.freq,
        },
        processes,
        cpus,
        threads,
        sampled_modules: top_rows(modules, false, opts.top),
        sampled_functions: top_rows(functions, false, opts.top),
        inclusive_functions: {
            let mut rows = top_rows(inclusive, false, opts.top);
            for r in &mut rows {
                r.pct = if focus_samples > 0 { r.count as f64 / focus_samples as f64 * 100.0 } else { 0.0 };
            }
            rows
        },
        ready,
        waits: wait_rows,
        wait_sites: top_rows(wait_sites, false, opts.top),
        preempted_by: top_rows(preempt, false, opts.top),
        faults_by_process: io_rows(faults_by_process, opts.top),
        faults_by_file: io_rows(faults_by_file, opts.top),
        disk_by_process: io_rows(disk_by_process, opts.top),
        disk_by_file: io_rows(disk_by_file, opts.top),
        disk_ms: dist(disk_lat),
        frames,
        explained,
        timeline,
        notes,
    }
}

/// PresentMon's per-frame numbers, keyed by process and present start in trace time.
#[derive(Clone, Debug, Default)]
pub struct PresentMonFrames {
    /// (pid, present start ns, GPU busy ms).
    pub frames: Vec<(u32, u64, f64)>,
}

impl PresentMonFrames {
    /// The frame of `pid` whose present started within half a millisecond of `t`.
    pub fn gpu_busy(&self, pid: u32, t: u64) -> Option<f64> {
        let i = self.frames.partition_point(|f| f.1 + 500_000 < t);
        self.frames[i..].iter().take_while(|f| f.1 <= t + 500_000).find(|f| f.0 == pid).map(|f| f.2)
    }
}

#[allow(clippy::too_many_arguments)]
fn explain(
    trace: &Trace,
    spans: &HashMap<u32, Vec<Span>>,
    pid: u32,
    tid: u32,
    t0: u64,
    t1: u64,
    frame_ms: f64,
    median_ms: f64,
    namer: &mut dyn Namer,
    pm: Option<&PresentMonFrames>,
) -> FrameExplain {
    let sys = &trace.sys;
    let mut x = FrameExplain { at_ms: ms(t1), frame_ms, median_ms, tid, ..Default::default() };
    let mut waits: HashMap<u8, WaitRow> = HashMap::new();
    let mut its_cpus: Vec<(u16, u64, u64)> = Vec::new();
    for s in spans.get(&tid).into_iter().flatten() {
        let o = overlap(s.start, s.end, t0, t1);
        if o == 0 {
            continue;
        }
        match s.state {
            State::Running(cpu) => {
                x.running_ms += ms(o);
                its_cpus.push((cpu, s.start.max(t0), s.end.min(t1)));
            }
            State::Ready => x.ready_ms += ms(o),
            State::Waiting(r) => {
                x.waiting_ms += ms(o);
                let w = waits.entry(r).or_insert_with(|| WaitRow { reason: wait_reason(r).to_string(), ..Default::default() });
                w.count += 1;
                w.total_ms += ms(o);
                w.max_ms = w.max_ms.max(ms(o));
            }
        }
    }
    x.waits = waits.into_values().collect();
    x.waits.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms));
    for list in spans.values() {
        for s in list {
            if let State::Running(_) = s.state {
                x.process_cpu_ms += ms(overlap(s.start, s.end, t0, t1));
            }
        }
    }
    let map = ImageMap::new(&trace.images);
    let mut longest: Option<(u64, u64, u16)> = None;
    for e in &trace.events {
        if e.start_ns >= t1 {
            break;
        }
        let o = overlap(e.start_ns, e.start_ns + e.elapsed_ns, t0, t1);
        if o == 0 {
            continue;
        }
        x.dpc_isr_ms += ms(o);
        if its_cpus.iter().any(|(c, a, b)| *c == e.cpu && overlap(e.start_ns, e.start_ns + e.elapsed_ns, *a, *b) > 0) {
            x.dpc_isr_on_its_cpus_ms += ms(o);
        }
        if longest.is_none_or(|l| e.elapsed_ns > l.1) {
            longest = Some((e.routine, e.elapsed_ns, e.cpu));
        }
    }
    x.longest_dpc_isr = longest.map(|(r, ns, cpu)| (map.module(r), ms(ns) * 1000.0, cpu));
    for h in sys.hard_faults.iter().filter(|h| h.pid == pid && overlap(h.t, h.t + h.elapsed_ns, t0, t1) > 0) {
        x.hard_faults += 1;
        x.hard_fault_ms += ms(overlap(h.t, h.t + h.elapsed_ns, t0, t1));
    }
    x.disk_ios = sys.disk_ios.iter().filter(|d| d.pid == pid && d.t >= t0 && d.t < t1).count() as u64;
    let mut pre: HashMap<String, (u64, f64)> = HashMap::new();
    let lo = sys.switches.partition_point(|s| s.t < t0);
    for s in sys.switches[lo..].iter().take_while(|s| s.t < t1) {
        if s.old_pid == pid && s.new_pid != pid && s.old_state == STATE_READY {
            pre.entry(sys.process_name(s.new_pid)).or_default().0 += 1;
        }
    }
    x.preempted_by = top_rows(pre, false, 3);
    let mut funcs: HashMap<String, (u64, f64)> = HashMap::new();
    let lo = sys.samples.partition_point(|s| s.t < t0);
    let interval = sys.sample_interval_ns.unwrap_or(1_000_000);
    for s in sys.samples[lo..].iter().take_while(|s| s.t < t1).filter(|s| s.tid == tid) {
        let e = funcs.entry(namer.function(pid, s.ip)).or_default();
        e.0 += 1;
        e.1 += ms(interval);
    }
    x.top_functions = top_rows(funcs, false, 3);
    x.gpu_busy_ms = pm.and_then(|p| p.gpu_busy(pid, t1));
    x.verdict = verdict(&x);
    x
}

/// The single most likely reason a frame was slow, from its breakdown.
pub fn verdict(x: &FrameExplain) -> String {
    let f = x.frame_ms.max(1e-9);
    let top_wait = x.waits.first().map(|w| w.reason.as_str()).unwrap_or("unknown");
    if x.hard_fault_ms > 0.25 * f {
        return format!("disk: {} hard page faults stalled the process for {:.1} ms", x.hard_faults, x.hard_fault_ms);
    }
    if x.ready_ms > 0.25 * f {
        let who = x.preempted_by.first().map(|p| format!(", preempted by {}", p.name)).unwrap_or_default();
        return format!("CPU contention: the render thread was ready but not running for {:.1} ms{who}", x.ready_ms);
    }
    if x.dpc_isr_on_its_cpus_ms > 0.2 * f || x.longest_dpc_isr.as_ref().is_some_and(|l| l.1 > 1000.0) {
        let l =
            x.longest_dpc_isr.as_ref().map(|l| format!(" (longest {:.0} us in {} on CPU {})", l.1, l.0, l.2)).unwrap_or_default();
        return format!("drivers: {:.2} ms of DPC/ISR time on the render thread's CPUs{l}", x.dpc_isr_on_its_cpus_ms);
    }
    if x.running_ms > 0.6 * f {
        return format!("CPU bound: the render thread ran for {:.1} ms of the {:.1} ms frame", x.running_ms, x.frame_ms);
    }
    if let Some(g) = x.gpu_busy_ms {
        if g > 0.75 * f {
            return format!("GPU bound: the GPU was busy for {g:.1} ms of the frame");
        }
    }
    if x.waiting_ms > 0.5 * f {
        return format!(
            "waiting: the render thread waited {:.1} ms, mostly {top_wait} (GPU, vsync, a lock or another thread)",
            x.waiting_ms
        );
    }
    format!("mixed: running {:.1} ms, ready {:.1} ms, waiting {:.1} ms", x.running_ms, x.ready_ms, x.waiting_ms)
}

/// Collapsed stacks (`process;thread;outer;...;inner count`) of the samples, for flame graph tools
/// such as inferno, speedscope or flamegraph.pl.
pub fn folded(trace: &Trace, opts: &Options, focus: Option<u32>, namer: &mut dyn Namer) -> String {
    let sys = &trace.sys;
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for s in sys.samples.iter().filter(|s| s.t >= opts.from_ns && s.t < opts.to_ns && s.pid != 0) {
        if focus.is_some_and(|f| f != s.pid) {
            continue;
        }
        let mut parts = vec![sys.process_name(s.pid).replace(';', "_"), format!("thread {}", s.tid)];
        if s.stack != NO_STACK {
            let frames = &sys.stacks[s.stack as usize];
            parts.extend(frames.iter().rev().map(|a| namer.function(s.pid, *a).replace([';', ' '], "_")));
        } else {
            parts.push(namer.function(s.pid, s.ip).replace([';', ' '], "_"));
        }
        *counts.entry(parts.join(";")).or_default() += 1;
    }
    counts.into_iter().map(|(k, v)| format!("{k} {v}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::model::{Event, Kind};
    use crate::trace::sys::{HardFault, Present, Process, Ready, Runtime, Sample, Switch, Thread};

    const MS: u64 = 1_000_000;

    fn sw(t_ms: f64, cpu: u16, new: (u32, u32), old: (u32, u32), state: u8, reason: u8) -> Switch {
        Switch {
            t: (t_ms * 1e6) as u64,
            cpu,
            new_tid: new.0,
            new_pid: new.1,
            old_tid: old.0,
            old_pid: old.1,
            old_state: state,
            wait_reason: reason,
            stack: NO_STACK,
            ..Default::default()
        }
    }

    /// Game (pid 10, render thread 11) and another process (pid 20, thread 21) on two CPUs.
    fn scenario() -> Trace {
        let mut t = Trace { duration_ns: 100 * MS, cpu_count: 2, ..Default::default() };
        let s = &mut t.sys;
        s.processes = vec![
            Process { pid: 10, name: "game.exe".into(), ..Default::default() },
            Process { pid: 20, name: "other.exe".into(), ..Default::default() },
            Process { pid: 30, name: "dwm.exe".into(), ..Default::default() },
        ];
        s.threads = vec![Thread { tid: 11, pid: 10, name: "Render".into(), ..Default::default() }];
        let idle = (0, 0);
        let game = (11, 10);
        let other = (21, 20);
        s.switches = vec![
            // CPU 0: idle until 10, game runs 10..30, waits (13, WrUserRequest) until ready at 40,
            // runs 45..50 (ready 5 ms), preempted by other at 50, other runs 50..60, game 60..100.
            sw(0.0, 0, idle, (5, 5), STATE_WAITING, 6),
            sw(10.0, 0, game, idle, 2, 0),
            sw(30.0, 0, idle, game, STATE_WAITING, 13),
            sw(45.0, 0, game, idle, 2, 0),
            sw(50.0, 0, other, game, STATE_READY, 0),
            sw(60.0, 0, game, other, STATE_WAITING, 6),
            sw(100.0, 0, idle, game, STATE_WAITING, 6),
            // CPU 1: other runs 0..20 then idle.
            sw(0.0, 1, other, (6, 6), STATE_WAITING, 6),
            sw(20.0, 1, idle, other, STATE_WAITING, 6),
            sw(100.0, 1, (7, 7), idle, 2, 0),
        ];
        s.switches.sort_by_key(|x| x.t);
        s.readies = vec![Ready { t: 40 * MS, cpu: 0, tid: 11, pid: 10, by_tid: 0 }];
        // Presents of the game at 0, 10, 20, 70 ms (a 50 ms frame); dwm presents too.
        for (i, ms_) in [0u64, 10, 20, 70].iter().enumerate() {
            s.presents.push(Present {
                t: ms_ * MS,
                end: ms_ * MS + 100_000,
                pid: 10,
                tid: 11,
                swap_chain: 0xa,
                runtime: Runtime::Dxgi,
                ..Default::default()
            });
            let _ = i;
        }
        for ms_ in [0u64, 16, 33, 50, 66, 83] {
            s.presents.push(Present { t: ms_ * MS, pid: 30, tid: 31, swap_chain: 0xd, ..Default::default() });
        }
        s.presents.sort_by_key(|p| p.t);
        s.samples = (0..20).map(|i| Sample { t: (10 + i) * MS, cpu: 0, tid: 11, pid: 10, ip: 0x1010, stack: NO_STACK }).collect();
        s.user_images = vec![(10, Image { base: 0x1000, size: 0x100, name: "game.exe".into(), ..Default::default() })];
        s.hard_faults = vec![HardFault { t: 25 * MS, elapsed_ns: 2 * MS, tid: 11, pid: 10, file: 1, offset: 0, bytes: 4096 }];
        s.file_names.insert(1, r"C:\game\data.pak".into());
        t.events = vec![Event { kind: Kind::Dpc, cpu: 0, start_ns: 55 * MS, elapsed_ns: 2 * MS, routine: 0xfff0, vector: 0 }];
        t.images = vec![Image { base: 0xff00, size: 0x1000, name: "slow.sys".into(), ..Default::default() }];
        t
    }

    fn run(t: &Trace, o: &Options) -> Report {
        let mut n = PlainNamer::new(t);
        analyze(t, o, &mut n, None)
    }

    #[test]
    fn precise_cpu_time_per_process_and_cpu() {
        let r = run(&scenario(), &Options::default());
        let game = r.processes.iter().find(|p| p.pid == 10).unwrap();
        // 10..30, 45..50, 60..100 = 65 ms.
        assert!((game.cpu_ms - 65.0).abs() < 1e-9, "{}", game.cpu_ms);
        let other = r.processes.iter().find(|p| p.pid == 20).unwrap();
        // CPU 1 0..20 and CPU 0 50..60 = 30 ms.
        assert!((other.cpu_ms - 30.0).abs() < 1e-9);
        // 95 ms of 200 ms of CPU time.
        assert!((game.cpu_pct - 32.5).abs() < 1e-9);
        assert!((r.cpus[0].busy_pct - 75.0).abs() < 1e-9); // 10..30, 45..100
        assert!((r.cpus[1].busy_pct - 20.0).abs() < 1e-9);
        assert!((r.cpus[0].dpc_isr_pct - 2.0).abs() < 1e-9);
    }

    #[test]
    fn time_before_a_cpus_first_switch_and_after_its_last_is_not_counted() {
        let mut t = scenario();
        // Drop the opening and closing switches: CPU 1's other.exe 0..20 and game 60..100 are open.
        t.sys.switches.retain(|s| s.t != 0 && s.t != 100 * MS);
        let r = run(&t, &Options::default());
        let game = r.processes.iter().find(|p| p.pid == 10).unwrap();
        assert!((game.cpu_ms - 25.0).abs() < 1e-9, "{}", game.cpu_ms);
        let other = r.processes.iter().find(|p| p.pid == 20).unwrap();
        assert!((other.cpu_ms - 10.0).abs() < 1e-9);
    }

    #[test]
    fn ready_time_waits_and_preemption() {
        let r = run(&scenario(), &Options::default());
        assert_eq!(r.overview.focus.as_deref(), Some("game.exe (10)"));
        // Ready 40..45 after the wait, and 50..60 after being preempted.
        let game = r.ready.iter().find(|x| x.name.starts_with("game.exe")).unwrap();
        assert_eq!(game.count, 2);
        assert!((game.total_ms - 15.0).abs() < 1e-9);
        assert_eq!((game.us.min, game.us.max), (5000.0, 10000.0));
        // One wait: 30..40, WrUserRequest.
        assert_eq!(r.waits.len(), 1);
        assert_eq!(r.waits[0].reason, "WrUserRequest");
        assert!((r.waits[0].total_ms - 10.0).abs() < 1e-9);
        assert_eq!(r.preempted_by[0].name, "other.exe");
        assert_eq!(r.preempted_by[0].count, 1);
        let th = &r.threads[0];
        assert_eq!((th.tid, th.name.as_str()), (11, "Render"));
        assert!(th.top_wait.starts_with("WrUserRequest"));
    }

    #[test]
    fn frames_lows_and_stutters() {
        let r = run(&scenario(), &Options::default());
        let f = &r.frames[0];
        assert_eq!((f.pid, f.frames, f.present_tid), (10, 3, 11));
        // Frame times 10, 10, 50 ms over 70 ms.
        assert!((f.avg_fps - 3.0 / 0.07).abs() < 1e-9);
        assert!((f.min_fps - 20.0).abs() < 1e-9);
        assert!((f.low_1_avg_fps - 20.0).abs() < 1e-9);
        assert_eq!(f.stutters, 1);
        assert!((f.stutter_threshold_ms - 20.0).abs() < 1e-9);
        assert!((f.present_ms.mean - 0.1).abs() < 1e-9);
        // dwm presented only 6 times, so it is left out; the focus process never is.
        assert_eq!(r.frames.len(), 1);
    }

    #[test]
    fn the_slow_frame_is_explained() {
        let r = run(&scenario(), &Options { explain: 1, ..Default::default() });
        let x = &r.explained[0];
        // Frame 20..70 ms: running 20..30, 45..50, 60..70 = 25; waiting 30..40; ready 40..45, 50..60.
        assert!((x.frame_ms - 50.0).abs() < 1e-9);
        assert!((x.running_ms - 25.0).abs() < 1e-9, "{}", x.running_ms);
        assert!((x.ready_ms - 15.0).abs() < 1e-9);
        assert!((x.waiting_ms - 10.0).abs() < 1e-9);
        assert_eq!(x.waits[0].reason, "WrUserRequest");
        assert!((x.hard_fault_ms - 2.0).abs() < 1e-9);
        assert!((x.dpc_isr_ms - 2.0).abs() < 1e-9);
        // The DPC ran on CPU 0 while the other process had it, not the render thread.
        assert_eq!(x.dpc_isr_on_its_cpus_ms, 0.0);
        assert_eq!(x.longest_dpc_isr.as_ref().unwrap().0, "slow.sys");
        assert_eq!(x.preempted_by[0].name, "other.exe");
        assert!(x.verdict.starts_with("CPU contention"), "{}", x.verdict);
    }

    #[test]
    fn samples_hard_faults_and_window() {
        let r = run(&scenario(), &Options::default());
        assert_eq!(r.sampled_functions[0].name, "game.exe+0x10");
        assert_eq!(r.sampled_functions[0].count, 20);
        assert_eq!(r.faults_by_file[0].name, r"C:\game\data.pak");
        assert_eq!(r.faults_by_process[0].bytes, 4096);
        // A window that excludes everything but 60..100 ms.
        let w = run(&scenario(), &Options { from_ns: 60 * MS, to_ns: 100 * MS, ..Default::default() });
        let game = w.processes.iter().find(|p| p.pid == 10).unwrap();
        assert!((game.cpu_ms - 40.0).abs() < 1e-9);
        assert!(w.faults_by_file.is_empty());
        assert_eq!(w.sampled_functions.len(), 0);
    }

    #[test]
    fn focus_by_name_and_missing_data_notes() {
        let r = run(&scenario(), &Options { focus: Some(Focus::parse("other")), ..Default::default() });
        assert_eq!(r.overview.focus.as_deref(), Some("other.exe (20)"));
        let empty = run(&Trace::default(), &Options::default());
        assert!(empty.notes.iter().any(|n| n.contains("context switch")));
        assert!(empty.notes.iter().any(|n| n.contains("Present")));
        assert_eq!(Focus::parse("1234"), Focus::Pid(1234));
    }

    #[test]
    fn lows_use_the_slowest_frames() {
        // 99 frames of 10 ms and one of 100 ms.
        let mut times = vec![0u64];
        for i in 0..100 {
            let ft = if i == 50 { 100 } else { 10 };
            times.push(times.last().unwrap() + ft * MS);
        }
        let s = frame_stats(&times, &[], 2.0);
        assert_eq!(s.frames, 100);
        assert!((s.min_fps - 10.0).abs() < 1e-9);
        // The slowest 1% is that one frame.
        assert!((s.low_1_avg_fps - 10.0).abs() < 1e-9);
        // The 99th percentile interpolates between 10 and 100 ms.
        assert!((s.frame_ms.p99 - 10.9).abs() < 1e-9);
        assert_eq!(s.stutters, 1);
        assert!(frame_stats(&[5], &[], 2.0).frames == 0);
    }

    #[test]
    fn lows_round_the_slowest_share_up_and_the_threshold_is_exclusive() {
        // 150 frames: 1% is 1.5 frames, so the slowest 2 are averaged (40 and 20 ms).
        let mut times = vec![0u64];
        for i in 0..150 {
            let ft = match i {
                10 => 40,
                20 => 20,
                _ => 10,
            };
            times.push(times.last().unwrap() + ft * MS);
        }
        let s = frame_stats(&times, &[], 2.0);
        assert!((s.low_1_avg_fps - 1000.0 / 30.0).abs() < 1e-9, "{}", s.low_1_avg_fps);
        // The 20 ms frame is exactly 2x the 10 ms median: not a stutter.
        assert_eq!(s.stutters, 1);
    }

    #[test]
    fn a_thread_preempted_by_its_own_process_is_not_listed() {
        let mut t = scenario();
        // At 50 ms the render thread is preempted by another thread of the game instead.
        for s in &mut t.sys.switches {
            if s.t == 50 * MS {
                s.new_tid = 12;
                s.new_pid = 10;
            }
            if s.t == 60 * MS && s.cpu == 0 {
                s.old_tid = 12;
                s.old_pid = 10;
            }
        }
        let r = run(&t, &Options::default());
        assert!(r.preempted_by.is_empty(), "{:?}", r.preempted_by);
    }

    #[test]
    fn dpc_time_on_other_cpus_is_not_on_the_render_threads_cpus() {
        let mut t = scenario();
        // While the render thread runs on CPU 0 (60..100 ms), a DPC runs on CPU 1.
        t.events = vec![Event { kind: Kind::Dpc, cpu: 1, start_ns: 65 * MS, elapsed_ns: MS, routine: 0xfff0, vector: 0 }];
        let r = run(&t, &Options { explain: 1, ..Default::default() });
        let x = &r.explained[0];
        assert!((x.dpc_isr_ms - 1.0).abs() < 1e-9);
        assert_eq!(x.dpc_isr_on_its_cpus_ms, 0.0);
        // The same DPC on CPU 0 counts.
        t.events[0].cpu = 0;
        let r = run(&t, &Options { explain: 1, ..Default::default() });
        assert!((r.explained[0].dpc_isr_on_its_cpus_ms - 1.0).abs() < 1e-9);
    }

    #[test]
    fn folded_stacks_run_outermost_first() {
        let mut t = scenario();
        t.sys.stacks = vec![vec![0x1010, 0x1020]];
        t.sys.samples[0].stack = 0;
        let mut n = PlainNamer::new(&t);
        let f = folded(&t, &Options::default(), Some(10), &mut n);
        assert!(f.contains("game.exe;thread 11;game.exe+0x20;game.exe+0x10 1\n"), "{f}");
        assert!(f.contains("game.exe;thread 11;game.exe+0x10 19\n"));
    }

    #[test]
    fn verdicts_follow_the_largest_cause() {
        let base = FrameExplain { frame_ms: 40.0, ..Default::default() };
        assert!(verdict(&FrameExplain { hard_fault_ms: 15.0, hard_faults: 3, ..base.clone() }).starts_with("disk"));
        assert!(verdict(&FrameExplain { running_ms: 30.0, ..base.clone() }).starts_with("CPU bound"));
        assert!(verdict(&FrameExplain { gpu_busy_ms: Some(35.0), waiting_ms: 30.0, ..base.clone() }).starts_with("GPU bound"));
        assert!(verdict(&FrameExplain { waiting_ms: 30.0, ..base.clone() }).starts_with("waiting"));
        assert!(
            verdict(&FrameExplain { longest_dpc_isr: Some(("x.sys".into(), 1500.0, 2)), ..base.clone() }).starts_with("drivers")
        );
        assert!(verdict(&base).starts_with("mixed"));
    }

    #[test]
    fn presentmon_frames_match_within_half_a_millisecond() {
        let pm = PresentMonFrames { frames: vec![(10, 100 * MS, 5.0), (20, 100 * MS + 100_000, 7.0), (10, 200 * MS, 9.0)] };
        assert_eq!(pm.gpu_busy(10, 100 * MS + 300_000), Some(5.0));
        assert_eq!(pm.gpu_busy(20, 100 * MS), Some(7.0));
        assert_eq!(pm.gpu_busy(10, 150 * MS), None);
    }
}
