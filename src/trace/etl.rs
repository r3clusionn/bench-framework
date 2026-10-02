//! Reads the DPC, ISR and image-load events of a kernel trace (`.etl`) with the Windows trace
//! consumer API. Timestamps are taken raw and converted here, so ISR and DPC durations keep the
//! full resolution of the performance counter instead of the whole microseconds that xperf prints.

use super::model::{base_name, Event, Image, Kind, Trace};

/// PerfInfo event group: ISR, DPC, timer DPC and thread DPC.
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

/// One event as the consumer hands it over.
pub struct Raw<'a> {
    pub provider: u128,
    pub opcode: u8,
    pub version: u8,
    /// Raw timestamp in clock ticks.
    pub ts: i64,
    pub cpu: u16,
    /// Pointer size of the traced machine, 4 or 8.
    pub ptr: usize,
    pub data: &'a [u8],
}

/// Bounds-checked little-endian reader over a payload.
struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn uint(&mut self, n: usize) -> Option<u64> {
        let b = self.take(n)?;
        let mut v = [0u8; 8];
        v[..n].copy_from_slice(b);
        Some(u64::from_le_bytes(v))
    }

    fn wstr(&mut self) -> String {
        let rest = &self.d[self.at.min(self.d.len())..];
        let units: Vec<u16> =
            rest.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
        String::from_utf16_lossy(&units)
    }
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

/// Accumulates events while a trace is being read.
pub struct Collector {
    /// Clock ticks per second.
    freq: u64,
    events: Vec<RawEvent>,
    images: Vec<Image>,
    first_ts: i64,
    last_ts: i64,
    pub malformed: u64,
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
        }
    }

    pub fn record(&mut self, r: &Raw) {
        self.first_ts = self.first_ts.min(r.ts);
        self.last_ts = self.last_ts.max(r.ts);
        if r.provider == PERFINFO {
            let kind = match r.opcode {
                OP_ISR | OP_ISR_MSI => Kind::Isr,
                OP_DPC => Kind::Dpc,
                OP_TIMER_DPC => Kind::TimerDpc,
                OP_THREAD_DPC => Kind::ThreadDpc,
                _ => return,
            };
            if self.perf_event(kind, r).is_none() {
                self.malformed += 1;
            }
        } else if r.provider == IMAGE
            && matches!(r.opcode, OP_IMAGE_LOAD | OP_IMAGE_DC_START | OP_IMAGE_DC_END)
            && self.image(r).is_none()
        {
            self.malformed += 1;
        }
    }

    fn perf_event(&mut self, kind: Kind, r: &Raw) -> Option<()> {
        let mut p = Reader { d: r.data, at: 0 };
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
        let mut p = Reader { d: r.data, at: 0 };
        let base = p.uint(r.ptr)?;
        let size = p.uint(r.ptr)?;
        // Only kernel images can contain an ISR or a DPC.
        let kernel_floor = if r.ptr == 8 { 0xffff_8000_0000_0000 } else { 0x8000_0000 };
        if base < kernel_floor {
            return Some(());
        }
        // ProcessId, ImageChecksum, TimeDateStamp and a reserved field, DefaultBase, four reserved fields.
        p.uint(4)?;
        p.uint(4)?;
        let timestamp = p.uint(4)? as u32;
        p.at = 3 * r.ptr + 32;
        let path = p.wstr();
        let name = base_name(&path);
        if name.is_empty() {
            return None;
        }
        self.images.push(Image { base, size, name, path, timestamp });
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
        Trace {
            events,
            images: self.images,
            duration_ns: ns(duration_ticks.filter(|d| *d > 0).unwrap_or(span)),
            cpu_count,
            events_lost,
            buffers_lost,
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
            ts: h.TimeStamp,
            cpu,
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

    fn image_payload(base: u64, size: u64, name: &str) -> Vec<u8> {
        let mut v = base.to_le_bytes().to_vec();
        v.extend(size.to_le_bytes());
        v.extend([0u8; 8 + 8 + 16]); // ids and checksums, DefaultBase, reserved
        for u in name.encode_utf16().chain([0]) {
            v.extend(u.to_le_bytes());
        }
        v
    }

    fn raw<'a>(provider: u128, opcode: u8, ts: i64, cpu: u16, data: &'a [u8]) -> Raw<'a> {
        Raw { provider, opcode, version: 2, ts, cpu, ptr: 8, data }
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
    fn kernel_images_are_kept_and_user_images_are_not() {
        let mut c = Collector::new(10_000_000);
        let k = image_payload(0xfffff80000000000, 0x1000, r"\SystemRoot\System32\drivers\ndis.sys");
        let u = image_payload(0x7ff600000000, 0x1000, r"C:\Users\someone\app.exe");
        c.record(&raw(IMAGE, OP_IMAGE_DC_END, 5, 0, &k));
        c.record(&raw(IMAGE, OP_IMAGE_DC_END, 5, 0, &u));
        let t = c.finish(1, None, 0, 0);
        assert_eq!(t.images.len(), 1);
        assert_eq!(t.images[0].name, "ndis.sys");
        assert_eq!((t.images[0].base, t.images[0].size), (0xfffff80000000000, 0x1000));
    }

    #[test]
    fn truncated_payloads_are_counted_not_trusted() {
        let mut c = Collector::new(10_000_000);
        c.record(&raw(PERFINFO, OP_DPC, 10, 0, &[1, 2, 3]));
        c.record(&raw(PERFINFO, OP_ISR, 10, 0, &dpc_payload(1, 2))); // no vector bytes
        c.record(&raw(IMAGE, OP_IMAGE_LOAD, 10, 0, &[0xff; 5]));
        assert_eq!(c.malformed, 3);
        assert!(c.finish(1, None, 0, 0).events.is_empty());
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
        c.record(&Raw { provider: PERFINFO, opcode: OP_DPC, version: 2, ts: 150, cpu: 0, ptr: 4, data: &p });
        let e = c.finish(1, None, 0, 0).events[0];
        assert_eq!((e.routine, e.elapsed_ns), (0x8000_1000, 50));
    }
}
