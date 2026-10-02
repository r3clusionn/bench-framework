//! Using benchlab as a library: measure your own functions, print a table, optionally save a
//! report that `benchlab compare` can diff against a later run.
//!
//!     cargo run --release --example custom -- [report.json]

use benchlab::report::{fmt_env, render_table};
use benchlab::{Config, Suite, Throughput};

fn main() {
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let data: Vec<u32> = (0..10_000)
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng as u32
        })
        .collect();
    let n = Throughput::Elements(data.len() as u64);

    let mut suite = Suite::new(Config::default());
    // Every sorting benchmark has to copy the input first, so measure the copy alone as a baseline.
    suite.bench_throughput("clone 10k u32 (baseline)", n, || data.clone());
    suite.bench_throughput("sort 10k u32", n, || {
        let mut v = data.clone();
        v.sort();
        v
    });
    suite.bench_throughput("sort_unstable 10k u32", n, || {
        let mut v = data.clone();
        v.sort_unstable();
        v
    });
    let report = suite.into_report();
    println!("{}\n", fmt_env(&report.environment));
    print!("{}", render_table(&report.results));
    if let Some(path) = std::env::args().nth(1) {
        report.save(std::path::Path::new(&path)).expect("write report");
        println!("\nwrote {path}");
    }
}
