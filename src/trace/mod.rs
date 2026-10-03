//! `benchlab trace`: record a Windows kernel trace with xperf, open it in WPA, and get a quick
//! DPC/ISR (and optional PresentMon) report without opening WPA at all.
//!
//! * [`xperf`] finds the Windows Performance Toolkit and builds its command lines.
//! * [`etl`] reads DPC, ISR and driver-load events from the `.etl` file itself.
//! * [`model`] turns those events into statistics.
//! * [`presentmon`] starts a PresentMon capture alongside the trace and summarises its CSV.
//! * [`render`] prints the report.

pub mod analysis;
pub mod etl;
pub mod export;
pub mod full;
pub mod html;
pub mod model;
pub mod presentmon;
pub mod render;
pub mod render_sys;
pub mod session;
pub mod symbols;
pub mod sys;
pub mod xperf;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use model::{analyze, Analysis, AnalysisOptions};

static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn install_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(_: u32) -> i32 {
        STOP.store(true, Ordering::SeqCst);
        1
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(not(windows))]
fn install_ctrl_c() {}

/// `YYYYMMDD-HHMMSS` in UTC, from seconds since the Unix epoch.
pub fn stamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86400) as i64;
    let rem = unix_secs % 86400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn now_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    stamp(secs)
}

/// A folder name that is safe on Windows.
pub fn sanitize_session(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty()
        || n == "."
        || n == ".."
        || n.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
    {
        return Err(format!("`{name}` is not a usable session name (no path separators or reserved characters)"));
    }
    Ok(n.to_string())
}

#[derive(Clone, Debug)]
pub struct RecordOptions {
    pub preset: String,
    pub flags: Option<String>,
    pub stackwalk: Option<String>,
    pub no_stacks: bool,
    pub delay_s: u64,
    pub timed_s: Option<u64>,
    pub out_dir: PathBuf,
    pub session: Option<String>,
    /// `Some(path)` enables PresentMon; an empty path means "find it".
    pub presentmon: Option<PathBuf>,
    pub process: Option<String>,
    pub force: bool,
    /// Resolve function names with DbgHelp (downloads symbols on first use).
    pub symbols: bool,
    pub analysis: AnalysisOptions,
    pub top: usize,
    /// Record the graphics providers too (`None`: as the preset says).
    pub graphics: Option<bool>,
    /// Focus process for the full report (default: the one presenting most frames).
    pub focus: Option<String>,
}

#[derive(Debug)]
pub struct Recorded {
    pub dir: PathBuf,
    pub etl: PathBuf,
    pub report: Option<PathBuf>,
    pub presentmon_csv: Option<PathBuf>,
    pub html: Option<PathBuf>,
    pub text: String,
}

fn need_admin() -> Result<(), String> {
    if xperf::is_elevated() {
        Ok(())
    } else {
        Err("recording a kernel trace needs an elevated terminal (run as administrator)".to_string())
    }
}

fn wait_with_progress(label: &str, secs: u64) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !STOP.load(Ordering::SeqCst) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        eprint!("\r{label}: {:>4} s left   ", left.as_secs() + 1);
        std::thread::sleep(Duration::from_millis(200).min(left));
    }
    eprintln!("\r{label}: done            ");
}

pub fn record(o: &RecordOptions) -> Result<Recorded, String> {
    let tools = xperf::find_tools()?;
    need_admin()?;
    let preset =
        xperf::preset(&o.preset).ok_or_else(|| format!("unknown preset `{}` (see `benchlab trace presets`)", o.preset))?;
    let flags = xperf::validate_flags(o.flags.as_deref().unwrap_or(preset.flags))?;
    let stackwalk = match (&o.stackwalk, o.no_stacks) {
        (_, true) => String::new(),
        (Some(s), false) => s.clone(),
        (None, false) if o.flags.is_none() => preset.stackwalk.to_string(),
        (None, false) => String::new(),
    };
    let session = match &o.session {
        Some(s) => sanitize_session(s)?,
        None => now_stamp(),
    };
    let dir = o.out_dir.join(&session);
    if dir.join("trace.etl").exists() {
        return Err(format!("{} already exists; pick another --session", dir.join("trace.etl").display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    let dir = PathBuf::from(dir.to_string_lossy().trim_start_matches(r"\\?\"));
    let raw = dir.join("kernel.etl");
    let etl = dir.join("trace.etl");
    let graphics = o.graphics.unwrap_or(preset.graphics && o.flags.is_none());
    let graphics_etl = dir.join("graphics.etl");
    let kernel_merged = dir.join("kernel-merged.etl");
    // A full report needs scheduling, samples or frames; a DPC-only trace gets the DPC report.
    let full_report = graphics
        || ["CSWITCH", "PROFILE", "DIAG", "LATENCY"].iter().any(|f| flags.to_ascii_uppercase().split('+').any(|x| x == *f));

    let loggers = xperf::run(&tools.xperf, &["-loggers".to_string()])?;
    if xperf::kernel_logger_running(&loggers.text) {
        if !o.force {
            return Err("an NT Kernel Logger session is already running (a trace someone else started?). Stop it with `benchlab trace stop`, or pass --force".to_string());
        }
        xperf::run(&tools.xperf, &["-stop".to_string()])?;
    }

    STOP.store(false, Ordering::SeqCst);
    install_ctrl_c();

    let mut pm_child = None;
    let mut pm_csv = None;
    // With the graphics providers in the trace, PresentMon analyses the trace afterwards instead.
    if let Some(path) = o.presentmon.as_ref().filter(|_| !graphics) {
        let exe = presentmon::find(if path.as_os_str().is_empty() { None } else { Some(path) })?;
        let csv = dir.join("presentmon.csv");
        let cap = presentmon::Capture {
            process: o.process.clone(),
            delay_s: o.delay_s,
            timed_s: o.timed_s.unwrap_or(0),
            session: format!("benchlab-{session}"),
        };
        pm_child = Some(presentmon::spawn(&exe, &cap, &csv)?);
        pm_csv = Some(csv);
    }
    let cleanup_pm = |c: &mut Option<std::process::Child>| {
        if let Some(c) = c.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    };

    if o.delay_s > 0 {
        wait_with_progress("starting in", o.delay_s);
        if STOP.load(Ordering::SeqCst) {
            cleanup_pm(&mut pm_child);
            return Err("interrupted before the trace started".to_string());
        }
    }

    let user = if graphics {
        match session::UserSession::start(GRAPHICS_SESSION, &graphics_etl) {
            Ok(s) => Some(s),
            Err(e) => {
                cleanup_pm(&mut pm_child);
                return Err(e);
            }
        }
    } else {
        None
    };
    let plan = xperf::StartPlan::new(&flags, &stackwalk, &raw);
    let started = xperf::run(&tools.xperf, &plan.args())?;
    if !started.ok {
        cleanup_pm(&mut pm_child);
        drop(user);
        return Err(format!("xperf could not start the trace: {}", started.text));
    }
    eprintln!("tracing {flags}{}", if stackwalk.is_empty() { String::new() } else { format!(" with stacks for {stackwalk}") });

    match o.timed_s {
        Some(s) => wait_with_progress("recording", s),
        None => {
            eprintln!("recording; press Enter or Ctrl+C to stop");
            std::thread::spawn(|| {
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                STOP.store(true, Ordering::SeqCst);
            });
            while !STOP.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    eprintln!("stopping and merging (this adds the driver and process information WPA needs)");
    let kernel_out = if graphics { &kernel_merged } else { &etl };
    let stopped = xperf::run(&tools.xperf, &xperf::stop_and_merge_args(kernel_out))?;
    if let Some(u) = user {
        u.stop()?;
    }
    if !kernel_out.is_file() {
        cleanup_pm(&mut pm_child);
        return Err(format!("xperf did not produce {}: {}", kernel_out.display(), stopped.text));
    }
    let _ = std::fs::remove_file(&raw);
    if graphics {
        let m = xperf::run(&tools.xperf, &xperf::merge_args(&[kernel_merged.clone(), graphics_etl.clone()], &etl))?;
        if !etl.is_file() {
            return Err(format!("xperf could not merge the graphics trace: {}", m.text));
        }
        let _ = std::fs::remove_file(&kernel_merged);
        let _ = std::fs::remove_file(&graphics_etl);
    }

    if let Some(mut c) = pm_child.take() {
        if o.timed_s.is_some() {
            // The capture ends by itself a moment after the trace.
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && c.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let _ = c.kill();
        let _ = c.wait();
    }

    if full_report {
        let ao = full::AnalyzeOptions {
            analysis: analysis::Options {
                focus: o.focus.as_deref().map(analysis::Focus::parse),
                top: o.top,
                ..Default::default()
            },
            symbols: o.symbols,
            presentmon: match &o.presentmon {
                Some(p) if !p.as_os_str().is_empty() => full::PresentMonChoice::Path(p.clone()),
                _ => full::PresentMonChoice::Auto,
            },
            presentmon_csv: Some(dir.join("presentmon.csv")),
            dpc_functions: o.analysis.by_function,
            ..Default::default()
        };
        let (trace, a) = full::analyze_file(&etl, &ao)?;
        let report = dir.join("report.txt");
        std::fs::write(&report, &a.text).map_err(|e| format!("{}: {e}", report.display()))?;
        let _ = std::fs::write(dir.join("report.json"), full::to_json(&a));
        let html = dir.join("report.html");
        let _ = std::fs::write(&html, html::render(&a, &trace, &etl.to_string_lossy()));
        return Ok(Recorded {
            dir,
            etl,
            report: Some(report),
            presentmon_csv: a.presentmon_csv.clone().or(pm_csv).filter(|p| p.is_file()),
            html: Some(html),
            text: a.text,
        });
    }

    let analysis = report_file(&etl, &o.analysis, o.symbols)?;
    let pm = match &pm_csv {
        Some(csv) => match std::fs::read_to_string(csv).map_err(|e| e.to_string()).and_then(|t| presentmon::summarize(&t)) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("PresentMon: {e}");
                None
            }
        },
        None => None,
    };
    let text = render::render(&analysis, &etl.to_string_lossy(), o.top, pm.as_ref());
    let report = dir.join("report.txt");
    std::fs::write(&report, &text).map_err(|e| format!("{}: {e}", report.display()))?;
    let json = dir.join("report.json");
    let _ = std::fs::write(&json, serde_json::to_string_pretty(&analysis).unwrap_or_default());
    Ok(Recorded { dir, etl, report: Some(report), presentmon_csv: pm_csv.filter(|p| p.is_file()), html: None, text })
}

/// Reads and analyses a trace file. With `symbols`, routines are named `driver!Function`.
pub fn report_file(etl: &Path, opts: &AnalysisOptions, symbols: bool) -> Result<Analysis, String> {
    let trace = etl::read(etl)?;
    if !symbols {
        return Ok(analyze(&trace, opts, None));
    }
    eprintln!("resolving symbols (the first run downloads them from the symbol server; set _NT_SYMBOL_PATH to change that)");
    let resolver = std::cell::RefCell::new(symbols::Resolver::new(&trace.images, None)?);
    if !resolver.borrow().has_symbol_server_support() {
        eprintln!("note: no dbghelp.dll with symsrv.dll was found (install the Debugging Tools for Windows or set BENCHLAB_DBGHELP), so only symbols already on disk can be used");
    }
    let by = |addr: u64| resolver.borrow_mut().resolve(addr);
    let a = analyze(&trace, opts, Some(&by));
    for s in &resolver.borrow().skipped {
        eprintln!("symbols skipped for {s}");
    }
    Ok(a)
}

pub fn merge(inputs: &[PathBuf], out: &Path) -> Result<String, String> {
    if inputs.len() < 2 {
        return Err("merging needs at least two traces".to_string());
    }
    for i in inputs {
        if !i.is_file() {
            return Err(format!("{}: no such file", i.display()));
        }
    }
    if inputs.iter().any(|i| i == out) {
        return Err("the output must not be one of the inputs".to_string());
    }
    let tools = xperf::find_tools()?;
    let r = xperf::run(&tools.xperf, &xperf::merge_args(inputs, out))?;
    if !r.ok || !out.is_file() {
        return Err(format!("xperf could not merge the traces: {}", r.text));
    }
    Ok(r.text)
}

/// Name of the graphics session `record` starts next to the kernel logger.
pub const GRAPHICS_SESSION: &str = "benchlab-graphics";

/// Stops a running kernel logger; with `out` it also merges what was recorded into that file.
pub fn stop(out: Option<&Path>) -> Result<String, String> {
    let tools = xperf::find_tools()?;
    need_admin()?;
    session::UserSession::stop_by_name(GRAPHICS_SESSION);
    let args = match out {
        Some(p) => xperf::stop_and_merge_args(p),
        None => vec!["-stop".to_string()],
    };
    let r = xperf::run(&tools.xperf, &args)?;
    if !r.ok && r.text.contains("0x1069") {
        return Ok("no kernel trace is running".to_string());
    }
    if !r.ok {
        return Err(r.text);
    }
    Ok(r.text)
}

pub fn open_wpa(etl: &Path, profile: Option<&Path>, symbols: bool) -> Result<(), String> {
    if !etl.is_file() {
        return Err(format!("{}: no such file", etl.display()));
    }
    let tools = xperf::find_tools()?;
    xperf::open_in_wpa(&tools, etl, profile, symbols)
}

/// What `benchlab trace check` prints.
pub fn check() -> String {
    let mut out = String::new();
    let mut line = |label: &str, value: String| out.push_str(&format!("{label:<22}{value}\n"));
    line("administrator", if xperf::is_elevated() { "yes".into() } else { "no (recording needs it, reports do not)".into() });
    match xperf::find_tools() {
        Ok(t) => {
            line("xperf", t.xperf.display().to_string());
            line("wpa", t.wpa.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()));
            if let Ok(l) = xperf::run(&t.xperf, &["-loggers".to_string()]) {
                line(
                    "NT Kernel Logger",
                    if xperf::kernel_logger_running(&l.text) {
                        "RUNNING (benchlab trace stop)".into()
                    } else {
                        "not running".into()
                    },
                );
            }
        }
        Err(e) => line("xperf", e),
    }
    line(
        "PresentMon",
        presentmon::find(None)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "not found (set PRESENTMON or pass --presentmon PATH)".into()),
    );
    out
}

pub fn presets_table() -> String {
    let w = xperf::PRESETS.iter().map(|p| p.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for p in xperf::PRESETS {
        out.push_str(&format!("{:<w$}  {}\n", p.name, p.about));
        if p.graphics {
            out.push_str(&format!("{:<w$}  graphics: DXGI, D3D9, DxgKrnl, Win32k and DWM events (a second session)\n", ""));
        }
        out.push_str(&format!("{:<w$}  flags: {}\n", "", p.flags));
        if !p.stackwalk.is_empty() {
            out.push_str(&format!("{:<w$}  stacks: {}\n", "", p.stackwalk));
        }
    }
    out
}

pub fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_dates() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(86_399), "19700101-235959");
        assert_eq!(stamp(951_782_400), "20000229-000000"); // leap day
        assert_eq!(stamp(1_791_000_000), "20261003-040000");
        assert_eq!(stamp(4_102_444_799), "20991231-235959");
    }

    #[test]
    fn session_names_cannot_escape_the_output_folder() {
        assert_eq!(sanitize_session("before-tweak").unwrap(), "before-tweak");
        assert_eq!(sanitize_session("  a b ").unwrap(), "a b");
        for bad in ["", " ", ".", "..", "a/b", r"a\b", "..\\x", "c:x", "a*b", "a|b", "a\nb"] {
            assert!(sanitize_session(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn merge_validates_before_running_anything() {
        assert!(merge(&[PathBuf::from("a.etl")], Path::new("o.etl")).unwrap_err().contains("at least two"));
        assert!(merge(&[PathBuf::from("nope1.etl"), PathBuf::from("nope2.etl")], Path::new("o.etl"))
            .unwrap_err()
            .contains("no such file"));
    }

    #[test]
    fn presets_table_lists_every_preset() {
        let t = presets_table();
        for p in xperf::PRESETS {
            assert!(t.contains(p.name) && t.contains(p.flags));
        }
    }
}
