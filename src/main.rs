use std::path::PathBuf;
use std::process::ExitCode;

use benchlab::report::{fmt_env, render_table, Environment};
use benchlab::{compare, micro, Config, OutlierPolicy, Suite};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "benchlab", version, about = "Benchmark harness with CPU, memory and cache microbenchmarks and regression detection")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the built-in microbenchmarks
    List,
    /// Print the machine description and timer overhead that go into a report
    Info,
    /// Run the built-in microbenchmarks
    Run {
        /// Run only benchmarks whose name contains one of these strings (default: all)
        filter: Vec<String>,
        /// Samples per benchmark
        #[arg(long, default_value_t = 50)]
        samples: usize,
        /// Target duration of one sample, in milliseconds
        #[arg(long, default_value_t = 10)]
        sample_time: u64,
        /// Warm-up per benchmark, in milliseconds
        #[arg(long, default_value_t = 200)]
        warmup: u64,
        /// Stop sampling a slow benchmark after this many milliseconds (at least 10 samples are kept)
        #[arg(long, default_value_t = 5000)]
        max_time: u64,
        /// Pin the measuring thread to this logical CPU
        #[arg(long, value_name = "CPU")]
        pin: Option<usize>,
        /// What to do with outliers (outside Tukey's fences) before summarising
        #[arg(long, value_enum, default_value_t = Policy::Keep)]
        outliers: Policy,
        /// Write the full report, with every raw sample, to this file
        #[arg(long, value_name = "FILE")]
        json: Option<PathBuf>,
        /// Do not print progress to stderr
        #[arg(short, long)]
        quiet: bool,
    },
    /// Compare two JSON reports; exits with status 1 if any benchmark regressed
    Compare {
        base: PathBuf,
        new: PathBuf,
        /// Smallest change in the median that counts, in percent
        #[arg(long, default_value_t = 5.0)]
        threshold: f64,
        /// Significance level of the Mann-Whitney U test
        #[arg(long, default_value_t = 0.01)]
        alpha: f64,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Policy {
    Keep,
    Trim,
    TrimSevere,
}

impl From<Policy> for OutlierPolicy {
    fn from(p: Policy) -> Self {
        match p {
            Policy::Keep => OutlierPolicy::Keep,
            Policy::Trim => OutlierPolicy::Trim,
            Policy::TrimSevere => OutlierPolicy::TrimSevere,
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("benchlab: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Cmd::List => {
            let all = micro::all();
            let w = all.iter().map(|m| m.name.len()).max().unwrap_or(0);
            for m in &all {
                println!("{:<w$}  {}", m.name, m.description);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Info => {
            println!("{}", fmt_env(&Environment::capture(None)));
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Run { filter, samples, sample_time, warmup, max_time, pin, outliers, json, quiet } => {
            let cfg = Config { warmup_ms: warmup, sample_time_ms: sample_time, samples, max_time_ms: max_time, outliers: outliers.into() };
            let mut suite = Suite::new(cfg);
            if let Some(cpu) = pin {
                suite = suite.pin(cpu).map_err(|e| format!("cannot pin to CPU {cpu}: {e}"))?;
            }
            let selected: Vec<_> = micro::all().into_iter().filter(|m| filter.is_empty() || filter.iter().any(|f| m.name.contains(f.as_str()))).collect();
            if selected.is_empty() {
                return Err("no benchmark matches the filter (see `benchlab list`)".to_string());
            }
            for (i, m) in selected.iter().enumerate() {
                if !quiet {
                    eprintln!("[{}/{}] {}", i + 1, selected.len(), m.name);
                }
                let mut f = (m.build)();
                suite.bench_throughput(&m.name, m.throughput, &mut f);
            }
            let report = suite.into_report();
            println!("{}\n", fmt_env(&report.environment));
            print!("{}", render_table(&report.results));
            if let Some(path) = json {
                report.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                eprintln!("wrote {}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Compare { base, new, threshold, alpha } => {
            let (b, n) = (benchlab::Report::load(&base)?, benchlab::Report::load(&new)?);
            let c = compare::compare(&b, &n, threshold / 100.0, alpha);
            print!("{}", compare::render(&c, threshold / 100.0, alpha));
            Ok(if c.regressions() > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
        }
    }
}
