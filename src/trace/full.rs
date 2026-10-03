//! `benchlab trace analyze` and `benchlab trace export`: read a trace once and produce every
//! section of the report (text, JSON, HTML, folded stacks), with symbols and PresentMon when
//! available.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::analysis::{self, Namer, PlainNamer, PresentMonFrames, Report};
use super::model::{self, Analysis, AnalysisOptions, Trace};
use super::{etl, presentmon, render, render_sys};

/// Whether to run PresentMon over the trace for GPU, display and latency metrics.
#[derive(Clone, Debug, PartialEq)]
pub enum PresentMonChoice {
    /// When PresentMon is found and the trace has Present events.
    Auto,
    Off,
    Path(PathBuf),
}

#[derive(Clone, Debug)]
pub struct AnalyzeOptions {
    pub analysis: analysis::Options,
    pub sections: Vec<String>,
    pub symbols: bool,
    pub presentmon: PresentMonChoice,
    /// Where PresentMon's CSV goes (default: a temporary file).
    pub presentmon_csv: Option<PathBuf>,
    /// Also produce collapsed stacks for flame graphs.
    pub folded: bool,
    pub dpc_functions: bool,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        AnalyzeOptions {
            analysis: analysis::Options::default(),
            sections: render_sys::parse_sections(None).unwrap_or_default(),
            symbols: false,
            presentmon: PresentMonChoice::Auto,
            presentmon_csv: None,
            folded: false,
            dpc_functions: false,
        }
    }
}

pub struct Analyzed {
    pub report: Report,
    pub dpc: Analysis,
    pub presentmon: Option<presentmon::Summary>,
    /// Why PresentMon did not run or failed, when it was wanted.
    pub presentmon_note: Option<String>,
    pub presentmon_csv: Option<PathBuf>,
    pub folded: Option<String>,
    pub text: String,
}

/// Names with DbgHelp: one resolver for the kernel and one per process, created on first use.
#[cfg(windows)]
pub struct SymNamer {
    plain: PlainNamer,
    kernel: super::symbols::Resolver,
    kernel_floor: u64,
    users: std::collections::HashMap<u32, Option<super::symbols::Resolver>>,
    images: std::collections::HashMap<u32, Vec<model::Image>>,
}

#[cfg(windows)]
impl SymNamer {
    pub fn new(trace: &Trace) -> Result<SymNamer, String> {
        let mut images: std::collections::HashMap<u32, Vec<model::Image>> = std::collections::HashMap::new();
        for (pid, img) in &trace.sys.user_images {
            images.entry(*pid).or_default().push(img.clone());
        }
        Ok(SymNamer {
            plain: PlainNamer::new(trace),
            kernel: super::symbols::Resolver::new(&trace.images, None)?,
            kernel_floor: 0xffff_8000_0000_0000,
            users: std::collections::HashMap::new(),
            images,
        })
    }

    pub fn has_symbol_server_support(&self) -> bool {
        self.kernel.has_symbol_server_support()
    }

    pub fn skipped(&self) -> Vec<String> {
        let mut v = self.kernel.skipped.clone();
        for r in self.users.values().flatten() {
            v.extend(r.skipped.iter().cloned());
        }
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(windows)]
impl Namer for SymNamer {
    fn module(&mut self, pid: u32, addr: u64) -> String {
        self.plain.module(pid, addr)
    }

    fn function(&mut self, pid: u32, addr: u64) -> String {
        if addr >= self.kernel_floor {
            return self.kernel.resolve(addr);
        }
        let images = &self.images;
        let r =
            self.users.entry(pid).or_insert_with(|| images.get(&pid).and_then(|l| super::symbols::Resolver::new(l, None).ok()));
        match r {
            Some(r) => r.resolve(addr),
            None => self.plain.function(pid, addr),
        }
    }
}

/// A copy of the trace's DPC/ISR events restricted to the analysis window.
fn windowed(trace: &Trace, from: u64, to: u64) -> Trace {
    if from == 0 && to == u64::MAX {
        return Trace { events: trace.events.clone(), images: trace.images.clone(), ..shallow(trace) };
    }
    let to = to.min(trace.duration_ns.max(from + 1));
    Trace {
        events: trace
            .events
            .iter()
            .filter(|e| e.start_ns >= from && e.start_ns < to)
            .map(|e| model::Event { start_ns: e.start_ns - from, ..*e })
            .collect(),
        images: trace.images.clone(),
        duration_ns: to - from,
        ..shallow(trace)
    }
}

fn shallow(trace: &Trace) -> Trace {
    Trace {
        events: Vec::new(),
        images: Vec::new(),
        duration_ns: trace.duration_ns,
        cpu_count: trace.cpu_count,
        events_lost: trace.events_lost,
        buffers_lost: trace.buffers_lost,
        sys: Default::default(),
    }
}

fn run_presentmon(
    trace: &Trace,
    etl: &Path,
    o: &AnalyzeOptions,
) -> (Option<presentmon::Summary>, Option<PresentMonFrames>, Option<String>, Option<PathBuf>) {
    let exe =
        match &o.presentmon {
            PresentMonChoice::Off => return (None, None, None, None),
            PresentMonChoice::Auto if trace.sys.presents.is_empty() => return (None, None, None, None),
            PresentMonChoice::Auto => match presentmon::find(None) {
                Ok(p) => p,
                Err(_) => return (
                    None,
                    None,
                    Some(
                        "PresentMon was not found, so GPU busy, displayed time and latency are missing (pass --presentmon PATH)"
                            .into(),
                    ),
                    None,
                ),
            },
            PresentMonChoice::Path(p) => match presentmon::find(Some(p)) {
                Ok(p) => p,
                Err(e) => return (None, None, Some(e), None),
            },
        };
    let csv = o
        .presentmon_csv
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("benchlab-presentmon-{}.csv", std::process::id())));
    let text = match presentmon::run_on_etl(&exe, etl, &csv) {
        Ok(t) => t,
        Err(e) => return (None, None, Some(e), None),
    };
    let keep = o.presentmon_csv.is_some();
    if !keep {
        let _ = std::fs::remove_file(&csv);
    }
    let summary = presentmon::summarize(&text).ok();
    let mut frames: Vec<(u32, u64, f64)> = presentmon::qpc_frames(&text)
        .into_iter()
        .filter_map(|(pid, q, g)| trace.sys.ticks_to_ns(q).map(|t| (pid, t, g)))
        .collect();
    frames.sort_by_key(|f| f.1);
    (summary, Some(PresentMonFrames { frames }), None, keep.then_some(csv))
}

pub fn analyze_trace(trace: &Trace, etl_path: &Path, o: &AnalyzeOptions) -> Result<Analyzed, String> {
    let (summary, pm_frames, pm_note, pm_csv) = run_presentmon(trace, etl_path, o);

    #[cfg(windows)]
    let mut sym = if o.symbols {
        eprintln!("resolving symbols (the first run downloads them from the symbol server; set _NT_SYMBOL_PATH to change that)");
        let s = SymNamer::new(trace)?;
        if !s.has_symbol_server_support() {
            eprintln!("note: no dbghelp.dll with symsrv.dll was found (install the Debugging Tools for Windows or set BENCHLAB_DBGHELP), so only symbols already on disk can be used");
        }
        Some(s)
    } else {
        None
    };
    #[cfg(not(windows))]
    let mut sym: Option<PlainNamer> = None;
    let mut plain = PlainNamer::new(trace);
    let namer: &mut dyn Namer = match sym.as_mut() {
        Some(s) => s,
        None => &mut plain,
    };

    let report = analysis::analyze(trace, &o.analysis, namer, pm_frames.as_ref());
    let focus_pid = report
        .overview
        .focus
        .as_ref()
        .and_then(|f| f.rsplit('(').next())
        .and_then(|p| p.trim_end_matches(')').parse::<u32>().ok());
    let folded = o.folded.then(|| analysis::folded(trace, &o.analysis, focus_pid, namer));

    let win = windowed(trace, o.analysis.from_ns, o.analysis.to_ns);
    let dpc_opts = AnalysisOptions { by_function: o.dpc_functions, cpu: None, worst: 10 };
    let dpc = if o.symbols {
        let cell = RefCell::new(namer);
        let by = |addr: u64| cell.borrow_mut().function(0, addr);
        model::analyze(&win, &dpc_opts, Some(&by))
    } else {
        model::analyze(&win, &dpc_opts, None)
    };
    #[cfg(windows)]
    if let Some(s) = &sym {
        for x in s.skipped() {
            eprintln!("symbols skipped for {x}");
        }
    }

    let pm_text = summary.as_ref().map(render::render_presentmon);
    let dpc_text = if trace.events.is_empty() {
        None
    } else {
        // The DPC report without its own header line (the overview has the trace already).
        let full = render::render(&dpc, &etl_path.to_string_lossy(), o.analysis.top, None);
        Some(full.split_once('\n').map(|x| x.1.to_string()).unwrap_or(full))
    };
    let mut text = render_sys::render(&report, &etl_path.to_string_lossy(), &o.sections, pm_text.as_deref(), dpc_text.as_deref());
    if let Some(n) = &pm_note {
        if o.sections.iter().any(|s| s == "presentmon" || s == "overview") {
            text.push_str(&format!("\nnote: {n}\n"));
        }
    }
    Ok(Analyzed { report, dpc, presentmon: summary, presentmon_note: pm_note, presentmon_csv: pm_csv, folded, text })
}

pub fn analyze_file(etl_path: &Path, o: &AnalyzeOptions) -> Result<(Trace, Analyzed), String> {
    let trace = etl::read(etl_path)?;
    let a = analyze_trace(&trace, etl_path, o)?;
    Ok((trace, a))
}

/// Everything `analyze` produces, as one JSON document.
pub fn to_json(a: &Analyzed) -> String {
    #[derive(serde::Serialize)]
    struct All<'a> {
        report: &'a Report,
        dpc_isr: &'a Analysis,
        presentmon: Option<&'a presentmon::Summary>,
    }
    serde_json::to_string_pretty(&All { report: &a.report, dpc_isr: &a.dpc, presentmon: a.presentmon.as_ref() })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::model::{Event, Kind};

    #[test]
    fn the_window_shifts_dpc_events_to_its_start() {
        let t = Trace {
            events: vec![
                Event { kind: Kind::Dpc, cpu: 0, start_ns: 100, elapsed_ns: 5, routine: 1, vector: 0 },
                Event { kind: Kind::Dpc, cpu: 0, start_ns: 500, elapsed_ns: 5, routine: 1, vector: 0 },
            ],
            duration_ns: 1000,
            ..Default::default()
        };
        let w = windowed(&t, 400, 800);
        assert_eq!(w.events.len(), 1);
        assert_eq!((w.events[0].start_ns, w.duration_ns), (100, 400));
        assert_eq!(windowed(&t, 0, u64::MAX).events.len(), 2);
    }

    #[test]
    fn an_empty_trace_still_renders_and_has_json() {
        let t = Trace::default();
        let o = AnalyzeOptions { presentmon: PresentMonChoice::Off, ..Default::default() };
        let a = analyze_trace(&t, Path::new("x.etl"), &o).unwrap();
        assert!(a.text.contains("Trace: x.etl"));
        assert!(a.text.contains("note: no context switch events"));
        let j: serde_json::Value = serde_json::from_str(&to_json(&a)).unwrap();
        assert!(j.get("report").is_some() && j.get("dpc_isr").is_some());
    }
}
