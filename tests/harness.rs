use std::process::Command;
use std::time::{Duration, Instant};

use benchlab::compare::{compare, Verdict};
use benchlab::stats::{summarize, OutlierPolicy};
use benchlab::{BenchResult, Config, Report, Suite, Throughput};

fn spin(d: Duration) {
    let t = Instant::now();
    while t.elapsed() < d {
        std::hint::spin_loop();
    }
}

fn quick() -> Config {
    Config { warmup_ms: 30, sample_time_ms: 5, samples: 25, max_time_ms: 5000, outliers: OutlierPolicy::Keep }
}

#[test]
fn calibration_sizes_a_sample_to_the_target_time() {
    let mut suite = Suite::new(quick());
    let r = suite.bench("spin100us", || spin(Duration::from_micros(100))).clone();
    // 5 ms target / 100 us per call is about 50 calls per sample.
    assert!((35..=60).contains(&r.iters_per_sample), "iters_per_sample = {}", r.iters_per_sample);
    let median_us = r.summary.median / 1e3;
    assert!((98.0..130.0).contains(&median_us), "median = {median_us} us");
    assert_eq!(r.samples_ns.len(), 25);
}

#[test]
fn a_slow_benchmark_stops_at_the_time_cap_but_keeps_ten_samples() {
    let cfg = Config { warmup_ms: 0, sample_time_ms: 1, samples: 1000, max_time_ms: 250, outliers: OutlierPolicy::Keep };
    let mut suite = Suite::new(cfg);
    let started = Instant::now();
    let r = suite.bench("slow", || spin(Duration::from_millis(15))).clone();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!((10..200).contains(&r.samples_ns.len()), "{} samples", r.samples_ns.len());
}

#[test]
fn throughput_rates_are_derived_from_the_median() {
    let mut suite = Suite::new(quick());
    let r = suite.bench_throughput("bytes", Throughput::Bytes(1000), || spin(Duration::from_micros(50))).clone();
    // 1000 bytes in about 50 us is about 0.02 GB/s.
    let rate = r.rate().unwrap();
    assert!((0.012..0.021).contains(&rate), "{rate}");
    let e = suite.bench_throughput("elems", Throughput::Elements(10), || spin(Duration::from_micros(50))).clone();
    let per_op = e.ns_per_op().unwrap();
    assert!((4_900.0..6_500.0).contains(&per_op), "{per_op}");
}

#[test]
fn report_survives_a_json_round_trip() {
    let mut suite = Suite::new(quick());
    suite.bench("a", || spin(Duration::from_micros(20)));
    suite.bench_throughput("b", Throughput::Bytes(64), || spin(Duration::from_micros(20)));
    let report = suite.into_report();
    let back = Report::from_json(&report.to_json()).unwrap();
    assert_eq!(back, report);
    assert!(Report::from_json("{\"schema\": 99}").is_err());
    assert!(Report::from_json("not json").is_err());
}

fn result(name: &str, samples: Vec<f64>) -> BenchResult {
    let summary = summarize(&samples, OutlierPolicy::Keep);
    BenchResult { name: name.to_string(), iters_per_sample: 1, samples_ns: samples, throughput: None, summary }
}

fn report(results: Vec<BenchResult>) -> Report {
    let mut suite = Suite::new(quick());
    suite.bench("placeholder", || 1);
    let mut r = suite.into_report();
    r.results = results;
    r
}

fn around(center: f64, n: usize, seed: u64) -> Vec<f64> {
    // A deterministic spread of about +-2 percent.
    let mut s = seed;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            center * (1.0 + ((s % 4001) as f64 / 4000.0 - 0.5) * 0.04)
        })
        .collect()
}

#[test]
fn verdicts_follow_the_threshold_and_the_significance_test() {
    let base = report(vec![
        result("steady", around(1000.0, 40, 1)),
        result("slower", around(1000.0, 40, 2)),
        result("faster", around(1000.0, 40, 3)),
        result("small-shift", around(1000.0, 40, 4)),
        result("gone", around(5.0, 10, 7)),
    ]);
    let new = report(vec![
        result("steady", around(1000.0, 40, 11)),
        result("slower", around(1200.0, 40, 12)),
        result("faster", around(800.0, 40, 13)),
        result("small-shift", around(1030.0, 40, 14)),
        result("added", around(5.0, 10, 17)),
    ]);
    let c = compare(&base, &new, 0.05, 0.01);
    let verdict = |n: &str| c.rows.iter().find(|r| r.name == n).unwrap().verdict;
    assert_eq!(verdict("steady"), Verdict::Unchanged);
    assert_eq!(verdict("slower"), Verdict::Regressed);
    assert_eq!(verdict("faster"), Verdict::Improved);
    // 3 percent is under the 5 percent threshold, however significant.
    assert_eq!(verdict("small-shift"), Verdict::Unchanged);
    assert_eq!(c.only_in_base, ["gone"]);
    assert_eq!(c.only_in_new, ["added"]);
    assert_eq!(c.regressions(), 1);
    let slower = c.rows.iter().find(|r| r.name == "slower").unwrap();
    assert!((slower.change - 0.2).abs() < 0.02, "{}", slower.change);
}

#[test]
fn a_shift_the_samples_cannot_support_is_inconclusive() {
    // Two samples of 6 points each whose medians differ by 10 percent but whose values interleave.
    let base = report(vec![result("x", vec![100.0, 90.0, 140.0, 95.0, 150.0, 105.0])]);
    let new = report(vec![result("x", vec![110.0, 92.0, 145.0, 99.0, 155.0, 118.0])]);
    let c = compare(&base, &new, 0.05, 0.01);
    assert!(c.rows[0].change > 0.05);
    assert_eq!(c.rows[0].verdict, Verdict::Inconclusive, "{:?}", c.rows[0]);
    assert_eq!(c.regressions(), 0);
}

#[test]
fn a_real_slowdown_is_detected_and_identical_code_is_not_flagged() {
    let run = |us: u64| {
        let mut s = Suite::new(quick());
        s.bench("work", || spin(Duration::from_micros(us)));
        s.into_report()
    };
    let (a, a2, b) = (run(100), run(100), run(150));
    assert_eq!(compare(&a, &a2, 0.10, 0.01).regressions(), 0);
    let c = compare(&a, &b, 0.10, 0.01);
    assert_eq!(c.rows[0].verdict, Verdict::Regressed, "{:?}", c.rows[0]);
    let c = compare(&b, &a, 0.10, 0.01);
    assert_eq!(c.rows[0].verdict, Verdict::Improved);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn pinning_moves_the_thread_to_the_requested_cpu() {
    let n = std::thread::available_parallelism().unwrap().get();
    assert!(n >= 2, "needs two CPUs");
    for cpu in [0, n - 1, 1] {
        let ok = std::thread::spawn(move || {
            benchlab::affinity::pin_current_thread(cpu).unwrap();
            // Give the scheduler a chance to move us if pinning did not work.
            for _ in 0..50 {
                spin(Duration::from_micros(200));
                if benchlab::affinity::current_cpu() != Some(cpu) {
                    return false;
                }
                std::thread::yield_now();
            }
            true
        })
        .join()
        .unwrap();
        assert!(ok, "thread did not stay on CPU {cpu}");
    }
    assert!(std::thread::spawn(|| benchlab::affinity::pin_current_thread(5000)).join().unwrap().is_err());
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_benchlab"))
}

#[test]
fn cli_run_compare_and_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.json");
    let out = bin()
        .args(["run", "cpu/int-latency", "cpu/fp-latency", "--samples", "20", "--warmup", "30", "--sample-time", "3", "--pin", "1", "-q", "--json"])
        .arg(&base)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("cpu/int-latency") && text.contains("cpu/fp-latency") && !text.contains("cache/"), "{text}");
    let rep = Report::load(&base).unwrap();
    assert_eq!(rep.results.len(), 2);
    assert_eq!(rep.environment.pinned_cpu, Some(1));

    // The same report against itself: nothing changed, exit 0.
    let same = bin().arg("compare").arg(&base).arg(&base).output().unwrap();
    assert_eq!(same.status.code(), Some(0), "{}", String::from_utf8_lossy(&same.stdout));
    assert!(String::from_utf8_lossy(&same.stdout).contains("0 regressed"));

    // A copy where every sample is 40 percent slower: regression, exit 1.
    let mut slow = rep.clone();
    for r in &mut slow.results {
        r.samples_ns.iter_mut().for_each(|x| *x *= 1.4);
        r.summary = summarize(&r.samples_ns, OutlierPolicy::Keep);
    }
    let slow_path = dir.path().join("slow.json");
    slow.save(&slow_path).unwrap();
    let c = bin().arg("compare").arg(&base).arg(&slow_path).output().unwrap();
    assert_eq!(c.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&c.stdout);
    assert!(stdout.contains("REGRESSED") && stdout.contains("2 regressed"), "{stdout}");
    // Reversed, they are improvements and the exit status is 0.
    assert_eq!(bin().arg("compare").arg(&slow_path).arg(&base).status().unwrap().code(), Some(0));

    // Errors exit with 2.
    std::fs::write(dir.path().join("bad.json"), "{}").unwrap();
    assert_eq!(bin().arg("compare").arg(&base).arg(dir.path().join("bad.json")).status().unwrap().code(), Some(2));
    assert_eq!(bin().args(["run", "no-such-benchmark", "-q"]).status().unwrap().code(), Some(2));
}

#[test]
fn cli_list_shows_every_builtin() {
    let out = bin().arg("list").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for n in benchlab::micro::all() {
        assert!(text.contains(&n.name), "{} missing from list", n.name);
    }
}
