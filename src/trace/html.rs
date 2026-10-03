//! A self-contained HTML report of `trace analyze`: headline numbers, time-line charts (frame
//! time, CPU, DPC/ISR, hard faults) with a hover readout, and every table of the text report.
//! No external scripts or fonts; light and dark themes follow the system.

use super::analysis::{IoRow, NameRow, Report};
use super::full::Analyzed;
use super::model::{Dist, Trace};
use super::render::num;

/// Escapes text for HTML.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

struct Html {
    out: String,
}

impl Html {
    fn table(&mut self, title: &str, note: &str, head: &[&str], left: usize, rows: Vec<Vec<String>>) {
        if rows.is_empty() {
            return;
        }
        self.out.push_str(&format!("<section><h2>{}</h2>", esc(title)));
        if !note.is_empty() {
            self.out.push_str(&format!("<p class=\"note\">{}</p>", esc(note)));
        }
        self.out.push_str("<div class=\"scroll\"><table><thead><tr>");
        for (i, h) in head.iter().enumerate() {
            self.out.push_str(&format!("<th{}>{}</th>", if i < left { " class=\"l\"" } else { "" }, esc(h)));
        }
        self.out.push_str("</tr></thead><tbody>");
        for r in rows {
            self.out.push_str("<tr>");
            for (i, c) in r.iter().enumerate() {
                self.out.push_str(&format!("<td{}>{}</td>", if i < left { " class=\"l\"" } else { "" }, esc(c)));
            }
            self.out.push_str("</tr>");
        }
        self.out.push_str("</tbody></table></div></section>");
    }
}

fn name_rows(rows: &[NameRow], with_ms: bool) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| {
            let mut v = vec![r.name.clone(), r.count.to_string()];
            if with_ms {
                v.push(num(r.ms));
            }
            v.push(format!("{:.1}", r.pct));
            v
        })
        .collect()
}

fn io_rows(rows: &[IoRow]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| {
            vec![r.name.clone(), r.count.to_string(), format!("{:.2}", r.bytes as f64 / 1e6), num(r.total_ms), num(r.max_ms)]
        })
        .collect()
}

fn dist(d: &Dist) -> Vec<String> {
    vec![num(d.mean), num(d.p50), num(d.p90), num(d.p99), num(d.p999), num(d.max)]
}

/// Numbers for the charts, as a JSON object the page's script reads.
fn chart_data(r: &Report) -> String {
    let t = &r.timeline;
    let round = |v: &[f64]| -> Vec<f64> { v.iter().map(|x| (x * 1000.0).round() / 1000.0).collect() };
    // Keep the page small: at most 20,000 frame points (every frame of a 2-minute 165 fps run).
    let step = (t.frames.len() / 20_000).max(1);
    let frames: Vec<[f64; 2]> =
        t.frames.iter().step_by(step).map(|f| [(f.0 * 100.0).round() / 100.0, (f.1 * 1000.0).round() / 1000.0]).collect();
    let marks: Vec<[f64; 2]> = r.explained.iter().map(|x| [x.at_ms, x.frame_ms]).collect();
    let threshold =
        r.frames.iter().find(|f| Some(format!("{} ({})", f.process, f.pid)) == r.overview.focus).map(|f| f.stutter_threshold_ms);
    serde_json::json!({
        "bucket": t.bucket_ms,
        "start": t.start_ms,
        "end": t.start_ms + t.bucket_ms * t.dpc_isr_pct.len().max(t.cpu_busy_pct.len()).max(t.hard_faults.len()) as f64,
        "busy": round(&t.cpu_busy_pct),
        "focus": round(&t.focus_cpu_pct),
        "dpc": round(&t.dpc_isr_pct),
        "faults": t.hard_faults,
        "frames": frames,
        "marks": marks,
        "threshold": threshold,
        "focusName": r.overview.focus.clone().unwrap_or_default(),
    })
    .to_string()
    .replace("</", "<\\/")
}

pub fn render(a: &Analyzed, trace: &Trace, source: &str) -> String {
    let r = &a.report;
    let o = &r.overview;
    let mut h = Html { out: String::new() };
    let focus = o.focus.clone().unwrap_or_else(|| "none".into());

    // Headline tiles for the focus process's main swap chain.
    let main = r.frames.iter().find(|f| Some(format!("{} ({})", f.process, f.pid)) == o.focus).or(r.frames.first());
    h.out.push_str("<section class=\"tiles\">");
    let mut tile = |label: &str, value: String, unit: &str| {
        h.out.push_str(&format!(
            "<div class=\"tile\"><div class=\"tl\">{}</div><div class=\"tv\">{}<span>{}</span></div></div>",
            esc(label),
            esc(&value),
            esc(unit)
        ));
    };
    if let Some(f) = main {
        tile("average", format!("{:.1}", f.avg_fps), " fps");
        tile("1% low (avg)", format!("{:.1}", f.low_1_avg_fps), " fps");
        tile("0.1% low (avg)", format!("{:.1}", f.low_01_avg_fps), " fps");
        tile("min", format!("{:.1}", f.min_fps), " fps");
        tile("stutters", f.stutters.to_string(), &format!(" > {} ms", num(f.stutter_threshold_ms)));
    }
    if let Some(p) = r.processes.iter().find(|p| Some(format!("{} ({})", p.name, p.pid)) == o.focus) {
        tile("focus CPU", format!("{:.1}", p.cpu_pct), " % CPU");
    }
    let dpc_total: f64 = r.cpus.iter().map(|c| c.dpc_isr_pct).sum::<f64>() / r.cpus.len().max(1) as f64;
    tile("DPC/ISR", format!("{dpc_total:.3}"), " % CPU");
    h.out.push_str("</section>");
    if main.is_some() {
        h.out.push_str("<p class=\"note\">1% and 0.1% low: 1000 over the average of the slowest 1% and 0.1% of frames. Min: 1000 over the longest frame.</p>");
    }

    for n in &r.notes {
        h.out.push_str(&format!("<p class=\"warn\">Note: {}</p>", esc(n)));
    }
    if let Some(n) = &a.presentmon_note {
        h.out.push_str(&format!("<p class=\"warn\">Note: {}</p>", esc(n)));
    }

    // Chart slots; the script draws into them.
    h.out.push_str(
        "<section><h2>Time line</h2><div id=\"charts\"></div><p class=\"note\">Hover a chart to read the values at that moment. \
         Dots on the frame chart mark the frames explained below; the dashed line is the stutter threshold.</p></section>",
    );

    let fps = |v: f64| format!("{v:.1}");
    h.table(
        "Frames per swap chain",
        "From the Present calls in the trace. 1% and 0.1% low use the 99th and 99.9th percentile frame time; the avg columns use the average of the slowest frames.",
        &["process", "API", "frames", "seconds", "avg fps", "min fps", "1% low", "0.1% low", "1% avg", "0.1% avg", "stutters"],
        2,
        r.frames
            .iter()
            .map(|f| {
                vec![
                    format!("{} ({})", f.process, f.pid),
                    f.runtime.clone(),
                    f.frames.to_string(),
                    format!("{:.2}", f.seconds),
                    fps(f.avg_fps),
                    fps(f.min_fps),
                    fps(f.low_1_fps),
                    fps(f.low_01_fps),
                    fps(f.low_1_avg_fps),
                    fps(f.low_01_avg_fps),
                    f.stutters.to_string(),
                ]
            })
            .collect(),
    );
    h.table(
        "Frame time (milliseconds)",
        "",
        &["process", "mean", "p50", "p90", "p99", "p99.9", "max"],
        1,
        r.frames
            .iter()
            .map(|f| {
                let mut v = vec![format!("{} ({})", f.process, f.pid)];
                v.extend(dist(&f.frame_ms));
                v
            })
            .collect(),
    );
    if let Some(pm) = &a.presentmon {
        for app in &pm.apps {
            h.table(
                &format!("PresentMon: {} ({} frames)", app.application, app.frames),
                "Milliseconds.",
                &["metric", "mean", "p50", "p90", "p99", "p99.9", "max"],
                1,
                app.metrics
                    .iter()
                    .map(|m| {
                        let mut v = vec![m.name.clone()];
                        v.extend(dist(&m.ms));
                        v
                    })
                    .collect(),
            );
        }
    }

    if !r.explained.is_empty() {
        h.out.push_str("<section><h2>Slowest frames, explained</h2>");
        for x in &r.explained {
            h.out.push_str(&format!(
                "<div class=\"card\"><div class=\"ch\"><b>{:.2} ms</b> at {:.1} ms ({:.1}x the median)</div><div class=\"cv\">{}</div><ul>",
                x.frame_ms,
                x.at_ms,
                if x.median_ms > 0.0 { x.frame_ms / x.median_ms } else { 0.0 },
                esc(&x.verdict)
            ));
            let waits: Vec<String> = x.waits.iter().take(3).map(|w| format!("{} {:.2} ms", w.reason, w.total_ms)).collect();
            h.out.push_str(&format!(
                "<li>Render thread {}: running {:.2} ms, ready {:.2} ms, waiting {:.2} ms{}</li>",
                x.tid,
                x.running_ms,
                x.ready_ms,
                x.waiting_ms,
                if waits.is_empty() { String::new() } else { esc(&format!(" ({})", waits.join(", "))) }
            ));
            let mut line = format!(
                "Process on CPU {:.2} ms; DPC/ISR {:.3} ms ({:.3} ms on its CPUs)",
                x.process_cpu_ms, x.dpc_isr_ms, x.dpc_isr_on_its_cpus_ms
            );
            if let Some((n, us, cpu)) = &x.longest_dpc_isr {
                line.push_str(&format!(", longest {} us in {n} on CPU {cpu}", num(*us)));
            }
            if let Some(g) = x.gpu_busy_ms {
                line.push_str(&format!("; GPU busy {g:.2} ms"));
            }
            h.out.push_str(&format!("<li>{}</li>", esc(&line)));
            if x.hard_faults > 0 || x.disk_ios > 0 {
                h.out.push_str(&format!(
                    "<li>Hard faults {} ({:.2} ms), disk I/Os {}</li>",
                    x.hard_faults, x.hard_fault_ms, x.disk_ios
                ));
            }
            if !x.preempted_by.is_empty() {
                let p: Vec<String> = x.preempted_by.iter().map(|p| format!("{} x{}", p.name, p.count)).collect();
                h.out.push_str(&format!("<li>Preempted by {}</li>", esc(&p.join(", "))));
            }
            if !x.top_functions.is_empty() {
                let f: Vec<String> = x.top_functions.iter().map(|p| format!("{} ({})", p.name, p.count)).collect();
                h.out.push_str(&format!("<li>Samples: {}</li>", esc(&f.join(", "))));
            }
            h.out.push_str("</ul></div>");
        }
        h.out.push_str("</section>");
    }

    h.table(
        "Processes: CPU usage (precise)",
        "From context switches. CPU % is the share of all CPUs' time.",
        &["process", "pid", "CPU ms", "CPU %", "samples", "switch-ins", "threads", "ready ms", "hard faults", "disk MB"],
        1,
        r.processes
            .iter()
            .filter(|p| p.pid != 0)
            .take(40)
            .map(|p| {
                vec![
                    p.name.clone(),
                    p.pid.to_string(),
                    num(p.cpu_ms),
                    format!("{:.2}", p.cpu_pct),
                    p.samples.to_string(),
                    p.switches_in.to_string(),
                    p.threads.to_string(),
                    num(p.ready_ms),
                    p.hard_faults.to_string(),
                    format!("{:.2}", p.disk_bytes as f64 / 1e6),
                ]
            })
            .collect(),
    );
    h.table(
        &format!("Threads of {focus}"),
        "",
        &["tid", "name", "CPU ms", "samples", "switch-ins", "ready ms", "ready p99 us", "wait ms", "most waited on"],
        2,
        r.threads
            .iter()
            .map(|t| {
                vec![
                    t.tid.to_string(),
                    t.name.clone(),
                    num(t.cpu_ms),
                    t.samples.to_string(),
                    t.switches_in.to_string(),
                    num(t.ready_ms),
                    num(t.ready_us.p99),
                    num(t.wait_ms),
                    t.top_wait.clone(),
                ]
            })
            .collect(),
    );
    h.table(
        "Logical CPUs",
        "",
        &["CPU", "busy %", "switches", "DPC/ISR %"],
        0,
        r.cpus
            .iter()
            .map(|c| {
                vec![
                    c.cpu.to_string(),
                    if c.busy_pct.is_nan() { "n/a".into() } else { format!("{:.1}", c.busy_pct) },
                    c.switches.to_string(),
                    format!("{:.3}", c.dpc_isr_pct),
                ]
            })
            .collect(),
    );
    h.table(
        "CPU usage (sampled) by process and module",
        "Idle excluded.",
        &["process module", "samples", "est. ms", "%"],
        1,
        name_rows(&r.sampled_modules, true),
    );
    h.table(
        "CPU usage (sampled) by function",
        "Where the instruction pointer was.",
        &["function", "samples", "est. ms", "%"],
        1,
        name_rows(&r.sampled_functions, true),
    );
    h.table(
        &format!("{focus}: inclusive by function"),
        "Functions anywhere on the call stack; % of the process's samples with stacks.",
        &["function", "samples", "est. ms", "%"],
        1,
        name_rows(&r.inclusive_functions, true),
    );
    h.table(
        "Ready time per process (microseconds)",
        "How long runnable threads waited for a CPU.",
        &["process", "count", "total ms", "mean", "p50", "p90", "p99", "p99.9", "max"],
        1,
        r.ready
            .iter()
            .map(|x| {
                let mut v = vec![x.name.clone(), x.count.to_string(), num(x.total_ms)];
                v.extend(dist(&x.us));
                v
            })
            .collect(),
    );
    h.table(
        &format!("Why {focus}'s threads waited"),
        "",
        &["reason", "waits", "total ms", "longest ms"],
        1,
        r.waits.iter().map(|w| vec![w.reason.clone(), w.count.to_string(), num(w.total_ms), num(w.max_ms)]).collect(),
    );
    h.table(
        "Where they waited",
        "First frame past the system's wait code, from switch-in stacks.",
        &["function", "switch-ins", "%"],
        1,
        name_rows(&r.wait_sites, false),
    );
    h.table("Preempted by", "", &["process", "times", "%"], 1, name_rows(&r.preempted_by, false));
    h.table(
        "Hard page faults by process",
        "",
        &["process", "count", "MB", "total ms", "max ms"],
        1,
        io_rows(&r.faults_by_process),
    );
    h.table("Hard page faults by file", "", &["file", "count", "MB", "total ms", "max ms"], 1, io_rows(&r.faults_by_file));
    h.table(
        "Disk I/O by process",
        "Total ms is disk service time.",
        &["process", "count", "MB", "total ms", "max ms"],
        1,
        io_rows(&r.disk_by_process),
    );
    h.table("Disk I/O by file", "", &["file", "count", "MB", "total ms", "max ms"], 1, io_rows(&r.disk_by_file));
    h.table(
        "DPC/ISR elapsed time per driver (microseconds)",
        "",
        &["driver", "kind", "calls", "total ms", "mean", "p50", "p99", "p99.9", "max", "per second"],
        2,
        a.dpc
            .groups
            .iter()
            .take(20)
            .map(|g| {
                vec![
                    g.name.clone(),
                    if g.isr { "ISR".into() } else { "DPC".into() },
                    g.count.to_string(),
                    num(g.total_us / 1000.0),
                    num(g.elapsed_us.mean),
                    num(g.elapsed_us.p50),
                    num(g.elapsed_us.p99),
                    num(g.elapsed_us.p999),
                    num(g.elapsed_us.max),
                    num(g.per_second),
                ]
            })
            .collect(),
    );
    h.out.push_str(&format!(
        "<section><details><summary>Full text report</summary><pre>{}</pre></details></section>",
        esc(&a.text)
    ));

    let title = format!("benchlab trace: {}", if focus == "none" { "analysis".to_string() } else { focus.clone() });
    let subtitle = format!(
        "{} &middot; {} CPUs &middot; {:.2} s analysed &middot; {} context switches &middot; {} samples &middot; {} presents",
        esc(source),
        o.cpu_count,
        o.to_s - o.from_s,
        o.switches,
        o.samples,
        o.presents
    );
    let _ = trace;
    PAGE.replace("{{TITLE}}", &esc(&title))
        .replace("{{SUBTITLE}}", &subtitle)
        .replace("{{BODY}}", &h.out)
        .replace("{{DATA}}", &chart_data(r))
}

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{TITLE}}</title>
<style>
:root {
  color-scheme: light;
  --surface-0: #f4f3f0;
  --surface-1: #fcfcfb;
  --border: #e2e0da;
  --grid: #ecebe7;
  --text-primary: #0b0b0b;
  --text-secondary: #52514e;
  --text-muted: #7a7974;
  --series-1: #2a78d6;
  --series-2: #eb6834;
  --warn-bg: #fff4dc;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;
    --surface-0: #121211;
    --surface-1: #1a1a19;
    --border: #33332f;
    --grid: #2a2a27;
    --text-primary: #ffffff;
    --text-secondary: #c3c2b7;
    --text-muted: #8f8e86;
    --series-1: #3987e5;
    --series-2: #d95926;
    --warn-bg: #3a2f14;
  }
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --surface-0: #121211;
  --surface-1: #1a1a19;
  --border: #33332f;
  --grid: #2a2a27;
  --text-primary: #ffffff;
  --text-secondary: #c3c2b7;
  --text-muted: #8f8e86;
  --series-1: #3987e5;
  --series-2: #d95926;
  --warn-bg: #3a2f14;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--surface-0); color: var(--text-primary);
  font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 1200px; margin: 0 auto; padding: 24px 16px 64px; }
h1 { font-size: 22px; margin: 0 0 4px; }
.sub { color: var(--text-secondary); margin: 0 0 20px; overflow-wrap: anywhere; }
h2 { font-size: 16px; margin: 0 0 8px; }
section { background: var(--surface-1); border: 1px solid var(--border); border-radius: 8px; padding: 16px; margin: 0 0 16px; }
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 12px; background: none; border: 0; padding: 0; }
.tile { background: var(--surface-1); border: 1px solid var(--border); border-radius: 8px; padding: 12px 14px; }
.tl { color: var(--text-secondary); font-size: 12px; }
.tv { font-size: 26px; font-weight: 600; font-variant-numeric: tabular-nums; }
.tv span { font-size: 13px; font-weight: 400; color: var(--text-muted); }
.note { color: var(--text-muted); font-size: 12px; margin: 0 0 8px; }
.warn { background: var(--warn-bg); border-radius: 6px; padding: 8px 12px; margin: 0 0 12px; }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-variant-numeric: tabular-nums; font-size: 13px; }
th, td { padding: 4px 8px; text-align: right; white-space: nowrap; border-bottom: 1px solid var(--grid); }
th { color: var(--text-secondary); font-weight: 600; }
.l { text-align: left; }
td.l { max-width: 520px; overflow: hidden; text-overflow: ellipsis; }
.card { border: 1px solid var(--border); border-radius: 6px; padding: 10px 12px; margin: 0 0 10px; }
.ch { font-size: 15px; }
.cv { color: var(--text-secondary); margin: 2px 0 4px; }
.card ul { margin: 0; padding-left: 18px; color: var(--text-secondary); }
pre { overflow-x: auto; font-size: 12px; }
.chart { position: relative; margin: 0 0 14px; }
.chart h3 { font-size: 13px; font-weight: 600; margin: 0 0 2px; color: var(--text-secondary); }
.legend { font-size: 12px; color: var(--text-secondary); display: flex; gap: 14px; margin: 0 0 2px; }
.sw { display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin-right: 5px; vertical-align: -1px; }
svg { display: block; width: 100%; height: 160px; }
.tip { position: absolute; pointer-events: none; background: var(--surface-1); border: 1px solid var(--border);
  border-radius: 6px; padding: 6px 8px; font-size: 12px; color: var(--text-primary); box-shadow: 0 2px 8px rgba(0,0,0,.15);
  white-space: nowrap; display: none; font-variant-numeric: tabular-nums; }
</style>
</head>
<body>
<main>
<h1>{{TITLE}}</h1>
<p class="sub">{{SUBTITLE}}</p>
{{BODY}}
</main>
<script id="data" type="application/json">{{DATA}}</script>
<script>
(function () {
  var D = JSON.parse(document.getElementById('data').textContent);
  var root = document.getElementById('charts');
  if (!root) return;
  var NS = 'http://www.w3.org/2000/svg';
  function el(tag, attrs) { var e = document.createElementNS(NS, tag); for (var k in attrs) e.setAttribute(k, attrs[k]); return e; }
  function fmt(v) { return Math.abs(v) >= 100 ? v.toFixed(0) : Math.abs(v) >= 10 ? v.toFixed(1) : v.toFixed(2); }
  // series: [{name, color, pts:[[x,y],...]}]; one y axis, starting at zero.
  function chart(title, unit, series, opts) {
    opts = opts || {};
    var all = [];
    series.forEach(function (s) { s.pts.forEach(function (p) { all.push(p); }); });
    if (!all.length) return;
    var box = document.createElement('div'); box.className = 'chart';
    var h = document.createElement('h3'); h.textContent = title + ' (' + unit + ')'; box.appendChild(h);
    if (series.length > 1) {
      var lg = document.createElement('div'); lg.className = 'legend';
      series.forEach(function (s) { var i = document.createElement('span'); i.innerHTML = '<span class="sw" style="background:' + s.color + '"></span>'; i.appendChild(document.createTextNode(s.name)); lg.appendChild(i); });
      box.appendChild(lg);
    }
    var W = 1000, H = 160, L = 44, R = 8, T = 8, B = 22;
    var svg = el('svg', { viewBox: '0 0 ' + W + ' ' + H, preserveAspectRatio: 'none', role: 'img', 'aria-label': title });
    // Every chart shares the analysed window as its time axis, so they line up vertically.
    var x0 = D.start, x1 = D.end, y1 = 0;
    all.forEach(function (p) { if (p[1] > y1) y1 = p[1]; });
    if (opts.ref && opts.ref > y1) y1 = opts.ref;
    y1 = y1 > 0 ? y1 * 1.08 : 1; if (x1 <= x0) x1 = x0 + 1;
    var X = function (v) { return L + (v - x0) / (x1 - x0) * (W - L - R); };
    var Y = function (v) { return T + (1 - v / y1) * (H - T - B); };
    for (var g = 0; g <= 4; g++) {
      var gv = y1 / 4 * g, gy = Y(gv);
      svg.appendChild(el('line', { x1: L, x2: W - R, y1: gy, y2: gy, stroke: 'var(--grid)', 'stroke-width': 1, 'vector-effect': 'non-scaling-stroke' }));
      var t = el('text', { x: L - 6, y: gy + 4, 'text-anchor': 'end', 'font-size': 11, fill: 'var(--text-muted)' }); t.textContent = fmt(gv); svg.appendChild(t);
    }
    for (var k = 0; k <= 5; k++) {
      var xv = x0 + (x1 - x0) / 5 * k;
      var tx = el('text', { x: X(xv), y: H - 6, 'text-anchor': k === 0 ? 'start' : k === 5 ? 'end' : 'middle', 'font-size': 11, fill: 'var(--text-muted)' });
      tx.textContent = (xv / 1000).toFixed(xv / 1000 >= 10 ? 0 : 1) + ' s'; svg.appendChild(tx);
    }
    if (opts.ref) svg.appendChild(el('line', { x1: L, x2: W - R, y1: Y(opts.ref), y2: Y(opts.ref), stroke: 'var(--text-muted)', 'stroke-dasharray': '4 4', 'stroke-width': 1, 'vector-effect': 'non-scaling-stroke' }));
    series.forEach(function (s) {
      var d = s.pts.map(function (p, i) { return (i ? 'L' : 'M') + X(p[0]).toFixed(1) + ' ' + Y(p[1]).toFixed(1); }).join('');
      svg.appendChild(el('path', { d: d, fill: 'none', stroke: s.color, 'stroke-width': 2, 'stroke-linejoin': 'round', 'vector-effect': 'non-scaling-stroke' }));
    });
    (opts.marks || []).forEach(function (m) {
      svg.appendChild(el('circle', { cx: X(m[0]), cy: Y(m[1]), r: 4, fill: 'var(--series-1)', stroke: 'var(--surface-1)', 'stroke-width': 2, 'vector-effect': 'non-scaling-stroke' }));
    });
    var cross = el('line', { y1: T, y2: H - B, stroke: 'var(--text-muted)', 'stroke-width': 1, 'vector-effect': 'non-scaling-stroke', visibility: 'hidden' });
    svg.appendChild(cross);
    box.appendChild(svg);
    var tip = document.createElement('div'); tip.className = 'tip'; box.appendChild(tip);
    function nearest(pts, xv) { var lo = 0, hi = pts.length - 1; while (lo < hi) { var mid = (lo + hi) >> 1; if (pts[mid][0] < xv) lo = mid + 1; else hi = mid; } if (lo > 0 && Math.abs(pts[lo - 1][0] - xv) < Math.abs(pts[lo][0] - xv)) lo--; return pts[lo]; }
    svg.addEventListener('mousemove', function (e) {
      var r = svg.getBoundingClientRect();
      var px = (e.clientX - r.left) / r.width * W;
      var xv = x0 + (px - L) / (W - L - R) * (x1 - x0);
      if (xv < x0 || xv > x1) { tip.style.display = 'none'; cross.setAttribute('visibility', 'hidden'); return; }
      var p0 = nearest(series[0].pts, xv);
      cross.setAttribute('x1', X(p0[0])); cross.setAttribute('x2', X(p0[0])); cross.setAttribute('visibility', 'visible');
      var html = '<b>' + (p0[0] / 1000).toFixed(3) + ' s</b>';
      series.forEach(function (s) { var p = nearest(s.pts, xv); html += '<br><span class="sw" style="background:' + s.color + '"></span>' + s.name + ': ' + fmt(p[1]) + ' ' + unit; });
      tip.innerHTML = html; tip.style.display = 'block';
      var left = e.clientX - r.left + 12; if (left > r.width - 180) left = e.clientX - r.left - 190;
      tip.style.left = left + 'px'; tip.style.top = '24px';
    });
    svg.addEventListener('mouseleave', function () { tip.style.display = 'none'; cross.setAttribute('visibility', 'hidden'); });
    root.appendChild(box);
  }
  function bucketed(v) { return v.map(function (y, i) { return [D.start + (i + 0.5) * D.bucket, y]; }); }
  var c1 = getComputedStyle(document.documentElement).getPropertyValue('--series-1').trim();
  var c2 = getComputedStyle(document.documentElement).getPropertyValue('--series-2').trim();
  if (D.frames.length) chart('Frame time, ' + D.focusName, 'ms', [{ name: 'frame time', color: c1, pts: D.frames }], { ref: D.threshold, marks: D.marks });
  if (D.busy.length) {
    var s = [{ name: 'all processes', color: c1, pts: bucketed(D.busy) }];
    if (D.focus.length) s.push({ name: D.focusName, color: c2, pts: bucketed(D.focus) });
    chart('CPU usage, share of all CPUs', '%', s);
  }
  if (D.dpc.some(function (v) { return v > 0; })) chart('DPC and ISR time, share of all CPUs', '%', [{ name: 'DPC/ISR', color: c1, pts: bucketed(D.dpc) }]);
  if (D.faults.some(function (v) { return v > 0; })) chart('Hard page faults per ' + fmt(D.bucket) + ' ms', 'faults', [{ name: 'hard faults', color: c1, pts: bucketed(D.faults) }]);
})();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::analysis::{FrameStats, Overview, Timeline};
    use crate::trace::full::{analyze_trace, AnalyzeOptions, PresentMonChoice};

    #[test]
    fn escaping() {
        assert_eq!(esc("<a href=\"x\">&'</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn page_is_complete_and_data_cannot_close_the_script() {
        let t = Trace::default();
        let o = AnalyzeOptions { presentmon: PresentMonChoice::Off, ..Default::default() };
        let mut a = analyze_trace(&t, std::path::Path::new("x.etl"), &o).unwrap();
        a.report.overview = Overview { focus: Some("evil</script>.exe (1)".into()), ..Default::default() };
        a.report.frames =
            vec![FrameStats { pid: 1, process: "evil</script>.exe".into(), frames: 3, avg_fps: 60.0, ..Default::default() }];
        a.report.timeline = Timeline { frames: vec![(1.0, 16.6), (2.0, 16.7)], ..Default::default() };
        let page = render(&a, &t, "x.etl");
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<title>benchlab trace: evil&lt;/script&gt;.exe (1)</title>"));
        // Exactly the two real script end tags.
        assert_eq!(page.matches("</script>").count(), 2);
        assert!(page.contains("60.0"));
        assert!(!page.contains("{{"));
    }
}
