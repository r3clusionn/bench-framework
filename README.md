# benchlab

A Rust benchmarking harness and CLI: it calibrates, warms up, takes repeated samples, reports percentiles and outliers, can pin the measuring thread to one CPU, saves every raw sample as JSON and flags regressions between two runs with a significance test. It ships with CPU, memory and cache microbenchmarks. On Windows it also captures games and analyses `.etl` traces the way WPA does: average and minimum FPS, 1% and 0.1% lows, stutters, and for each slow frame what its render thread was doing (running, waiting for a CPU, waiting on what) next to DPC/ISR time per driver, CPU usage per process, thread and function, ready time, hard faults and disk I/O. For anyone who wants trustworthy timings for their own code, or wants to know why a game stutters without spending an afternoon in WPA.

**Status:** v0.3.0, working on Windows. The Linux code (CPU pinning, CPU name) is compile-checked but was not run. Not published to crates.io.

![benchlab trace analyze: frames per swap chain with average and minimum FPS, 1% and 0.1% lows and stutters, then the slowest frames each explained by what the render thread did](docs/images/trace-analyze.png)

## Features

- Warm-up and calibration: the closure runs for the warm-up period while the harness works out how many calls fit in one sample, so each sample lasts about 10 ms regardless of how fast the code is.
- Statistics per benchmark: median, mean, standard deviation, MAD, p1 to p99, IQR, coefficient of variation.
- Outliers counted with Tukey's fences (mild beyond 1.5 IQR, severe beyond 3 IQR), and optionally trimmed before summarising.
- CPU pinning (`--pin CPU`) so a hybrid CPU cannot move the measurement between a fast and a slow core.
- JSON reports with every raw sample and a description of the machine.
- `compare`: median change plus a Mann-Whitney U test on the raw samples; exits 1 on a significant regression, so it can gate CI.
- Built-in microbenchmarks: CPU latency and throughput, memory read, write and copy bandwidth, and a pointer-chase latency curve across working-set sizes from 4 KiB to 256 MiB.
- Usable as a library for your own benchmarks.
- `benchlab trace record --preset game` (Windows): captures a game in one command (kernel events with stacks plus the graphics providers PresentMon uses) and writes a text, JSON and HTML report. See [Capturing a game](#capturing-a-game).
- `benchlab trace analyze`: a WPA-style analysis of any `.etl` file, in sections you can pick: frames and lows, slow frames explained, CPU usage (precise and sampled, by process, thread, module and function, inclusive from stacks), ready time, wait reasons and where threads waited, preemption, hard faults, disk I/O, DPC/ISR per driver. Symbols from Microsoft's symbol server, a time window, a focus process, flame graph output. See [Analysing a trace](#analysing-a-trace).
- `benchlab trace export`: any event table as CSV (context switches, ready events, samples, hard faults, disk, presents, DPCs and ISRs, processes, threads).
- The xtw-style DPC/ISR report (usage by CPU, elapsed time and interval per driver), PresentMon capture, trace merging and opening in WPA.

## How to install

Requires a recent stable Rust (built and tested with 1.98.1).

```sh
git clone https://github.com/r3clusionn/bench-framework
cd bench-framework
cargo install --path .
```

Or run it in place with `cargo run --release -- run`. Always build with `--release`.

## How to use

```sh
benchlab list                                  # the built-in benchmarks
benchlab info                                  # machine, clock step and cost of reading the clock
benchlab run --pin 2 --json base.json          # run everything, pinned to logical CPU 2
benchlab run cache mem/read --samples 100      # only names that contain "cache" or "mem/read"
benchlab compare base.json new.json            # exit status 1 if anything regressed
```

![benchlab run --pin 2: 24 microbenchmarks with median, p5, spread, p95, p99, outlier count, cost per operation and rate](docs/images/run.png)

To capture a game and find out why it stutters, see [Capturing a game](#capturing-a-game); to analyse a trace you already have, see [Analysing a trace](#analysing-a-trace). Recording needs the Windows Performance Toolkit (`xperf.exe`, part of the Windows ADK) and an elevated terminal; analysing does not.

| Option of `run` | What it does |
|---|---|
| `FILTER...` | Run only benchmarks whose name contains one of the strings. |
| `--samples N` | Samples per benchmark (default 50). |
| `--sample-time MS` | Target duration of one sample (default 10). |
| `--warmup MS` | Warm-up per benchmark (default 200). |
| `--max-time MS` | Stop a slow benchmark after this long, keeping at least 10 samples (default 5000). |
| `--pin CPU` | Pin the measuring thread to this logical CPU. |
| `--outliers keep\|trim\|trim-severe` | Drop outliers before summarising (default `keep`). |
| `--json FILE` | Save the full report. |

| Option of `compare` | What it does |
|---|---|
| `--threshold PCT` | Smallest median change that counts (default 5). |
| `--alpha P` | Significance level (default 0.01). |

A comparison says `REGRESSED` or `improved` only when the median moved by more than the threshold and the samples are significantly different. A large shift with overlapping samples is `inconclusive`. Exit status is 0 when nothing regressed, 1 when something did, 2 on an error such as an unreadable report.

![benchlab compare of a run pinned to CPU 2 against a run pinned to CPU 20, with 20 regressions, 1 improvement and 3 unchanged](docs/images/compare.png)

*The screenshot compares the same code on two different cores of one machine (CPU 2 against CPU 20, an E-core), which is why almost everything regressed. It is a demonstration of the report, not a code change.*

### As a library

```rust
use benchlab::{Config, Suite, Throughput};

let mut suite = Suite::new(Config::default());
suite.bench_throughput("sort_unstable 10k", Throughput::Elements(10_000), || {
    let mut v = data.clone();
    v.sort_unstable();
    v
});
let report = suite.into_report();
report.save(std::path::Path::new("report.json")).unwrap();
```

The closure's return value goes through `black_box`, so the work is not optimised away. `examples/custom.rs` is a complete program that benchmarks two sorts against a clone baseline.

![The custom example: a clone baseline, sort and sort_unstable on 10,000 u32 values with medians and rates](docs/images/library.png)

## Capturing a game

Run this in an elevated terminal while the game is running (or start it during the delay), then play:

```sh
benchlab trace record --preset game --process game.exe --delay 5 --timed 60
```

It records the kernel events of the `game` preset (processes, context switches, ready events, CPU samples with stacks, DPCs, ISRs, hard faults, disk I/O) with xperf, and in a second session the graphics events that frame analysis needs: Present calls (DXGI and Direct3D 9) and the DxgKrnl, Win32k and DWM events PresentMon reads, filtered to exactly the event ids PresentMon 2.6 enables so the trace stays small. Both sessions use the performance counter as their clock and are merged into one `trace.etl`, so a frame, a DPC and a context switch can be compared to the tenth of a microsecond. The folder then holds:

| File | Content |
|---|---|
| `trace.etl` | The merged trace. Opens in WPA, `benchlab trace analyze` and PresentMon (`--etl_file`). |
| `report.txt`, `report.json` | The full analysis (all sections below). |
| `report.html` | The same with time-line charts of frame time, CPU usage, DPC/ISR time and hard faults on one time axis. |
| `presentmon.csv` | PresentMon's per-frame metrics for the trace, when PresentMon was found. |

A 60 second capture is about 750 MB on a 24-thread machine (12 MB/s here). `--no-stacks` makes it smaller and drops the inclusive and wait-site tables.

![The HTML report: average, 1% and 0.1% low and minimum FPS, stutters, then frame time, CPU usage, DPC/ISR time and hard faults on one time axis](docs/images/trace-html.png)

## Analysing a trace

`trace analyze` reads any kernel trace, recorded by benchlab, WPR, xperf or another tool. It needs neither elevation nor the toolkit. Sections are only printed when the trace has the events for them; the overview says what is missing.

```sh
benchlab trace analyze trace.etl                                  # every section
benchlab trace analyze trace.etl -s frames,explain --explain 10   # FPS, lows and the 10 slowest frames
benchlab trace analyze trace.etl --process game.exe --from 12.5 --to 20 --symbols
benchlab trace analyze trace.etl --html report.html --json report.json --folded game.folded
benchlab trace export trace.etl --table cswitch -o cswitch.csv
benchlab trace sections                                           # the section and table names
```

| Section | What it shows |
|---|---|
| `overview` | Length, event counts, the focus process, what the trace could not answer, lost events. |
| `frames` | Per swap chain: frames, average FPS, min FPS (the longest frame), 1% and 0.1% low both as percentile and as the average of the slowest frames, stutters (frames over twice the median), frame time and time inside Present. |
| `presentmon` | PresentMon's metrics for the trace (GPU busy and wait, displayed time, latency), when PresentMon is found or given with `--presentmon`. |
| `explain` | The slowest frames of the focus process. For each: what its render thread did during the frame (running, ready but not running, waiting and on what), the process's CPU time, DPC/ISR time in total and on the CPUs the render thread used with the longest call, hard faults, who preempted it, the functions it ran, GPU busy from PresentMon, and a one-line likely cause. |
| `processes` | CPU usage (precise) per process from context switches, with samples, switch-ins, threads, ready time, hard faults and disk bytes. |
| `cpus` | Busy share and DPC/ISR share of each logical CPU. |
| `threads` | The focus process's threads: CPU time, ready time and its 99th percentile, wait time and the main wait reason. |
| `sampled` | CPU usage (sampled) by process and module, by function, and inclusive by function from the call stacks. |
| `ready` | Ready time (from becoming runnable to running) per process: count, total and percentiles. |
| `waits` | Why the focus process's threads waited (wait reasons), where (the first frame past the system's wait code in the switch-in stack), and which processes preempted them. |
| `faults` | Hard page faults by process and by file, with bytes and time. |
| `disk` | Disk I/O by process and by file, and the service time distribution. |
| `dpc` | The DPC/ISR report described below. |

| Option of `trace analyze` | What it does |
|---|---|
| `--process NAME\|PID` | The focus process. Default: the one that presented the most frames (not the compositor), else the busiest. |
| `--from S`, `--to S` | Analyse only this window, in seconds from the start of the trace. |
| `-s, --section LIST` | Sections to print, comma separated. |
| `--top N`, `--explain N`, `--stutter X` | Rows per table, slow frames to explain, stutter threshold as a multiple of the median frame. |
| `--symbols` | Function names from PDBs (Microsoft's symbol server, cached in `%LOCALAPPDATA%\benchlab\symbols`), for drivers and for user-mode modules of every process. |
| `--presentmon PATH`, `--no-presentmon` | PresentMon to run over the trace (default: `PRESENTMON` or `PATH`). |
| `--html`, `--json`, `--folded`, `--out` | Write the HTML report, everything as JSON, collapsed stacks of the focus process for flame graph tools (inferno, speedscope, flamegraph.pl), or the text report to a file. |

![benchlab trace analyze -s threads,ready,waits with symbols: the focus process's threads, ready time per process, wait reasons, wait sites and preempting processes](docs/images/trace-threads.png)

### How the numbers are computed

- **CPU usage (precise)** follows xperf's `-a cswitch -process`: a thread's time runs from its switch-in to its switch-out on the same CPU, so nothing before a CPU's first context switch or after its last counts.
- **Ready time** starts when a thread becomes ready (a ready-thread event, or being preempted) and ends when it is switched in. **Wait time** runs from switching out in the waiting state to the ready event.
- **Frames** are the time between consecutive Present calls of one swap chain (PresentMon's `MsBetweenPresents`). Min FPS is 1000 over the longest frame; "1% low" is 1000 over the 99th percentile frame time; "1% avg" is 1000 over the average of the slowest 1% of frames (rounded up to at least one frame), the definition most benchmark tools use.
- **Likely cause** of a slow frame, checked in this order: hard faults over a quarter of the frame, ready time over a quarter (CPU contention), DPC/ISR time over a fifth on the render thread's CPUs or a single call over 1 ms (drivers), running over 60% (CPU bound), GPU busy over 75% (GPU bound, needs PresentMon), waiting over half (and on what). It is a pointer to the right table, not a diagnosis.

## The DPC/ISR report and other trace commands

```sh
benchlab trace check                          # which tools were found, can this terminal record
benchlab trace record --delay 3 --timed 10    # the default dpc preset: 3 s to get ready, then 10 s, then the report
benchlab trace record --preset latency --presentmon --process game.exe --timed 30 --open-wpa
benchlab trace report traces\20261002-131500\trace.etl --symbols --top 20
benchlab trace merge a.etl b.etl -o merged.etl
benchlab trace open trace.etl --symbols       # WPA, with a symbol path set
benchlab trace stop                           # a trace was left running
```

`record` writes one folder per trace, named by `--session` (default: the UTC date and time), under `--out-dir` (default `traces`). With a preset that has context switches, samples or graphics events the folder gets the full report above; with `dpc` it gets the DPC/ISR report:

| Option of `trace record` | What it does |
|---|---|
| `--preset NAME` | `dpc` (default, small), `game`, `latency`, `cpu`, `disk`, `power` or `full`. `benchlab trace presets` lists the kernel flags and stack walks of each. |
| `--flags A+B`, `--stackwalk A+B`, `--no-stacks` | Your own kernel flags and stack walk events instead of a preset's. Flags are validated against xperf's list before anything starts. |
| `--graphics`, `--no-graphics` | Add or leave out the graphics session whatever the preset says. |
| `--delay S`, `--timed S` | Wait before starting; stop automatically. Without `--timed` it records until Enter or Ctrl+C, and both stop the trace cleanly. |
| `--out-dir DIR`, `--session NAME` | Where the trace goes. A session name that could escape the folder is refused. |
| `--process EXE` | The focus process of the report; it also limits a live PresentMon capture. |
| `--presentmon [PATH]` | Without the graphics session: run PresentMon live alongside. With it: the PresentMon used to analyse the trace afterwards. PresentMon is not bundled: pass its path, set `PRESENTMON`, or put it on `PATH`. PresentMon 1.x and 2.x argument styles and column names are both handled. |
| `--symbols`, `--functions` | Group by function instead of by driver (`--symbols` resolves real names, `--functions` gives `driver+offset`). |
| `--open-report`, `--open-wpa` | Open the report in the default editor, and the trace in WPA. |
| `--force` | Stop a kernel trace that is already running first. |

The DPC/ISR report has these sections:

| Section | What it tells you |
|---|---|
| ISR/DPC usage by CPU | For each CPU, the number of ISRs and DPCs, the time they took, and the share of that CPU's time. |
| Elapsed time per call, by driver | Count, total, mean, p50, p99, p99.9 and max of how long one call ran. High p99.9 or max values point at drivers that delay everything else on that CPU. |
| Interval between calls | How often each driver's routines run (1 ms is 1000 times per second), with calls per second and the CPUs they run on. |
| Histogram | Calls per power-of-two bucket of elapsed time. |
| Longest calls | The slowest ISRs and DPCs with their time in the trace, CPU and driver. |
| PresentMon | Per application: frames, average FPS, 1% and 0.1% low FPS (1000 over the p99 and p99.9 frame time), dropped frames, and mean, p50, p90, p99, p99.9 and max of every frame metric PresentMon reported (frame time, CPU and GPU busy and wait, displayed time, latencies). `benchlab trace frames file.csv` prints just this. |

![benchlab trace report: ISR and DPC usage per CPU, elapsed time and interval per driver function, a histogram and the longest calls](docs/images/trace-report.png)

The report format follows what [xtw](https://github.com/valleyofdoom/xtw) describes in its README (usage by CPU, interval, elapsed time, PresentMon). No code from xtw was used.

### How the trace is read

xperf records and merges, but `benchlab` reads the `.etl` file itself with the Windows trace consumer API, from the layouts of the kernel's event classes (`CSwitch`, `ReadyThread`, `SampledProfile`, `StackWalk_Event`, `PageFault_HardFault`, `DiskIo`, `FileIo_Name`, `Process`, `Thread`, `Image`, `PerfInfo` ISR and DPC) and the DXGI and D3D9 Present events. xperf's text output has a resolution of one microsecond, so an ISR that runs for 300 ns shows as 0; benchlab keeps the performance counter's ticks (100 ns here). Threads that only appear in the rundown at the end of a trace are attributed to their process afterwards.

`--symbols` names routines with DbgHelp (`dxgkrnl.sys!DpiFdoDpcForIsr` instead of `dxgkrnl.sys`). It needs a `dbghelp.dll` with `symsrv.dll` beside it (Debugging Tools for Windows, or the Performance Toolkit's own copy; set `BENCHLAB_DBGHELP` to its folder if it is not found), downloads PDBs from Microsoft's symbol server (or uses `_NT_SYMBOL_PATH`), and only uses a module file whose `TimeDateStamp` equals the one in the trace. Modules without public symbols, such as NVIDIA's, stay `module+offset`; the nearest export is never shown as if it were the function. The first `--symbols` run on a game trace took 49 seconds here, most of it downloading PDBs.

A merged trace covers the wall-clock span of all its parts, so rates (calls per second) of two traces recorded minutes apart are diluted by the gap. Merge traces recorded at the same time.

## The built-in benchmarks

| Group | Benchmarks | What they show |
|---|---|---|
| `cpu` | `int-latency`, `int-throughput`, `fp-latency`, `fp-throughput` | A chain of dependent steps against 8 independent chains of the same step: latency against instruction-level parallelism. Per op is one step. |
| `mem/read` | 32 KiB, 1 MiB, 32 MiB, 256 MiB | Sequential sum of a buffer, in GB/s (10^9 bytes per second). |
| `mem/write` | the same sizes | Sequential fill. |
| `mem/copy` | 1 MiB, 256 MiB | `copy_from_slice`. The rate counts bytes copied, not read plus written. |
| `cache/chase` | 4 KiB to 256 MiB in 10 steps | Dependent loads through a random cycle of cache lines: per op is the load latency, and the steps in the curve are the cache levels. |

## Results

Measured with `benchlab run --pin 2` (release build, generic x86-64 target, so memory loops use SSE2) on an Intel Core i9-14900KF, Windows 11, Rust 1.98.1, while other programs were running. In a pair of full runs, 23 of the 24 medians agreed within 3 percent; the 1 MiB copy differed by 12 percent, and the 16 MiB load-latency test varied by 9 to 37 percent between samples, so treat those two as rough. Samples are 50 per benchmark of about 10 ms each.

| Benchmark | Result |
|---|---|
| Integer step, dependent chain / 8 chains | 0.79 ns / 0.20 ns per step (4 times more with independent work) |
| f64 multiply-add, dependent chain / 8 chains | 1.36 ns / 0.17 ns per step |
| Sequential read at 32 KiB / 1 MiB / 32 MiB / 256 MiB | 130 / 118 / 35.8 / 24.0 GB/s |
| Sequential write at the same sizes | 318 / 82 / 39 / 19 GB/s |
| Copy at 1 MiB / 256 MiB | 69 / 18.6 GB/s |
| Load latency, working set 4 to 32 KiB | 1.19 ns |
| Load latency, 64 to 256 KiB / 1 MiB | 3.3 ns / 4.2 ns |
| Load latency, 4 MiB / 16 MiB | 13.4 ns / 18.2 ns |
| Load latency, 64 MiB / 256 MiB | 76.7 ns / 85.1 ns |

The latency steps line up with this CPU's caches (48 KiB L1 data, 2 MiB L2 per P-core, 36 MiB shared L3): the jumps are at 64 KiB, 4 MiB and 64 MiB. The 64 MiB and 256 MiB figures include page-table misses, because the buffers use ordinary 4 KiB pages.

## How it works

- **Calibration.** During the warm-up the batch size doubles until a batch takes a few milliseconds, then is scaled so one sample lasts the target time. The clock on Windows ticks every 100 ns (`benchlab info` measures it), so samples must be far longer than that.
- **Samples are per call.** Each sample is the time of a batch divided by its call count, so a 10 ms sample of a 20 ns function averages half a million calls. Every sample is kept in the report.
- **Summary.** Percentiles use linear interpolation between ranks (NumPy's default). The summary is computed after the outlier policy has been applied, and the outlier counts are always computed on all samples.
- **Regression test.** The Mann-Whitney U test makes no assumption that timings are normally distributed, which they rarely are. It uses the normal approximation with tie and continuity corrections, so very small samples (under about 8) are not reliable.
- **Pinning.** Windows `SetThreadAffinityMask` (first 64 logical CPUs) and Linux `sched_setaffinity`, declared directly with no extra crate.

## Verification

- `cargo test --release` runs 90 unit tests, 10 harness tests, 2 oracle tests, two live trace tests and a doc test. The harness tests check that calibration lands within the target (a 100 us busy loop gives 35 to 60 calls per 5 ms sample and a median of 98 to 130 us), the time cap, the JSON round trip, the compare verdicts (including a real 100 us against 150 us busy loop, and identical code not being flagged), pinning (the thread stays on the requested CPU, read back with `GetCurrentProcessorNumber`), and the CLI exit codes.
- The statistics are checked against NumPy 2.5.3 and SciPy 1.18.1 (`scripts/make_oracle.py` writes `tests/oracle.json`): mean, standard deviation, MAD, seven percentiles and the four outlier counts on 9 data sets, and the U statistic and p-value of `mannwhitneyu` on 5 pairs including tied data. They match to a relative 1e-9.
- The harness tests were run 15 times in a row without a failure.
- The live trace test records 3 seconds of kernel trace (it skips itself without administrator rights or xperf), decodes the same file with `xperf -a dumper`, and requires the same number of DPCs and ISRs on every CPU, the same DPC count for every driver both tools can name, and the same total time within xperf's one-microsecond rounding. It passed on this machine (about 10,000 DPCs and 9,500 ISRs in a 4 second trace). Breaking the image-name offset or dropping the message-signalled interrupt opcode on purpose makes it fail.
- While building the trace reader, comparing with xperf showed it was missing 7,249 of 9,583 ISRs: message-signalled interrupts use a second opcode. The test above is what caught it.
- **The game analysis is checked against a program with known stalls.** `examples/frames.rs` renders Direct3D 11 frames every 5 ms and every Nth frame stalls its render thread, alternately busy (a spin loop) and asleep, printing the performance-counter time of each stalled frame's Present. The live test `tests/live_game.rs` records it with `--preset game` and requires: every Present it made is in the trace (frames + 1 equals presents); every stall is a stutter and is explained within 1 ms of the time the program printed; every busy stall's likely cause is "CPU bound" and every sleeping one's is "waiting"; and CPU time of every process equals `xperf -a cswitch -process` within xperf's 1 us rounding. It passed 6 times in a row. The first run, before the test allowed for it, saw two extra stutters that were not stalls; they were not captured and did not recur, so the test now accepts machine hiccups under 25 ms and prints any it finds.
- On a 4.4 s trace of this machine, every context switch (63,209) and ready event (44,894) exported with `trace export` matched `xperf -a dumper` field for field (CPU, both threads and processes, priorities, state, wait reason, time within 1 us), as did all 2,012 samples (instruction pointer, thread, module), 107 hard faults (offset, size, duration, process) and 166 disk reads and writes (offset, size, disk, service time, file).
- On the game trace, frame time statistics equal PresentMon 2.6's analysis of the same file (mean 5.35 ms, p99 6.77 against 6.78, max 45.79 ms), and benchlab counts one frame more than PresentMon (1,121 against 1,120) because PresentMon drops the first.
- 20 hand-made mutations of the trace parser and analysis (field order, the SID skip, ready and preemption logic, the slowest-1% rounding, the stutter threshold, the DPC-on-its-CPUs check, CSV quoting, PresentMon column names) are each caught by the unit tests. The first round left five uncaught, which led to the tests for switch-in stacks, rounding, the threshold, same-process preemption and other-CPU DPCs; a sixth survivor was a redundant check and was removed.
- PresentMon 2.6 writes `MsCPUBusy`, `MsGPUBusy` and `TimeInMs` by default, which v0.2.0 did not read, so its PresentMon table had only frame time and display rows for 2.6 captures. Found while building this version and fixed.
- PresentMon statistics were checked against an independent Python calculation of the raw CSV (frame count, mean, p50, p90, p99, p99.9, max, FPS and 1% low agree).
- While building the integer benchmark, the first version (`x * k + 1` repeated) measured shorter than a multiply plus an add can take, because integer multiply-add chains can be algebraically merged by the compiler. It was replaced with a multiply, shift and xor step that cannot be merged, and the latency rose from 0.57 ns to 0.76 ns.

### Trace results

On an Intel Core i9-14900KF, Windows 11, Windows Performance Toolkit 10.0.26100, five runs each:

| Task | Time |
|---|---|
| `benchlab trace report` on a 47 MB trace (8 s, 24 CPUs, DPC, ISR, context switches and CPU samples with stacks) | 30 ms median (29 to 50) |
| `xperf -a dumper` on the same trace | 2,040 ms median |
| `xperf -a dpcisr` on the same trace | 119 ms median |

`record --timed 8` takes 9.1 seconds from start to report with either the `dpc` or the `latency` preset (one run each), so stopping, merging and reporting add about a second. The traces were 29 MB and 46 MB.

| Task, on a 100 MB `--preset game` trace (9 s, 24 CPUs, 179,489 context switches, 5,194 samples with stacks, 65,006 DPC/ISR, 2,664 presents) | Time, five runs |
|---|---|
| `benchlab trace analyze` (every section, no symbols, no PresentMon) | 268 ms median (255 to 323) |
| `xperf -a cswitch -process` (only the per-process CPU table) | 462 ms median (three runs) |

## Limits

- Wall-clock time only: no cycle counters, no hardware counters, no allocation tracking.
- Measures a closure in a loop; there is no per-iteration setup or teardown hook, so state that must be rebuilt every call is part of the measurement (see the clone baseline in the example).
- The microbenchmarks are compiled for generic x86-64. Building with `RUSTFLAGS="-C target-cpu=native"` raises the in-cache read and write rates.
- On Windows pinning covers the first 64 logical CPUs (one processor group).
- Linux and macOS were not run. The Linux build was compile-checked for `x86_64-unknown-linux-musl`; `benchlab trace` is Windows only. Recording and the live tests need an elevated terminal.
- Not read: GPU work per process from DxgKrnl DMA packets (use PresentMon's GPU busy, which `analyze` shows), file I/O other than names, registry, network, memory, power and idle states. Those stay in the trace for WPA.
- The frame explanation reads only what the trace shows: GPU-bound frames are recognised only with PresentMon, and a render thread waiting on another thread is reported as a wait (with its wait site) rather than followed to that thread.
- Stack frames in modules without public symbols are `module+offset`. Stacks are not collapsed across inlined frames.
- The graphics session needs Windows 10 or later (the event ids are Windows 11's); it was only run on Windows 11 23H2 with an NVIDIA RTX 5070 Ti. No AMD or Intel GPU and no full-screen exclusive game was tested.
- Tracing has overhead and uses the one NT Kernel Logger session Windows allows: recording fails (with a clear message) when another tool owns it, and `--force` stops that other session.
- ISR and DPC time is the kernel's own measurement of the routine; it does not include the interrupt latency before the routine started.
- The trace reader supports traces that use the performance counter or system time as their clock, not the CPU cycle counter.

## License

MIT (see `LICENSE`).
