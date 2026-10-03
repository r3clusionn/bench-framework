//! Records a `--preset game` trace while `examples/frames.rs` renders frames with stalls it chooses
//! itself, then checks the analysis against that ground truth and against xperf:
//!
//! * every frame the program presented is in the trace, and every stall is a stutter;
//! * each stall is explained at the right time, busy stalls as CPU bound and sleeping ones as waits;
//! * CPU time per process (from context switches) equals `xperf -a cswitch -process`.
//!
//! Skipped (and says so) when xperf is missing or the test is not elevated.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use benchlab::trace::model::AnalysisOptions;
use benchlab::trace::{self, analysis, full, xperf};

fn example(name: &str) -> PathBuf {
    // Test binaries live in target/<profile>/deps; examples in target/<profile>/examples.
    let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    deps.parent().unwrap().join("examples").join(format!("{name}.exe"))
}

#[test]
fn game_capture_finds_every_stall_and_agrees_with_xperf() {
    let Ok(tools) = xperf::find_tools() else {
        eprintln!("SKIPPED: xperf not found");
        return;
    };
    if !xperf::is_elevated() {
        eprintln!("SKIPPED: not elevated");
        return;
    }
    let frames_exe = example("frames");
    assert!(frames_exe.is_file(), "build the example first: {}", frames_exe.display());

    let dir = tempfile::tempdir().unwrap();
    let opts = trace::RecordOptions {
        preset: "game".into(),
        flags: None,
        stackwalk: None,
        no_stacks: false,
        delay_s: 0,
        timed_s: Some(6),
        out_dir: dir.path().to_path_buf(),
        session: Some("game".into()),
        presentmon: None,
        process: None,
        force: true,
        symbols: false,
        analysis: AnalysisOptions::default(),
        top: 10,
        graphics: None,
        focus: Some("frames.exe".into()),
    };
    // Start the program a second into the trace and let it finish a second before the end.
    let app = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        let out = Command::new(frames_exe)
            .args(["--seconds", "3.5", "--stall-every", "60", "--stall-ms", "30", "--mode", "alternate"])
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    });
    let rec = trace::record(&opts).expect("record");
    let log = app.join().unwrap();
    assert!(rec.html.as_ref().is_some_and(|h| h.is_file()), "HTML report written");

    let presented: usize = log.lines().find_map(|l| l.strip_prefix("frames ")).unwrap().trim().parse().unwrap();
    let stalls: Vec<(String, i64)> = log
        .lines()
        .filter_map(|l| l.strip_prefix("stall "))
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f[0].to_string(), f[2].parse().unwrap())
        })
        .collect();
    assert!(stalls.len() >= 6, "{log}");

    let o = full::AnalyzeOptions {
        analysis: analysis::Options {
            focus: Some(analysis::Focus::parse("frames.exe")),
            explain: stalls.len() + 5,
            ..Default::default()
        },
        presentmon: full::PresentMonChoice::Off,
        ..Default::default()
    };
    let (tr, a) = full::analyze_file(&rec.etl, &o).unwrap();
    let r = &a.report;
    let f = &r.frames[0];
    assert_eq!(f.process, "frames.exe");
    // One frame per pair of consecutive presents.
    assert_eq!(f.frames + 1, presented, "every Present is in the trace");
    // Every stall is a stutter. The machine can add a hiccup of its own, so more stutters are
    // allowed, but the slowest frames must be exactly the stalls (30 ms and more).
    let stall_ms: Vec<f64> = stalls.iter().map(|(_, q)| tr.sys.ticks_to_ns(*q).unwrap() as f64 / 1e6).collect();
    let extra: Vec<String> = tr
        .sys
        .presents
        .iter()
        .filter(|p| p.pid == f.pid && p.swap_chain == f.swap_chain)
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| (w[1].t as f64 / 1e6, (w[1].t - w[0].t) as f64 / 1e6))
        .filter(|(at, ft)| *ft > f.stutter_threshold_ms && !stall_ms.iter().any(|s| (s - at).abs() < 1.0))
        .map(|(at, ft)| format!("{ft:.2} ms at {at:.1} ms"))
        .collect();
    eprintln!("stutters that are not stalls: {extra:?} (threshold {:.2} ms)", f.stutter_threshold_ms);
    assert_eq!(f.stutters, stalls.len() + extra.len());
    assert!(extra.iter().all(|e| e.split(' ').next().unwrap().parse::<f64>().unwrap() < 25.0), "{extra:?}");

    for (kind, qpc) in &stalls {
        let t_ms = tr.sys.ticks_to_ns(*qpc).unwrap() as f64 / 1e6;
        let x = r.explained.iter().min_by(|a, b| (a.at_ms - t_ms).abs().total_cmp(&(b.at_ms - t_ms).abs())).unwrap();
        // The program reads the clock just before it calls Present.
        assert!((x.at_ms - t_ms).abs() < 1.0, "stall at {t_ms} ms explained at {} ms", x.at_ms);
        assert!(x.frame_ms > 30.0, "{x:?}");
        match kind.as_str() {
            "spin" => assert!(x.verdict.starts_with("CPU bound"), "busy stall at {t_ms}: {}", x.verdict),
            _ => assert!(x.verdict.starts_with("waiting"), "sleeping stall at {t_ms}: {}", x.verdict),
        }
    }

    // CPU time per process from context switches, against xperf's own computation.
    let out = rec.etl.with_extension("cswitch.txt");
    xperf::run(
        &tools.xperf,
        &[
            "-i".into(),
            rec.etl.to_string_lossy().into_owned(),
            "-o".into(),
            out.to_string_lossy().into_owned(),
            "-a".into(),
            "cswitch".into(),
            "-process".into(),
        ],
    )
    .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let mut theirs: BTreeMap<u32, f64> = BTreeMap::new();
    for line in text.lines() {
        // "   29702120,             Idle (   0)"
        let Some((us, rest)) = line.split_once(',') else {
            continue;
        };
        let (Ok(us), Some(pid)) = (
            us.trim().parse::<f64>(),
            rest.rsplit_once('(').and_then(|x| x.1.trim().trim_end_matches(')').trim().parse::<u32>().ok()),
        ) else {
            continue;
        };
        *theirs.entry(pid).or_default() += us;
    }
    let full_window = full::AnalyzeOptions {
        analysis: analysis::Options { top: 10_000, ..Default::default() },
        presentmon: full::PresentMonChoice::Off,
        ..Default::default()
    };
    let all = full::analyze_trace(&tr, &rec.etl, &full_window).unwrap();
    let ours: BTreeMap<u32, f64> = all.report.processes.iter().map(|p| (p.pid, p.cpu_ms * 1000.0)).collect();
    assert!(theirs.len() > 20, "xperf output: {text}");
    for (pid, us) in &theirs {
        let mine = ours.get(pid).copied().unwrap_or(0.0);
        // xperf prints whole microseconds.
        assert!((mine - us).abs() <= 1.0, "pid {pid}: xperf {us} us, benchlab {mine} us");
    }
}
