use std::path::PathBuf;
use std::process::ExitCode;

use benchlab::report::{fmt_env, render_table, Environment};
use benchlab::trace::model::AnalysisOptions;
use benchlab::{compare, micro, trace, Config, OutlierPolicy, Suite};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "benchlab",
    version,
    about = "Benchmark harness with CPU, memory and cache microbenchmarks and regression detection"
)]
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
    /// Record and analyse Windows kernel traces (xperf, WPA, PresentMon)
    Trace {
        #[command(subcommand)]
        command: TraceCmd,
    },
}

#[derive(Subcommand)]
enum TraceCmd {
    /// Record a kernel trace, then print a DPC/ISR report (needs an elevated terminal)
    Record {
        /// What to trace: dpc, latency, cpu, disk, power or full (see `trace presets`)
        #[arg(long, default_value = "dpc")]
        preset: String,
        /// Kernel flags instead of a preset, joined with +, e.g. PROC_THREAD+LOADER+DPC+INTERRUPT
        #[arg(long)]
        flags: Option<String>,
        /// Stack walk events, joined with +, e.g. Profile+CSwitch (default: the preset's)
        #[arg(long)]
        stackwalk: Option<String>,
        /// Do not collect stacks even if the preset asks for them
        #[arg(long)]
        no_stacks: bool,
        /// Wait this many seconds before the trace starts
        #[arg(long, default_value_t = 0)]
        delay: u64,
        /// Stop automatically after this many seconds (default: stop on Enter or Ctrl+C)
        #[arg(long)]
        timed: Option<u64>,
        /// Folder that holds one sub-folder per trace
        #[arg(long, default_value = "traces")]
        out_dir: PathBuf,
        /// Name of the sub-folder (default: the UTC date and time)
        #[arg(long)]
        session: Option<String>,
        /// Capture frame data with PresentMon at the same time (optionally the path to PresentMon.exe)
        #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
        presentmon: Option<PathBuf>,
        /// Only record this process in PresentMon, e.g. game.exe
        #[arg(long)]
        process: Option<String>,
        /// Open the report in the default text editor when done
        #[arg(long)]
        open_report: bool,
        /// Open the trace in Windows Performance Analyzer when done
        #[arg(long)]
        open_wpa: bool,
        /// Stop an already running NT Kernel Logger session first
        #[arg(long)]
        force: bool,
        /// Rows in the per-driver tables
        #[arg(long, default_value_t = 15)]
        top: usize,
        /// Group by function (driver+offset) instead of by driver
        #[arg(long)]
        functions: bool,
        /// Name functions using symbols (downloads PDBs from the symbol server on first use)
        #[arg(long)]
        symbols: bool,
    },
    /// Print the DPC/ISR report of an existing .etl file (no elevation or toolkit needed)
    Report {
        etl: PathBuf,
        /// Rows in the per-driver tables
        #[arg(long, default_value_t = 15)]
        top: usize,
        /// Group by function (driver+offset) instead of by driver
        #[arg(long)]
        functions: bool,
        /// Name functions using symbols (downloads PDBs from the symbol server on first use)
        #[arg(long)]
        symbols: bool,
        /// Only count events on this logical CPU
        #[arg(long)]
        cpu: Option<u16>,
        /// How many of the longest calls to list
        #[arg(long, default_value_t = 10)]
        worst: usize,
        /// Add the summary of a PresentMon CSV
        #[arg(long, value_name = "CSV")]
        presentmon_csv: Option<PathBuf>,
        /// Also write the full analysis as JSON
        #[arg(long, value_name = "FILE")]
        json: Option<PathBuf>,
        /// Write the report to a file instead of the terminal
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Summarise a PresentMon CSV
    Frames { csv: PathBuf },
    /// Merge two or more traces into one
    Merge {
        /// Traces to merge
        #[arg(required = true, num_args = 2..)]
        traces: Vec<PathBuf>,
        /// The merged trace to write
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Open a trace in Windows Performance Analyzer
    Open {
        etl: PathBuf,
        /// A .wpaProfile to apply
        #[arg(long)]
        profile: Option<PathBuf>,
        /// Set the symbol path to a local cache plus Microsoft's symbol server (unless _NT_SYMBOL_PATH is set)
        #[arg(long)]
        symbols: bool,
    },
    /// Stop a kernel trace that is still running (optionally merging it into FILE)
    Stop {
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Show which tools were found and whether this terminal can record
    Check,
    /// List the trace presets and the kernel flags they use
    Presets,
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
        Cmd::Run {
            filter,
            samples,
            sample_time,
            warmup,
            max_time,
            pin,
            outliers,
            json,
            quiet,
        } => {
            let cfg = Config {
                warmup_ms: warmup,
                sample_time_ms: sample_time,
                samples,
                max_time_ms: max_time,
                outliers: outliers.into(),
            };
            let mut suite = Suite::new(cfg);
            if let Some(cpu) = pin {
                suite = suite
                    .pin(cpu)
                    .map_err(|e| format!("cannot pin to CPU {cpu}: {e}"))?;
            }
            let selected: Vec<_> = micro::all()
                .into_iter()
                .filter(|m| filter.is_empty() || filter.iter().any(|f| m.name.contains(f.as_str())))
                .collect();
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
                report
                    .save(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                eprintln!("wrote {}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Compare {
            base,
            new,
            threshold,
            alpha,
        } => {
            let (b, n) = (
                benchlab::Report::load(&base)?,
                benchlab::Report::load(&new)?,
            );
            let c = compare::compare(&b, &n, threshold / 100.0, alpha);
            print!("{}", compare::render(&c, threshold / 100.0, alpha));
            Ok(if c.regressions() > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Cmd::Trace { command } => run_trace(command),
    }
}

fn open_default(path: &std::path::Path) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
}

fn run_trace(cmd: TraceCmd) -> Result<ExitCode, String> {
    match cmd {
        TraceCmd::Record {
            preset,
            flags,
            stackwalk,
            no_stacks,
            delay,
            timed,
            out_dir,
            session,
            presentmon,
            process,
            open_report,
            open_wpa,
            force,
            top,
            functions,
            symbols,
        } => {
            let opts = trace::RecordOptions {
                preset,
                flags,
                stackwalk,
                no_stacks,
                delay_s: delay,
                timed_s: timed,
                out_dir,
                session,
                presentmon,
                process,
                force,
                symbols,
                analysis: AnalysisOptions {
                    by_function: functions,
                    ..AnalysisOptions::default()
                },
                top,
            };
            let r = trace::record(&opts)?;
            println!("{}", r.text);
            println!("Saved in {}", r.dir.display());
            println!("  trace   {}", r.etl.display());
            if let Some(p) = &r.report {
                println!("  report  {}", p.display());
            }
            if let Some(p) = &r.presentmon_csv {
                println!("  frames  {}", p.display());
            }
            if open_report {
                if let Some(p) = &r.report {
                    open_default(p);
                }
            }
            if open_wpa {
                trace::open_wpa(&r.etl, None, true)?;
            }
        }
        TraceCmd::Report {
            etl,
            top,
            functions,
            symbols,
            cpu,
            worst,
            presentmon_csv,
            json,
            out,
        } => {
            let a = trace::report_file(
                &etl,
                &AnalysisOptions {
                    by_function: functions,
                    cpu,
                    worst,
                },
                symbols,
            )?;
            let pm = match presentmon_csv {
                Some(p) => Some(trace::presentmon::summarize(
                    &std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?,
                )?),
                None => None,
            };
            let text = trace::render::render(&a, &etl.to_string_lossy(), top, pm.as_ref());
            match out {
                Some(p) => {
                    std::fs::write(&p, &text).map_err(|e| format!("{}: {e}", p.display()))?;
                    eprintln!("wrote {}", p.display());
                }
                None => print!("{text}"),
            }
            if let Some(p) = json {
                std::fs::write(
                    &p,
                    serde_json::to_string_pretty(&a).map_err(|e| e.to_string())?,
                )
                .map_err(|e| format!("{}: {e}", p.display()))?;
                eprintln!("wrote {}", p.display());
            }
        }
        TraceCmd::Frames { csv } => {
            let s = trace::presentmon::summarize(
                &std::fs::read_to_string(&csv).map_err(|e| format!("{}: {e}", csv.display()))?,
            )?;
            print!("{}", trace::render::render_presentmon(&s));
        }
        TraceCmd::Merge { traces, out } => {
            let t = trace::merge(&traces, &out)?;
            if !t.is_empty() {
                println!("{t}");
            }
            println!("wrote {}", out.display());
        }
        TraceCmd::Open {
            etl,
            profile,
            symbols,
        } => trace::open_wpa(&etl, profile.as_deref(), symbols)?,
        TraceCmd::Stop { out } => {
            let t = trace::stop(out.as_deref())?;
            println!("{}", if t.is_empty() { "stopped" } else { t.as_str() });
        }
        TraceCmd::Check => print!("{}", trace::check()),
        TraceCmd::Presets => print!("{}", trace::presets_table()),
    }
    Ok(ExitCode::SUCCESS)
}
