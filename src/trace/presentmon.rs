//! PresentMon integration: starts a capture next to a trace and summarises its CSV (frame time,
//! CPU and GPU busy and wait, displayed time, latency). PresentMon is not bundled: point at it with
//! `--presentmon PATH`, the `PRESENTMON` environment variable, or put it on `PATH`.
//!
//! Both the 1.x column names (`msBetweenPresents`, ...) and the 2.x names (`FrameTime`, ...) are
//! understood.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};

use super::model::Dist;

/// A metric PresentMon reports, with the column names it has had.
///
/// 2.x writes `FrameTime`, `CPUBusy`, ... with `--v2_metrics` and `MsBetweenPresents`,
/// `MsCPUBusy`, ... by default; 1.x writes `msBetweenPresents`, ... Names match without case.
const METRICS: &[(&str, &[&str])] = &[
    ("Frame time", &["FrameTime", "msBetweenPresents"]),
    ("CPU busy", &["CPUBusy", "MsCPUBusy"]),
    ("CPU wait", &["CPUWait", "MsCPUWait"]),
    ("GPU latency", &["GPULatency", "MsGPULatency"]),
    ("GPU busy", &["GPUBusy", "MsGPUBusy"]),
    ("GPU wait", &["GPUWait", "MsGPUWait"]),
    ("GPU time", &["GPUTime", "MsGPUTime", "msGPUActive"]),
    ("Displayed time", &["DisplayedTime", "msBetweenDisplayChange"]),
    ("In present API", &["msInPresentAPI"]),
    ("Until displayed", &["DisplayLatency", "msUntilDisplayed"]),
    ("Input to photon", &["AllInputToPhotonLatency", "MsAllInputToPhotonLatency"]),
    ("Click to photon", &["ClickToPhotonLatency", "MsClickToPhotonLatency"]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    /// `-process_name` (PresentMon 1.x).
    Single,
    /// `--process_name` (PresentMon 2.x).
    Double,
}

impl Syntax {
    /// Decides from the first line of `PresentMon -help` output, e.g. `PresentMon 1.10.0`.
    pub fn from_help(help: &str) -> Syntax {
        let first = help.lines().find(|l| l.contains("PresentMon")).unwrap_or("");
        let ver = first.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit())).unwrap_or("1");
        match ver.split('.').next().and_then(|m| m.parse::<u32>().ok()) {
            Some(m) if m >= 2 => Syntax::Double,
            _ => Syntax::Single,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Capture {
    pub process: Option<String>,
    pub delay_s: u64,
    pub timed_s: u64,
    pub session: String,
}

pub fn args(c: &Capture, csv: &Path, syntax: Syntax) -> Vec<String> {
    let dash = match syntax {
        Syntax::Single => "-",
        Syntax::Double => "--",
    };
    let mut a: Vec<String> = Vec::new();
    let mut flag = |name: &str, value: Option<String>| {
        a.push(format!("{dash}{name}"));
        if let Some(v) = value {
            a.push(v);
        }
    };
    if let Some(p) = &c.process {
        flag("process_name", Some(p.clone()));
    }
    flag("output_file", Some(csv.to_string_lossy().into_owned()));
    if c.delay_s > 0 {
        flag("delay", Some(c.delay_s.to_string()));
    }
    if c.timed_s > 0 {
        flag("timed", Some(c.timed_s.to_string()));
        flag("terminate_after_timed", None);
    }
    flag("session_name", Some(if c.session.is_empty() { "benchlab".to_string() } else { c.session.clone() }));
    flag("stop_existing_session", None);
    if syntax == Syntax::Single {
        flag("no_top", None);
    }
    a
}

pub fn find(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return if p.is_file() { Ok(p.to_path_buf()) } else { Err(format!("{}: PresentMon not found", p.display())) };
    }
    if let Some(p) = std::env::var_os("PRESENTMON").map(PathBuf::from).filter(|p| p.is_file()) {
        return Ok(p);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                let mut hits: Vec<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                            n.to_ascii_lowercase().starts_with("presentmon") && n.to_ascii_lowercase().ends_with(".exe")
                        })
                    })
                    .collect();
                hits.sort();
                if let Some(h) = hits.pop() {
                    return Ok(h);
                }
            }
        }
    }
    Err("PresentMon.exe not found: pass --presentmon PATH, set PRESENTMON, or put it on PATH (https://github.com/GameTechDev/PresentMon)".to_string())
}

pub fn detect_syntax(exe: &Path) -> Syntax {
    let out = Command::new(exe).arg("-help").stdin(Stdio::null()).output();
    match out {
        Ok(o) => Syntax::from_help(&format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(_) => Syntax::Single,
    }
}

pub fn spawn(exe: &Path, c: &Capture, csv: &Path) -> Result<Child, String> {
    let syntax = detect_syntax(exe);
    Command::new(exe)
        .args(args(c, csv, syntax))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))
}

/// Arguments to analyse an existing trace: `--etl_file` with QPC timestamps, so every frame can be
/// placed on the trace's own time line.
pub fn etl_args(etl: &Path, csv: &Path, syntax: Syntax) -> Vec<String> {
    let dash = match syntax {
        Syntax::Single => "-",
        Syntax::Double => "--",
    };
    let mut a = vec![
        format!("{dash}etl_file"),
        etl.to_string_lossy().into_owned(),
        format!("{dash}output_file"),
        csv.to_string_lossy().into_owned(),
        format!("{dash}qpc_time"),
    ];
    a.push(match syntax {
        Syntax::Single => "-no_top".to_string(),
        Syntax::Double => "--no_console_stats".to_string(),
    });
    a
}

/// Runs PresentMon over a trace file and returns its CSV.
pub fn run_on_etl(exe: &Path, etl: &Path, csv: &Path) -> Result<String, String> {
    let syntax = detect_syntax(exe);
    let _ = std::fs::remove_file(csv);
    let out = Command::new(exe)
        .args(etl_args(etl, csv, syntax))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    std::fs::read_to_string(csv).map_err(|_| {
        let err = String::from_utf8_lossy(&out.stderr);
        format!("PresentMon wrote no CSV: {}", err.trim().lines().last().unwrap_or("no output"))
    })
}

/// Per-frame GPU busy time from a CSV written with `--qpc_time`: (pid, QPC value of the present,
/// GPU busy ms).
pub fn qpc_frames(csv: &str) -> Vec<(u32, i64, f64)> {
    let mut lines = csv.lines().filter(|l| !l.trim().is_empty());
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let header = split_csv(head.trim_start_matches('\u{feff}'));
    let col = |names: &[&str]| header.iter().position(|h| names.iter().any(|n| h.eq_ignore_ascii_case(n)));
    let (Some(pid), Some(t), Some(gpu)) =
        (col(&["ProcessID"]), col(&["TimeInQPC", "CPUStartQPC"]), col(&["MsGPUBusy", "GPUBusy"]))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in lines {
        let f = split_csv(line);
        let (Some(p), Some(q), Some(g)) = (f.get(pid), f.get(t), f.get(gpu)) else {
            continue;
        };
        if let (Ok(p), Ok(q), Some(g)) = (p.trim().parse::<u32>(), q.trim().parse::<i64>(), number(g)) {
            out.push((p, q, g));
        }
    }
    out
}

/// Splits one CSV line, honouring double quotes.
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricStats {
    pub name: String,
    pub ms: Dist,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppSummary {
    pub application: String,
    pub frames: usize,
    pub dropped: usize,
    pub seconds: f64,
    pub avg_fps: f64,
    /// Frames per second at the 1st percentile frame time (the slowest 1% of frames).
    pub low_1_fps: f64,
    pub low_01_fps: f64,
    pub metrics: Vec<MetricStats>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub apps: Vec<AppSummary>,
}

fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("na") {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Summarises a PresentMon CSV per application.
pub fn summarize(csv: &str) -> Result<Summary, String> {
    let mut lines = csv.lines().filter(|l| !l.trim().is_empty());
    let header = split_csv(lines.next().ok_or("the PresentMon CSV is empty")?.trim_start_matches('\u{feff}'));
    let col = |names: &[&str]| header.iter().position(|h| names.iter().any(|n| h.eq_ignore_ascii_case(n)));
    let app_col = col(&["Application"]).ok_or("not a PresentMon CSV: no Application column")?;
    let frame_col = col(&["FrameTime", "msBetweenPresents"]).ok_or("not a PresentMon CSV: no frame time column")?;
    // Present time: seconds (1.x and 2.x with `--v1_metrics`) or milliseconds (2.x `TimeInMs`). A
    // QPC column (`--qpc_time`) has no unit here, so the duration then comes from the frame times.
    let (time_col, time_scale) = match col(&["TimeInSeconds"]) {
        Some(c) => (Some(c), 1.0),
        None => (col(&["TimeInMs"]), 1e-3),
    };
    let dropped_col = col(&["Dropped"]);
    let metric_cols: Vec<(&str, usize)> = METRICS.iter().filter_map(|(label, names)| col(names).map(|c| (*label, c))).collect();

    struct Acc {
        frames: Vec<f64>,
        dropped: usize,
        first_t: f64,
        last_t: f64,
        metrics: Vec<Vec<f64>>,
    }
    let mut apps: BTreeMap<String, Acc> = BTreeMap::new();
    for line in lines {
        let f = split_csv(line);
        let Some(app) = f.get(app_col) else { continue };
        let Some(frame) = f.get(frame_col).and_then(|v| number(v)) else {
            continue;
        };
        let acc = apps.entry(app.clone()).or_insert_with(|| Acc {
            frames: vec![],
            dropped: 0,
            first_t: f64::MAX,
            last_t: f64::MIN,
            metrics: vec![vec![]; metric_cols.len()],
        });
        acc.frames.push(frame);
        if dropped_col.and_then(|c| f.get(c)).is_some_and(|v| v.trim() == "1") {
            acc.dropped += 1;
        }
        if let Some(t) = time_col.and_then(|c| f.get(c)).and_then(|v| number(v)).map(|t| t * time_scale) {
            acc.first_t = acc.first_t.min(t);
            acc.last_t = acc.last_t.max(t);
        }
        for (i, (_, c)) in metric_cols.iter().enumerate() {
            if let Some(v) = f.get(*c).and_then(|v| number(v)) {
                acc.metrics[i].push(v);
            }
        }
    }
    let mut out = Vec::new();
    for (application, mut acc) in apps {
        let total_ms: f64 = acc.frames.iter().sum();
        let seconds = if acc.last_t > acc.first_t { acc.last_t - acc.first_t } else { total_ms / 1000.0 };
        let ft = Dist::of(&mut acc.frames.clone());
        let fps = |ms: f64| if ms > 0.0 { 1000.0 / ms } else { 0.0 };
        let metrics = metric_cols
            .iter()
            .zip(acc.metrics.iter_mut())
            .filter(|(_, v)| !v.is_empty())
            .map(|((label, _), v)| MetricStats { name: label.to_string(), ms: Dist::of(v) })
            .collect();
        out.push(AppSummary {
            application,
            frames: ft.count,
            dropped: acc.dropped,
            seconds,
            avg_fps: if seconds > 0.0 { ft.count as f64 / seconds } else { 0.0 },
            low_1_fps: fps(ft.p99),
            low_01_fps: fps(ft.p999),
            metrics,
        });
    }
    out.sort_by(|a, b| b.frames.cmp(&a.frames).then_with(|| a.application.cmp(&b.application)));
    Ok(Summary { apps: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = "Application,ProcessID,SwapChainAddress,Runtime,SyncInterval,PresentFlags,AllowsTearing,PresentMode,Dropped,TimeInSeconds,msInPresentAPI,msBetweenPresents,msBetweenDisplayChange,msUntilRenderComplete,msUntilDisplayed\n\
game.exe,10,0x1,DXGI,0,0,1,Hardware: Independent Flip,0,1.000,0.5,10.0,10.0,1.0,2.0\n\
game.exe,10,0x1,DXGI,0,0,1,Hardware: Independent Flip,0,1.010,0.5,10.0,10.0,1.0,2.0\n\
game.exe,10,0x1,DXGI,0,0,1,Hardware: Independent Flip,1,1.030,0.5,20.0,NA,1.0,NA\n\
game.exe,10,0x1,DXGI,0,0,1,Hardware: Independent Flip,0,1.040,0.5,10.0,10.0,1.0,2.0\n\
dwm.exe,5,0x2,Other,0,0,0,Composed,0,1.000,0.1,16.0,16.0,1.0,1.0\n";

    #[test]
    fn syntax_follows_the_major_version() {
        assert_eq!(Syntax::from_help("PresentMon 1.10.0\n\nCapture Target Options:"), Syntax::Single);
        assert_eq!(Syntax::from_help("PresentMon 2.3.0\n"), Syntax::Double);
        assert_eq!(Syntax::from_help("PresentMon 10.0.1"), Syntax::Double);
        assert_eq!(Syntax::from_help("garbage"), Syntax::Single);
    }

    #[test]
    fn arguments_for_both_syntaxes() {
        let c = Capture { process: Some("game.exe".into()), delay_s: 3, timed_s: 10, session: "s1".into() };
        assert_eq!(
            args(&c, Path::new("o.csv"), Syntax::Single),
            [
                "-process_name",
                "game.exe",
                "-output_file",
                "o.csv",
                "-delay",
                "3",
                "-timed",
                "10",
                "-terminate_after_timed",
                "-session_name",
                "s1",
                "-stop_existing_session",
                "-no_top"
            ]
        );
        let d = args(&Capture::default(), Path::new("o.csv"), Syntax::Double);
        assert_eq!(d, ["--output_file", "o.csv", "--session_name", "benchlab", "--stop_existing_session"]);
    }

    #[test]
    fn csv_splitting_honours_quotes() {
        assert_eq!(split_csv(r#"a,"b,c",d"#), ["a", "b,c", "d"]);
        assert_eq!(split_csv(r#""say ""hi""",x"#), [r#"say "hi""#, "x"]);
        assert_eq!(split_csv(""), [""]);
        assert_eq!(split_csv("a,,b"), ["a", "", "b"]);
    }

    #[test]
    fn version_one_csv_is_summarised_per_application() {
        let s = summarize(V1).unwrap();
        assert_eq!(s.apps.len(), 2);
        let g = &s.apps[0];
        assert_eq!((g.application.as_str(), g.frames, g.dropped), ("game.exe", 4, 1));
        assert!((g.seconds - 0.04).abs() < 1e-9);
        assert!((g.avg_fps - 100.0).abs() < 1e-6);
        let ft = &g.metrics.iter().find(|m| m.name == "Frame time").unwrap().ms;
        assert_eq!((ft.min, ft.max, ft.mean), (10.0, 20.0, 12.5));
        // NA values are skipped, not counted as zero.
        let shown = &g.metrics.iter().find(|m| m.name == "Displayed time").unwrap().ms;
        assert_eq!(shown.count, 3);
        assert!(g.low_1_fps < g.avg_fps);
    }

    #[test]
    fn version_two_column_names_are_recognised() {
        let csv = "Application,FrameTime,CPUBusy,CPUWait,GPUBusy,DisplayedTime,ClickToPhotonLatency\nx.exe,8.0,3.0,5.0,7.0,8.0,NA\nx.exe,10.0,4.0,6.0,9.0,10.0,25.5\n";
        let s = summarize(csv).unwrap();
        let a = &s.apps[0];
        assert_eq!(a.frames, 2);
        let names: Vec<&str> = a.metrics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Frame time", "CPU busy", "CPU wait", "GPU busy", "Displayed time", "Click to photon"]);
        assert_eq!(a.metrics.last().unwrap().ms.count, 1);
        // No TimeInSeconds column: the duration is the sum of the frame times.
        assert!((a.seconds - 0.018).abs() < 1e-9);
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(summarize("").is_err());
        assert!(summarize("a,b\n1,2\n").is_err());
        assert!(summarize("Application,FrameTime\n").unwrap().apps.is_empty());
        assert!(summarize("Application,FrameTime\nx.exe,notanumber\nx.exe\n").unwrap().apps.is_empty());
    }

    #[test]
    fn version_two_six_default_columns_and_millisecond_times() {
        // PresentMon 2.6 without --v2_metrics: Ms-prefixed names and TimeInMs.
        let csv = "Application,ProcessID,TimeInMs,MsBetweenPresents,MsCPUBusy,MsCPUWait,MsGPUBusy,MsGPUWait,MsGPUTime,MsAllInputToPhotonLatency\n\
x.exe,4,100.0,10.0,6.0,4.0,8.0,2.0,8.5,NA\n\
x.exe,4,110.0,10.0,7.0,3.0,9.0,1.0,9.5,30.0\n\
x.exe,4,130.0,20.0,8.0,12.0,9.0,11.0,9.0,NA\n";
        let s = summarize(csv).unwrap();
        let a = &s.apps[0];
        let names: Vec<&str> = a.metrics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Frame time", "CPU busy", "CPU wait", "GPU busy", "GPU wait", "GPU time", "Input to photon"]);
        // 30 ms between the first and last present.
        assert!((a.seconds - 0.03).abs() < 1e-9);
    }

    #[test]
    fn qpc_frames_and_etl_arguments() {
        let csv = "Application,ProcessID,TimeInQPC,MsBetweenPresents,MsGPUBusy\nx.exe,4,123456789,10.0,8.5\nx.exe,4,123556789,10.0,NA\ny.exe,nope,1,1,1\n";
        assert_eq!(qpc_frames(csv), vec![(4, 123456789, 8.5)]);
        assert!(qpc_frames("Application,FrameTime\nx,1\n").is_empty());
        let a = etl_args(Path::new("t.etl"), Path::new("o.csv"), Syntax::Double);
        assert_eq!(a, ["--etl_file", "t.etl", "--output_file", "o.csv", "--qpc_time", "--no_console_stats"]);
        assert_eq!(etl_args(Path::new("t.etl"), Path::new("o.csv"), Syntax::Single)[0], "-etl_file");
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_first_column() {
        let s = summarize("\u{feff}Application,FrameTime\nx.exe,4.0\n").unwrap();
        assert_eq!(s.apps[0].frames, 1);
    }
}
