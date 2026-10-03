//! Reads a trace (`.etl`) with the Windows trace consumer API: DPC, ISR and image-load events for
//! the driver report, and the system events of [`super::sys`] for the full analysis. Timestamps
//! are taken raw and converted here, so durations keep the full resolution of the performance
//! counter instead of the whole microseconds that xperf prints.

use std::collections::HashMap;

use super::model::{base_name, Event, Image, Kind, Trace};
use super::sys::{
    self, DiskIo, HardFault, IoKind, Present, Process, Reader, Ready, Runtime, Sample, Switch, SysEvents, Thread, NO_STACK,
};

/// PerfInfo event group: ISR, DPC, timer DPC, thread DPC, CPU samples.
pub const PERFINFO: u128 = 0xce1dbfb4_137e_4da6_87b0_3f59aa102cbc;
/// Image event group: module load, unload and rundown.
pub const IMAGE: u128 = 0x2cb15d1d_5fc1_11d2_abe1_00a0c911f518;

const OP_IMAGE_DC_START: u8 = 3;
const OP_IMAGE_DC_END: u8 = 4;
const OP_IMAGE_LOAD: u8 = 10;
const OP_THREAD_DPC: u8 = 66;
const OP_ISR_MSI: u8 = 50;
const OP_ISR: u8 = 67;
const OP_DPC: u8 = 68;
const OP_TIMER_DPC: u8 = 69;
const OP_SAMPLE: u8 = 46;
const OP_SET_INTERVAL: u8 = 72;
const OP_INTERVAL_START: u8 = 73;

const OP_START: u8 = 1;
const OP_END: u8 = 2;
const OP_DC_START: u8 = 3;
const OP_DC_END: u8 = 4;
const OP_CSWITCH: u8 = 36;
const OP_READY: u8 = 50;
const OP_THREAD_NAME: u8 = 72;
const OP_HARD_FAULT: u8 = 32;
const OP_STACK: u8 = 32;
const OP_DISK_READ: u8 = 10;
const OP_DISK_WRITE: u8 = 11;
const OP_DISK_FLUSH: u8 = 14;
const OP_FILE_NAME: u8 = 0;
const OP_FILE_CREATE: u8 = 32;
const OP_FILE_RUNDOWN: u8 = 36;

const DXGI_PRESENT_START: u16 = 42;
const DXGI_PRESENT_STOP: u16 = 43;
const DXGI_MPO_START: u16 = 55;
const DXGI_MPO_STOP: u16 = 56;
const D3D9_PRESENT_START: u16 = 1;
const D3D9_PRESENT_STOP: u16 = 2;

/// One event as the consumer hands it over.
pub struct Raw<'a> {
    pub provider: u128,
    pub opcode: u8,
    pub version: u8,
    /// Event id (manifest providers; 0 for the kernel's classic events).
    pub id: u16,
    /// Raw timestamp in clock ticks.
    pub ts: i64,
    pub cpu: u16,
    /// Process and thread from the event header (meaningful for manifest providers).
    pub pid: u32,
    pub tid: u32,
    /// Pointer size of the traced machine, 4 or 8.
    pub ptr: usize,
    pub data: &'a [u8],
}

#[derive(Clone, Copy)]
struct RawEvent {
    kind: Kind,
    cpu: u16,
    start: i64,
    elapsed: i64,
    routine: u64,
    vector: u16,
}

/// What a stack walk event belongs to.
#[derive(Clone, Copy)]
enum StackOwner {
    Sample(usize),
    Switch(usize),
}

/// Accumulates events while a trace is being read. Times in [`SysEvents`] hold raw ticks until
/// [`Collector::finish`] converts them.
pub struct Collector {
    /// Clock ticks per second.
    freq: u64,
    events: Vec<RawEvent>,
    images: Vec<Image>,
    first_ts: i64,
    last_ts: i64,
    pub malformed: u64,
    sys: SysEvents,
    /// Thread id to process id, as of the event being read.
    owner: HashMap<u32, u32>,
    stack_ids: HashMap<Vec<u64>, u32>,
    pending_stacks: HashMap<(i64, u32), StackOwner>,
    open_presents: HashMap<u32, usize>,
    /// Hard faults are logged when they end; their start is in the payload.
    hard_fault_starts: Vec<i64>,
}

impl Collector {
    pub fn new(freq: u64) -> Collector {
        Collector {
            freq: freq.max(1),
            events: Vec::new(),
            images: Vec::new(),
            first_ts: i64::MAX,
            last_ts: i64::MIN,
            malformed: 0,
            sys: SysEvents::default(),
            owner: HashMap::new(),
            stack_ids: HashMap::new(),
            pending_stacks: HashMap::new(),
            open_presents: HashMap::new(),
            hard_fault_starts: Vec::new(),
        }
    }

    fn pid_of(&self, tid: u32) -> u32 {
        if tid == 0 {
            0
        } else {
            self.owner.get(&tid).copied().unwrap_or(u32::MAX)
        }
    }

    pub fn record(&mut self, r: &Raw) {
        self.first_ts = self.first_ts.min(r.ts);
        self.last_ts = self.last_ts.max(r.ts);
        let ok = match r.provider {
            PERFINFO => self.perfinfo(r),
            IMAGE if matches!(r.opcode, OP_IMAGE_LOAD | OP_IMAGE_DC_START | OP_IMAGE_DC_END) => self.image(r),
            sys::THREAD => self.thread(r),
            sys::PROCESS => self.process(r),
            sys::STACK_WALK if r.opcode == OP_STACK => self.stack(r),
            sys::PAGE_FAULT if r.opcode == OP_HARD_FAULT => self.hard_fault(r),
            sys::DISK_IO => self.disk(r),
            sys::FILE_IO if matches!(r.opcode, OP_FILE_NAME | OP_FILE_CREATE | OP_FILE_RUNDOWN) => {
                sys::parse_file_name(r.data, r.ptr).map(|(f, name)| {
                    self.sys.file_names.insert(f, name);
                })
            }
            sys::DXGI => self.present(r, Runtime::Dxgi),
            sys::D3D9 => self.present(r, Runtime::D3d9),
            _ => Some(()),
        };
        if ok.is_none() {
            self.malformed += 1;
        }
    }

    fn perfinfo(&mut self, r: &Raw) -> Option<()> {
        let kind = match r.opcode {
            OP_ISR | OP_ISR_MSI => Kind::Isr,
            OP_DPC => Kind::Dpc,
            OP_TIMER_DPC => Kind::TimerDpc,
            OP_THREAD_DPC => Kind::ThreadDpc,
            OP_SAMPLE => {
                let (ip, tid) = sys::parse_sample(r.data, r.ptr)?;
                let idx = self.sys.samples.len();
                self.sys.samples.push(Sample { t: r.ts as u64, cpu: r.cpu, tid, pid: self.pid_of(tid), ip, stack: NO_STACK });
                self.pending_stacks.insert((r.ts, tid), StackOwner::Sample(idx));
                return Some(());
            }
            OP_SET_INTERVAL | OP_INTERVAL_START => {
                let (source, interval) = sys::parse_interval(r.data)?;
                if source == 0 && interval > 0 {
                    self.sys.sample_interval_ns = Some(interval as u64 * 100);
                }
                return Some(());
            }
            _ => return Some(()),
        };
        let mut p = Reader::new(r.data);
        let initial = p.uint(8)? as i64;
        let routine = p.uint(r.ptr)?;
        let vector = if kind.is_isr() {
            // ReturnValue (u8), then the vector (u16).
            p.take(1)?;
            p.uint(2)? as u16
        } else {
            0
        };
        self.events.push(RawEvent { kind, cpu: r.cpu, start: initial, elapsed: (r.ts - initial).max(0), routine, vector });
        Some(())
    }

    fn image(&mut self, r: &Raw) -> Option<()> {
        let mut p = Reader::new(r.data);
        let base = p.uint(r.ptr)?;
        let size = p.uint(r.ptr)?;
        // ProcessId, ImageChecksum, TimeDateStamp and a reserved field, DefaultBase, four reserved fields.
        let pid = p.u32()?;
        p.u32()?;
        let timestamp = p.u32()?;
        p.at = 3 * r.ptr + 32;
        let path = p.wstr();
        let name = base_name(&path);
        if name.is_empty() {
            return None;
        }
        let image = Image { base, size, name, path, timestamp };
        let kernel_floor = if r.ptr == 8 { 0xffff_8000_0000_0000 } else { 0x8000_0000 };
        if base >= kernel_floor {
            // Only kernel images can contain an ISR or a DPC.
            self.images.push(image);
        } else {
            self.sys.user_images.push((pid, image));
        }
        Some(())
    }

    fn thread(&mut self, r: &Raw) -> Option<()> {
        match r.opcode {
            OP_START | OP_DC_START | OP_END | OP_DC_END => {
                let (pid, tid, name) = sys::parse_thread(r.data, r.ptr)?;
                self.owner.insert(tid, pid);
                let t = r.ts as u64;
                let (start, end) = match r.opcode {
                    OP_START => (Some(t), None),
                    OP_END => (None, Some(t)),
                    _ => (None, None),
                };
                match self.sys.threads.iter_mut().rev().find(|x| x.tid == tid && x.pid == pid && x.end_ns.is_none()) {
                    Some(x) if r.opcode != OP_START => {
                        x.end_ns = x.end_ns.or(end);
                        if x.name.is_empty() {
                            x.name = name;
                        }
                    }
                    _ => self.sys.threads.push(Thread { tid, pid, name, start_ns: start, end_ns: end }),
                }
            }
            OP_THREAD_NAME => {
                let (pid, tid, name) = sys::parse_thread_name(r.data)?;
                match self.sys.threads.iter_mut().rev().find(|x| x.tid == tid) {
                    Some(x) => x.name = name,
                    None => self.sys.threads.push(Thread { tid, pid, name, ..Default::default() }),
                }
            }
            OP_CSWITCH => {
                let (new_tid, old_tid, new_prio, old_prio, old_state, wait_reason) = sys::parse_cswitch(r.data)?;
                let idx = self.sys.switches.len();
                self.sys.switches.push(Switch {
                    t: r.ts as u64,
                    cpu: r.cpu,
                    new_tid,
                    new_pid: self.pid_of(new_tid),
                    old_tid,
                    old_pid: self.pid_of(old_tid),
                    new_prio,
                    old_prio,
                    old_state,
                    wait_reason,
                    stack: NO_STACK,
                });
                self.pending_stacks.insert((r.ts, new_tid), StackOwner::Switch(idx));
            }
            OP_READY => {
                let tid = Reader::new(r.data).u32()?;
                self.sys.readies.push(Ready { t: r.ts as u64, cpu: r.cpu, tid, pid: self.pid_of(tid), by_tid: r.tid });
            }
            _ => {}
        }
        Some(())
    }

    fn process(&mut self, r: &Raw) -> Option<()> {
        if !matches!(r.opcode, OP_START | OP_END | OP_DC_START | OP_DC_END) {
            return Some(());
        }
        let p = sys::parse_process(r.data, r.version, r.ptr)?;
        let t = r.ts as u64;
        match self.sys.processes.iter_mut().rev().find(|x| x.pid == p.pid && x.end_ns.is_none()) {
            Some(x) if r.opcode != OP_START => {
                if r.opcode == OP_END {
                    x.end_ns = Some(t);
                }
                if x.name.is_empty() {
                    x.name = p.name;
                    x.command_line = p.command_line;
                }
            }
            _ => self.sys.processes.push(Process {
                pid: p.pid,
                parent: p.parent,
                name: p.name,
                command_line: p.command_line,
                start_ns: (r.opcode == OP_START).then_some(t),
                end_ns: (r.opcode == OP_END).then_some(t),
            }),
        }
        Some(())
    }

    fn intern(&mut self, frames: Vec<u64>) -> u32 {
        if let Some(id) = self.stack_ids.get(&frames) {
            return *id;
        }
        let id = self.sys.stacks.len() as u32;
        self.sys.stacks.push(frames.clone());
        self.stack_ids.insert(frames, id);
        id
    }

    fn stack(&mut self, r: &Raw) -> Option<()> {
        let (ts, tid, frames) = sys::parse_stack(r.data, r.ptr)?;
        self.sys.counts.stacks += 1;
        let Some(owner) = self.pending_stacks.get(&(ts, tid)).copied() else {
            return Some(());
        };
        let slot = match owner {
            StackOwner::Sample(i) => &mut self.sys.samples[i].stack,
            StackOwner::Switch(i) => &mut self.sys.switches[i].stack,
        };
        let current = *slot;
        // A stack that crosses into user mode arrives as a kernel part and then a user part.
        let frames = if current == NO_STACK {
            self.sys.counts.stacks_attached += 1;
            frames
        } else {
            let mut all = self.sys.stacks[current as usize].clone();
            all.extend(frames);
            all
        };
        let id = self.intern(frames);
        match owner {
            StackOwner::Sample(i) => self.sys.samples[i].stack = id,
            StackOwner::Switch(i) => self.sys.switches[i].stack = id,
        }
        // Stack events follow their event closely; forget old keys so the map stays small.
        if self.pending_stacks.len() > 200_000 {
            let horizon = r.ts - self.freq as i64;
            self.pending_stacks.retain(|k, _| k.0 >= horizon);
        }
        Some(())
    }

    fn hard_fault(&mut self, r: &Raw) -> Option<()> {
        let (initial, offset, file, tid, bytes) = sys::parse_hard_fault(r.data, r.ptr)?;
        self.hard_fault_starts.push(initial);
        self.sys.hard_faults.push(HardFault {
            t: initial.max(0) as u64,
            elapsed_ns: (r.ts - initial).max(0) as u64,
            tid,
            pid: self.pid_of(tid),
            file,
            offset,
            bytes,
        });
        Some(())
    }

    fn disk(&mut self, r: &Raw) -> Option<()> {
        let kind = match r.opcode {
            OP_DISK_READ => IoKind::Read,
            OP_DISK_WRITE => IoKind::Write,
            OP_DISK_FLUSH => IoKind::Flush,
            _ => return Some(()),
        };
        let p = sys::parse_disk(r.data, r.ptr, kind == IoKind::Flush)?;
        let tid = p.tid.unwrap_or(0);
        self.sys.disk_ios.push(DiskIo {
            t: r.ts as u64,
            elapsed_ns: p.response_ticks,
            disk: p.disk,
            kind,
            bytes: p.bytes,
            offset: p.offset,
            file: p.file,
            tid,
            pid: if tid == 0 { u32::MAX } else { self.pid_of(tid) },
        });
        Some(())
    }

    fn present(&mut self, r: &Raw, runtime: Runtime) -> Option<()> {
        let (start, stop) = match runtime {
            Runtime::Dxgi => {
                (matches!(r.id, DXGI_PRESENT_START | DXGI_MPO_START), matches!(r.id, DXGI_PRESENT_STOP | DXGI_MPO_STOP))
            }
            Runtime::D3d9 => (r.id == D3D9_PRESENT_START, r.id == D3D9_PRESENT_STOP),
        };
        if start {
            let (swap_chain, flags, sync_interval) = sys::parse_present(r.data, r.ptr, runtime)?;
            let idx = self.sys.presents.len();
            self.sys.presents.push(Present {
                t: r.ts as u64,
                end: 0,
                pid: r.pid,
                tid: r.tid,
                runtime,
                swap_chain,
                sync_interval,
                flags,
            });
            self.open_presents.insert(r.tid, idx);
        } else if stop {
            if let Some(idx) = self.open_presents.remove(&r.tid) {
                self.sys.presents[idx].end = r.ts as u64;
            }
        }
        Some(())
    }

    /// Turns the raw ticks into nanoseconds relative to the first event.
    pub fn finish(self, cpu_count: usize, duration_ticks: Option<i64>, events_lost: u32, buffers_lost: u32) -> Trace {
        let freq = self.freq as u128;
        let ns = |ticks: i64| -> u64 { (ticks.max(0) as u128 * 1_000_000_000 / freq) as u64 };
        // A call is stamped when it ends; its start (`InitialTime`) is earlier, so zero is the
        // earliest of either and no start time is negative.
        let first_start = self.events.iter().map(|e| e.start).min().unwrap_or(i64::MAX);
        let zero = if self.first_ts == i64::MAX { 0 } else { self.first_ts.min(first_start) };
        let span = if self.first_ts == i64::MAX { 0 } else { self.last_ts - self.first_ts };
        let mut events: Vec<Event> = self
            .events
            .iter()
            .map(|e| Event {
                kind: e.kind,
                cpu: e.cpu,
                start_ns: ns(e.start - zero),
                elapsed_ns: ns(e.elapsed),
                routine: e.routine,
                vector: e.vector,
            })
            .collect();
        events.sort_by_key(|e| (e.start_ns, e.cpu));

        let mut sys = self.sys;
        // Threads that only appear in the rundown at the end of the trace were unknown while their
        // events were read; give those events the owner the trace knows by now.
        let owner = &self.owner;
        let fix = |pid: &mut u32, tid: u32| {
            if *pid == u32::MAX {
                if let Some(p) = owner.get(&tid) {
                    *pid = *p;
                }
            }
        };
        for s in &mut sys.switches {
            fix(&mut s.new_pid, s.new_tid);
            fix(&mut s.old_pid, s.old_tid);
        }
        for r in &mut sys.readies {
            fix(&mut r.pid, r.tid);
        }
        for s in &mut sys.samples {
            fix(&mut s.pid, s.tid);
        }
        for h in &mut sys.hard_faults {
            fix(&mut h.pid, h.tid);
        }
        for d in &mut sys.disk_ios {
            if d.tid != 0 {
                fix(&mut d.pid, d.tid);
            }
        }
        let at = |t: u64| ns(t as i64 - zero);
        let opt = |t: Option<u64>| t.map(at);
        for p in &mut sys.processes {
            p.start_ns = opt(p.start_ns);
            p.end_ns = opt(p.end_ns);
        }
        for t in &mut sys.threads {
            t.start_ns = opt(t.start_ns);
            t.end_ns = opt(t.end_ns);
        }
        for s in &mut sys.switches {
            s.t = at(s.t);
        }
        for s in &mut sys.readies {
            s.t = at(s.t);
        }
        for s in &mut sys.samples {
            s.t = at(s.t);
        }
        for h in &mut sys.hard_faults {
            h.t = at(h.t);
            h.elapsed_ns = ns(h.elapsed_ns as i64);
        }
        for d in &mut sys.disk_ios {
            d.t = at(d.t);
            d.elapsed_ns = ns(d.elapsed_ns as i64);
        }
        for p in &mut sys.presents {
            p.t = at(p.t);
            p.end = if p.end == 0 { 0 } else { at(p.end) };
        }
        sys.switches.sort_by_key(|s| s.t);
        sys.readies.sort_by_key(|s| s.t);
        sys.samples.sort_by_key(|s| s.t);
        sys.hard_faults.sort_by_key(|s| s.t);
        sys.disk_ios.sort_by_key(|s| s.t);
        sys.presents.sort_by_key(|s| s.t);
        sys.zero_ticks = zero;
        sys.freq = self.freq;
        sys.counts.switches = sys.switches.len() as u64;
        sys.counts.readies = sys.readies.len() as u64;
        sys.counts.samples = sys.samples.len() as u64;
        sys.counts.hard_faults = sys.hard_faults.len() as u64;
        sys.counts.disk_ios = sys.disk_ios.len() as u64;
        sys.counts.presents = sys.presents.len() as u64;
        Trace {
            events,
            images: self.images,
            duration_ns: ns(duration_ticks.filter(|d| *d > 0).unwrap_or(span)),
            cpu_count,
            events_lost,
            buffers_lost,
            sys,
        }
    }
}

#[cfg(windows)]
pub use win::read;

#[cfg(not(windows))]
pub fn read(_path: &std::path::Path) -> Result<Trace, String> {
    Err("reading .etl files is only supported on Windows".to_string())
}

#[cfg(windows)]
mod win {
    use std::path::Path;
    use std::sync::Mutex;

    use windows_sys::core::GUID;
    use windows_sys::Win32::System::Diagnostics::Etw::{
        CloseTrace, OpenTraceW, ProcessTrace, EVENT_HEADER_FLAG_32_BIT_HEADER, EVENT_HEADER_FLAG_PROCESSOR_INDEX, EVENT_RECORD,
        EVENT_TRACE_LOGFILEW, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_RAW_TIMESTAMP,
    };

    use super::{Collector, Raw};
    use crate::trace::model::Trace;

    static STATE: Mutex<Option<Collector>> = Mutex::new(None);

    fn guid_u128(g: &GUID) -> u128 {
        let mut b = [0u8; 16];
        b[..4].copy_from_slice(&g.data1.to_be_bytes());
        b[4..6].copy_from_slice(&g.data2.to_be_bytes());
        b[6..8].copy_from_slice(&g.data3.to_be_bytes());
        b[8..].copy_from_slice(&g.data4);
        u128::from_be_bytes(b)
    }

    unsafe extern "system" fn on_event(ev: *mut EVENT_RECORD) {
        let ev = &*ev;
        let h = &ev.EventHeader;
        let flags = h.Flags as u32;
        let cpu = if flags & EVENT_HEADER_FLAG_PROCESSOR_INDEX != 0 {
            ev.BufferContext.Anonymous.ProcessorIndex
        } else {
            ev.BufferContext.Anonymous.Anonymous.ProcessorNumber as u16
        };
        let data: &[u8] = if ev.UserData.is_null() {
            &[]
        } else {
            std::slice::from_raw_parts(ev.UserData as *const u8, ev.UserDataLength as usize)
        };
        let raw = Raw {
            provider: guid_u128(&h.ProviderId),
            opcode: h.EventDescriptor.Opcode,
            version: h.EventDescriptor.Version,
            id: h.EventDescriptor.Id,
            ts: h.TimeStamp,
            cpu,
            pid: h.ProcessId,
            tid: h.ThreadId,
            ptr: if flags & EVENT_HEADER_FLAG_32_BIT_HEADER != 0 { 4 } else { 8 },
            data,
        };
        if let Ok(mut g) = STATE.lock() {
            if let Some(c) = g.as_mut() {
                c.record(&raw);
            }
        }
    }

    pub fn read(path: &Path) -> Result<Trace, String> {
        let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        // One read at a time: the callback has no per-call context of its own.
        static BUSY: Mutex<()> = Mutex::new(());
        let _one = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            let mut lf: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
            lf.LogFileName = wide.as_ptr() as *mut u16;
            lf.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
            lf.Anonymous2.EventRecordCallback = Some(on_event);
            let handle = OpenTraceW(&mut lf);
            if handle.Value == u64::MAX {
                return Err(format!("{}: cannot open the trace (is it a valid .etl file?)", path.display()));
            }
            let h = &lf.LogfileHeader;
            // ReservedFlags carries the clock type: 1 is the performance counter, 2 system time.
            let clock = h.ReservedFlags;
            let freq = match clock {
                2 => 10_000_000,
                3 => {
                    CloseTrace(handle);
                    return Err("this trace uses the CPU cycle counter as its clock, which is not supported".to_string());
                }
                _ => h.PerfFreq.max(1) as u64,
            };
            let cpus = h.NumberOfProcessors as usize;
            let (lost, buffers_lost) = (h.Anonymous2.Anonymous.EventsLost, h.BuffersLost);
            let duration = (h.EndTime - h.StartTime) * freq as i64 / 10_000_000;
            *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Collector::new(freq));
            let status = ProcessTrace(&handle, 1, std::ptr::null(), std::ptr::null());
            CloseTrace(handle);
            let collector = STATE.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or("trace state lost")?;
            if status != 0 {
                return Err(format!("{}: ProcessTrace failed with error {status}", path.display()));
            }
            Ok(collector.finish(cpus, Some(duration), lost, buffers_lost))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dpc_payload(initial: u64, routine: u64) -> Vec<u8> {
        let mut v = initial.to_le_bytes().to_vec();
        v.extend(routine.to_le_bytes());
        v
    }

    fn isr_payload(initial: u64, routine: u64, vector: u16) -> Vec<u8> {
        let mut v = dpc_payload(initial, routine);
        v.push(1);
        v.extend(vector.to_le_bytes());
        v.push(0);
        v
    }

    fn image_payload(base: u64, size: u64, pid: u32, name: &str) -> Vec<u8> {
        let mut v = base.to_le_bytes().to_vec();
        v.extend(size.to_le_bytes());
        v.extend(pid.to_le_bytes());
        v.extend([0u8; 4 + 8 + 16]); // checksum and timestamp, DefaultBase, reserved
        for u in name.encode_utf16().chain([0]) {
            v.extend(u.to_le_bytes());
        }
        v
    }

    fn raw<'a>(provider: u128, opcode: u8, ts: i64, cpu: u16, data: &'a [u8]) -> Raw<'a> {
        Raw { provider, opcode, version: 2, id: 0, ts, cpu, pid: 0, tid: 0, ptr: 8, data }
    }

    fn thread_payload(pid: u32, tid: u32) -> Vec<u8> {
        let mut v = pid.to_le_bytes().to_vec();
        v.extend(tid.to_le_bytes());
        v.extend([0u8; 7 * 8 + 8]);
        v
    }

    fn cswitch_payload(new: u32, old: u32, state: u8, reason: u8) -> Vec<u8> {
        let mut v = new.to_le_bytes().to_vec();
        v.extend(old.to_le_bytes());
        v.extend([8, 8, 0, 0, reason, 1, state, 0]);
        v.extend([0u8; 8]);
        v
    }

    #[test]
    fn dpc_elapsed_is_end_minus_initial_time() {
        let mut c = Collector::new(10_000_000); // 100 ns ticks
        let p = dpc_payload(1000, 0xfffff80000001000);
        c.record(&raw(PERFINFO, OP_DPC, 1050, 3, &p));
        let t = c.finish(4, None, 0, 0);
        assert_eq!(t.events.len(), 1);
        let e = t.events[0];
        assert_eq!((e.kind, e.cpu, e.elapsed_ns, e.routine), (Kind::Dpc, 3, 5000, 0xfffff80000001000));
        assert_eq!(e.start_ns, 0);
    }

    #[test]
    fn isr_carries_its_vector_and_every_opcode_maps_to_a_kind() {
        let mut c = Collector::new(1_000_000_000);
        let isr = isr_payload(100, 0xfffff80000002000, 0x51);
        c.record(&raw(PERFINFO, OP_ISR, 130, 0, &isr));
        for op in [OP_DPC, OP_TIMER_DPC, OP_THREAD_DPC] {
            let p = dpc_payload(200, 0xfffff80000003000);
            c.record(&raw(PERFINFO, op, 210, 1, &p));
        }
        let t = c.finish(2, None, 0, 0);
        assert_eq!(t.events.len(), 4);
        let isr = t.events.iter().find(|e| e.kind == Kind::Isr).unwrap();
        assert_eq!((isr.vector, isr.elapsed_ns), (0x51, 30));
        let kinds: Vec<Kind> = t.events.iter().map(|e| e.kind).collect();
        for k in [Kind::Dpc, Kind::TimerDpc, Kind::ThreadDpc] {
            assert!(kinds.contains(&k));
        }
    }

    #[test]
    fn message_signalled_interrupts_are_isrs_too() {
        // Opcode 50 has the same fields as opcode 67 followed by a 32-bit message number.
        let mut c = Collector::new(1_000_000_000);
        let mut p = isr_payload(100, 0xfffff80000002000, 0xb1);
        p.extend(2u32.to_le_bytes());
        assert_eq!(p.len(), 24);
        c.record(&raw(PERFINFO, OP_ISR_MSI, 107, 4, &p));
        let e = c.finish(5, None, 0, 0).events[0];
        assert_eq!((e.kind, e.vector, e.elapsed_ns, e.cpu), (Kind::Isr, 0xb1, 7, 4));
    }

    #[test]
    fn a_clock_that_ran_backwards_gives_zero_elapsed() {
        let mut c = Collector::new(10_000_000);
        let p = dpc_payload(2000, 0xfffff80000001000);
        c.record(&raw(PERFINFO, OP_DPC, 1990, 0, &p));
        assert_eq!(c.finish(1, None, 0, 0).events[0].elapsed_ns, 0);
    }

    #[test]
    fn kernel_images_go_to_the_driver_map_and_user_images_to_their_process() {
        let mut c = Collector::new(10_000_000);
        let k = image_payload(0xfffff80000000000, 0x1000, 0, r"\SystemRoot\System32\drivers\ndis.sys");
        let u = image_payload(0x7ff600000000, 0x1000, 77, r"C:\Users\someone\app.exe");
        c.record(&raw(IMAGE, OP_IMAGE_DC_END, 5, 0, &k));
        c.record(&raw(IMAGE, OP_IMAGE_DC_END, 5, 0, &u));
        let t = c.finish(1, None, 0, 0);
        assert_eq!(t.images.len(), 1);
        assert_eq!(t.images[0].name, "ndis.sys");
        assert_eq!((t.images[0].base, t.images[0].size), (0xfffff80000000000, 0x1000));
        assert_eq!(t.sys.user_images.len(), 1);
        assert_eq!((t.sys.user_images[0].0, t.sys.user_images[0].1.name.as_str()), (77, "app.exe"));
    }

    #[test]
    fn truncated_payloads_are_counted_not_trusted() {
        let mut c = Collector::new(10_000_000);
        c.record(&raw(PERFINFO, OP_DPC, 10, 0, &[1, 2, 3]));
        c.record(&raw(PERFINFO, OP_ISR, 10, 0, &dpc_payload(1, 2))); // no vector bytes
        c.record(&raw(IMAGE, OP_IMAGE_LOAD, 10, 0, &[0xff; 5]));
        c.record(&raw(sys::THREAD, OP_CSWITCH, 10, 0, &[0; 6]));
        c.record(&raw(PERFINFO, OP_SAMPLE, 10, 0, &[0; 6]));
        assert_eq!(c.malformed, 5);
        let t = c.finish(1, None, 0, 0);
        assert!(t.events.is_empty() && t.sys.switches.is_empty() && t.sys.samples.is_empty());
    }

    #[test]
    fn unrelated_events_only_move_the_time_window() {
        let mut c = Collector::new(10_000_000);
        c.record(&raw(0x1234, 1, 100, 0, &[]));
        c.record(&raw(0x1234, 1, 600, 0, &[]));
        let t = c.finish(1, None, 0, 0);
        assert_eq!(t.duration_ns, 50_000);
        assert!(t.events.is_empty());
    }

    #[test]
    fn events_are_sorted_and_zeroed_at_the_first_event() {
        let mut c = Collector::new(1_000_000_000);
        let late = dpc_payload(900, 1);
        let early = dpc_payload(500, 1);
        c.record(&raw(PERFINFO, OP_DPC, 910, 0, &late));
        c.record(&raw(PERFINFO, OP_DPC, 520, 1, &early));
        let t = c.finish(2, None, 0, 0);
        assert_eq!(t.events[0].start_ns, 0);
        assert_eq!(t.events[1].start_ns, 400);
    }

    #[test]
    fn thirty_two_bit_payloads_use_four_byte_pointers() {
        let mut c = Collector::new(1_000_000_000);
        let mut p = 100u64.to_le_bytes().to_vec();
        p.extend(0x8000_1000u32.to_le_bytes());
        c.record(&Raw {
            provider: PERFINFO,
            opcode: OP_DPC,
            version: 2,
            id: 0,
            ts: 150,
            cpu: 0,
            pid: 0,
            tid: 0,
            ptr: 4,
            data: &p,
        });
        let e = c.finish(1, None, 0, 0).events[0];
        assert_eq!((e.routine, e.elapsed_ns), (0x8000_1000, 50));
    }

    #[test]
    fn switches_and_samples_know_their_process_and_get_their_stacks() {
        let mut c = Collector::new(1_000_000_000);
        c.record(&raw(sys::THREAD, OP_DC_START, 10, 0, &thread_payload(500, 501)));
        c.record(&raw(sys::THREAD, OP_START, 20, 0, &thread_payload(600, 601)));
        let sw = cswitch_payload(501, 601, sys::STATE_WAITING, 13);
        c.record(&raw(sys::THREAD, OP_CSWITCH, 100, 2, &sw));
        let mut sample = 0x7ff6_0000_1234u64.to_le_bytes().to_vec();
        sample.extend(501u32.to_le_bytes());
        sample.extend([1, 0, 0, 0]);
        c.record(&raw(PERFINFO, OP_SAMPLE, 150, 2, &sample));
        // The sample's stack comes as a kernel part and a user part.
        for frames in [vec![0xffff_f800_0000_1000u64], vec![0x7ff6_0000_1234, 0x7ff6_0000_2000]] {
            let mut s = 150u64.to_le_bytes().to_vec();
            s.extend(500u32.to_le_bytes());
            s.extend(501u32.to_le_bytes());
            for f in frames {
                s.extend(f.to_le_bytes());
            }
            c.record(&raw(sys::STACK_WALK, OP_STACK, 151, 2, &s));
        }
        // A stack for an event that is not ours is ignored.
        let mut other = 999u64.to_le_bytes().to_vec();
        other.extend([0u8; 8]);
        other.extend(1u64.to_le_bytes());
        c.record(&raw(sys::STACK_WALK, OP_STACK, 152, 2, &other));
        let t = c.finish(4, None, 0, 0);
        let s = t.sys.switches[0];
        assert_eq!(
            (s.new_tid, s.new_pid, s.old_tid, s.old_pid, s.old_state, s.wait_reason, s.cpu),
            (501, 500, 601, 600, 5, 13, 2)
        );
        assert_eq!(s.t, 90);
        let smp = t.sys.samples[0];
        assert_eq!((smp.pid, smp.tid, smp.ip), (500, 501, 0x7ff6_0000_1234));
        assert_eq!(t.sys.stacks[smp.stack as usize], vec![0xffff_f800_0000_1000, 0x7ff6_0000_1234, 0x7ff6_0000_2000]);
        assert_eq!(t.sys.counts.stacks, 3);
        assert_eq!(t.sys.counts.stacks_attached, 1);
        assert_eq!(s.stack, NO_STACK);
        assert_eq!(t.sys.threads.len(), 2);
        assert_eq!(t.sys.threads[1].start_ns, Some(10));
    }

    #[test]
    fn threads_known_only_from_the_end_rundown_still_get_their_process() {
        let mut c = Collector::new(1_000_000_000);
        let sw = cswitch_payload(0, 700, sys::STATE_WAITING, 6);
        c.record(&raw(sys::THREAD, OP_CSWITCH, 100, 0, &sw));
        c.record(&raw(sys::THREAD, OP_DC_END, 900, 0, &thread_payload(70, 700)));
        let t = c.finish(1, None, 0, 0);
        assert_eq!((t.sys.switches[0].old_pid, t.sys.switches[0].new_pid), (70, 0));
    }

    #[test]
    fn a_switch_gets_the_stack_of_the_thread_it_switches_in() {
        let mut c = Collector::new(1_000_000_000);
        let sw = cswitch_payload(501, 0, 2, 0);
        c.record(&raw(sys::THREAD, OP_CSWITCH, 100, 0, &sw));
        let mut s = 100u64.to_le_bytes().to_vec();
        s.extend(500u32.to_le_bytes());
        s.extend(501u32.to_le_bytes());
        s.extend(0x7ff6_0000_4000u64.to_le_bytes());
        c.record(&raw(sys::STACK_WALK, OP_STACK, 101, 0, &s));
        let t = c.finish(1, None, 0, 0);
        let st = t.sys.switches[0].stack;
        assert_ne!(st, NO_STACK);
        assert_eq!(t.sys.stacks[st as usize], vec![0x7ff6_0000_4000]);
    }

    #[test]
    fn presents_pair_start_and_stop_on_the_same_thread() {
        let mut c = Collector::new(1_000_000_000);
        let mut start = 0xabcu64.to_le_bytes().to_vec();
        start.extend(0u32.to_le_bytes());
        start.extend(1u32.to_le_bytes());
        let stop = 0xabcu64.to_le_bytes().to_vec();
        fn ev(id: u16, ts: i64, tid: u32, data: &[u8]) -> Raw<'_> {
            Raw { provider: sys::DXGI, opcode: 0, version: 0, id, ts, cpu: 0, pid: 42, tid, ptr: 8, data }
        }
        // Thread 7 presents twice; thread 8 starts one that never returns.
        for (id, ts, tid) in [(42, 100, 7), (43, 130, 7), (42, 140, 8), (42, 200, 7), (43, 250, 7)] {
            let data = if id == 42 { &start } else { &stop };
            c.record(&ev(id, ts, tid, data));
        }
        let t = c.finish(1, None, 0, 0);
        let p: Vec<(u64, u64, u32)> = t.sys.presents.iter().map(|p| (p.t, p.end, p.tid)).collect();
        assert_eq!(p, vec![(0, 30, 7), (40, 0, 8), (100, 150, 7)]);
        assert_eq!(t.sys.presents[0].sync_interval, 1);
        assert_eq!(t.sys.presents[0].pid, 42);
    }

    #[test]
    fn hard_faults_start_at_their_initial_time() {
        let mut c = Collector::new(1_000_000_000);
        c.record(&raw(sys::THREAD, OP_DC_START, 1, 0, &thread_payload(9, 90)));
        let mut h = 50u64.to_le_bytes().to_vec();
        h.extend(0u64.to_le_bytes());
        h.extend(0u64.to_le_bytes());
        h.extend(0xf11eu64.to_le_bytes());
        h.extend(90u32.to_le_bytes());
        h.extend(4096u32.to_le_bytes());
        c.record(&raw(sys::PAGE_FAULT, OP_HARD_FAULT, 80, 0, &h));
        let t = c.finish(1, None, 0, 0);
        let f = t.sys.hard_faults[0];
        assert_eq!((f.t, f.elapsed_ns, f.pid, f.bytes), (49, 30, 9, 4096));
    }
}
