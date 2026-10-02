//! benchlab's statistics against NumPy and SciPy (see scripts/make_oracle.py).

use benchlab::stats::{mann_whitney, summarize, OutlierPolicy};
use serde_json::Value;

fn oracle() -> Value {
    serde_json::from_str(include_str!("oracle.json")).unwrap()
}

fn series(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()
}

fn close(a: f64, b: f64, what: &str) {
    let tol = 1e-9 * b.abs().max(1.0);
    assert!((a - b).abs() <= tol, "{what}: got {a}, expected {b}");
}

#[test]
fn summaries_match_numpy() {
    let o = oracle();
    for (name, case) in o["summaries"].as_object().unwrap() {
        let data = series(&case["data"]);
        let s = summarize(&data, OutlierPolicy::Keep);
        close(s.mean, case["mean"].as_f64().unwrap(), &format!("{name} mean"));
        close(s.stddev, case["stddev"].as_f64().unwrap(), &format!("{name} stddev"));
        close(s.mad, case["mad"].as_f64().unwrap(), &format!("{name} mad"));
        let p = &case["percentiles"];
        for (label, got) in [("1", s.p1), ("5", s.p5), ("25", s.p25), ("50", s.median), ("75", s.p75), ("95", s.p95), ("99", s.p99)] {
            close(got, p[label].as_f64().unwrap(), &format!("{name} p{label}"));
        }
        let out = &case["outliers"];
        let want = |k: &str| out[k].as_u64().unwrap() as usize;
        assert_eq!(
            (s.outliers.low_severe, s.outliers.low_mild, s.outliers.high_mild, s.outliers.high_severe),
            (want("low_severe"), want("low_mild"), want("high_mild"), want("high_severe")),
            "{name} outliers"
        );
    }
}

#[test]
fn mann_whitney_matches_scipy() {
    let o = oracle();
    let mut significant = 0;
    for pair in o["mann_whitney"].as_array().unwrap() {
        let x = series(&o["summaries"][pair["x"].as_str().unwrap()]["data"]);
        let y = series(&o["summaries"][pair["y"].as_str().unwrap()]["data"]);
        let m = mann_whitney(&x, &y);
        let label = format!("{} vs {}", pair["x"], pair["y"]);
        close(m.u, pair["u"].as_f64().unwrap(), &format!("{label} U"));
        // p-values span many orders of magnitude, so compare relatively.
        let want = pair["p"].as_f64().unwrap();
        assert!((m.p - want).abs() <= 1e-9 * want.max(1e-300) + 1e-12, "{label} p: got {}, expected {want}", m.p);
        if want < 0.01 {
            significant += 1;
        }
    }
    assert!(significant >= 2, "the fixture should include clearly different samples");
}
