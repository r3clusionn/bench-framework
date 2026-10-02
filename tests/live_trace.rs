//! Records a real kernel trace and checks the ETL reader against `xperf -a dumper`, which is
//! produced by Microsoft's own decoder. Skipped (and says so) when xperf is missing or the test
//! process is not elevated.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use benchlab::trace::model::{analyze, AnalysisOptions, Kind};
use benchlab::trace::{self, etl, xperf};

struct Dumped {
    /// (cpu, elapsed microseconds, module) per DPC row.
    dpc: Vec<(u16, u64, String)>,
    isr: Vec<(u16, u64, String)>,
}

fn dump(xperf_exe: &std::path::Path, etl: &std::path::Path) -> Dumped {
    let out = etl.with_extension("dump.csv");
    let r = xperf::run(
        xperf_exe,
        &[
            "-i".into(),
            etl.to_string_lossy().into_owned(),
            "-o".into(),
            out.to_string_lossy().into_owned(),
            "-a".into(),
            "dumper".into(),
        ],
    )
    .unwrap();
    assert!(out.is_file(), "dumper failed: {}", r.text);
    let text = String::from_utf8_lossy(&std::fs::read(&out).unwrap()).into_owned();
    let mut d = Dumped { dpc: vec![], isr: vec![] };
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some(&name) = f.first() else { continue };
        // Rows: DPC|DPCTmr, TimeStamp, ElapsedTime, CPU, ServiceAddr, Image!Function
        let (is_dpc, is_isr) = (name == "DPC" || name == "DPCTmr", name == "Interrupt");
        if !(is_dpc || is_isr) || f.len() < 6 || f[1] == "TimeStamp" {
            continue;
        }
        let module = f.last().unwrap().split('!').next().unwrap().to_ascii_lowercase();
        let row = (f[3].parse().unwrap(), f[2].parse().unwrap(), module);
        if is_dpc {
            d.dpc.push(row);
        } else {
            d.isr.push(row);
        }
    }
    d
}

#[test]
fn reader_agrees_with_xperf_on_a_live_trace() {
    let Ok(tools) = xperf::find_tools() else {
        eprintln!("SKIPPED: xperf not found");
        return;
    };
    if !xperf::is_elevated() {
        eprintln!("SKIPPED: not elevated");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let opts = trace::RecordOptions {
        preset: "dpc".into(),
        flags: None,
        stackwalk: None,
        no_stacks: true,
        delay_s: 0,
        timed_s: Some(3),
        out_dir: dir.path().to_path_buf(),
        session: Some("live".into()),
        presentmon: None,
        process: None,
        force: true,
        symbols: false,
        analysis: AnalysisOptions::default(),
        top: 5,
    };
    let rec = trace::record(&opts).expect("record");
    assert!(rec.etl.is_file() && rec.report.as_ref().unwrap().is_file());
    assert!(rec.text.contains("ISR/DPC usage by CPU"));

    let ours = etl::read(&rec.etl).unwrap();
    let theirs = dump(&tools.xperf, &rec.etl);
    let a = analyze(&ours, &AnalysisOptions { worst: 0, ..AnalysisOptions::default() }, None);

    let our_dpc: Vec<_> = ours.events.iter().filter(|e| !e.kind.is_isr()).collect();
    let our_isr: Vec<_> = ours.events.iter().filter(|e| e.kind == Kind::Isr).collect();
    assert!(our_dpc.len() > 100, "too few DPCs to mean anything: {}", our_dpc.len());
    assert_eq!(our_dpc.len(), theirs.dpc.len(), "DPC count");
    assert_eq!(our_isr.len(), theirs.isr.len(), "ISR count");

    // Per-CPU counts, exactly.
    let by_cpu = |v: &mut BTreeMap<u16, usize>, cpu: u16| *v.entry(cpu).or_default() += 1;
    let (mut mine, mut xp) = (BTreeMap::new(), BTreeMap::new());
    our_dpc.iter().for_each(|e| by_cpu(&mut mine, e.cpu));
    theirs.dpc.iter().for_each(|r| by_cpu(&mut xp, r.0));
    assert_eq!(mine, xp, "DPC count per CPU");
    let (mut mine, mut xp) = (BTreeMap::new(), BTreeMap::new());
    our_isr.iter().for_each(|e| by_cpu(&mut mine, e.cpu));
    theirs.isr.iter().for_each(|r| by_cpu(&mut xp, r.0));
    assert_eq!(mine, xp, "ISR count per CPU");

    // Per-driver counts, exactly, for every driver both tools can name.
    let mut mine: BTreeMap<String, usize> = BTreeMap::new();
    let map = benchlab::trace::model::ImageMap::new(&ours.images);
    for e in &our_dpc {
        *mine.entry(map.module(e.routine).to_ascii_lowercase()).or_default() += 1;
    }
    let mut xp: BTreeMap<String, usize> = BTreeMap::new();
    for r in &theirs.dpc {
        *xp.entry(r.2.clone()).or_default() += 1;
    }
    let mut compared = 0;
    for (name, n) in &xp {
        if name.starts_with("unknown") || name.starts_with("0x") || name.is_empty() {
            continue;
        }
        assert_eq!(mine.get(name), Some(n), "DPC count for {name}");
        compared += 1;
    }
    assert!(compared >= 3, "compared only {compared} drivers: {xp:?}");

    // Total elapsed time: xperf prints whole microseconds, so allow one per event.
    let ours_us: f64 = our_dpc.iter().map(|e| e.elapsed_ns as f64 / 1e3).sum();
    let theirs_us: f64 = theirs.dpc.iter().map(|r| r.1 as f64).sum();
    let slack = our_dpc.len() as f64;
    assert!((ours_us - theirs_us).abs() <= slack, "DPC time {ours_us} against {theirs_us}");
    let ours_us: f64 = our_isr.iter().map(|e| e.elapsed_ns as f64 / 1e3).sum();
    let theirs_us: f64 = theirs.isr.iter().map(|r| r.1 as f64).sum();
    assert!((ours_us - theirs_us).abs() <= our_isr.len() as f64, "ISR time {ours_us} against {theirs_us}");
    assert_eq!(a.dpc_events as usize, our_dpc.len());

    // The kernel logger must be free again.
    let loggers = xperf::run(&tools.xperf, &["-loggers".into()]).unwrap();
    assert!(!xperf::kernel_logger_running(&loggers.text), "the NT Kernel Logger was left running");
    let _: PathBuf = rec.dir;
}
