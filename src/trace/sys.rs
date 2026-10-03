//! System events of a trace beyond DPCs and ISRs: processes and threads, context switches, ready
//! events, CPU samples with their stacks, hard faults, disk I/O, file names, user-mode images and
//! frame presents (DXGI and Direct3D 9). These are what WPA's CPU Usage (Precise and Sampled),
//! Ready, Hard Faults, Disk Usage and Generic Events tables are built from.
//!
//! The payload layouts are those of the kernel's MOF classes (`Thread_V2_TypeGroup1`, `CSwitch`,
//! `ReadyThread`, `SampledProfile`, `StackWalk_Event`, `PageFault_HardFault`, `DiskIo_TypeGroup1`,
//! `FileIo_Name`, `Process_V4_TypeGroup1`, `Image_Load`) and of the manifest providers; the parser
//! is checked against `xperf -a dumper` by the live tests.

use std::collections::HashMap;

use super::model::Image;

pub const PROCESS: u128 = 0x3d6fa8d0_fe05_11d0_9dda_00c04fd7ba7c;
pub const THREAD: u128 = 0x3d6fa8d1_fe05_11d0_9dda_00c04fd7ba7c;
pub const PAGE_FAULT: u128 = 0x3d6fa8d3_fe05_11d0_9dda_00c04fd7ba7c;
pub const DISK_IO: u128 = 0x3d6fa8d4_fe05_11d0_9dda_00c04fd7ba7c;
pub const FILE_IO: u128 = 0x90cbdc39_4a3e_11d1_84f4_0000f80464e3;
pub const STACK_WALK: u128 = 0xdef2fe46_7bd6_4b80_bd94_f57fe20d0ce3;
/// Microsoft-Windows-DXGI.
pub const DXGI: u128 = 0xca11c036_0102_4a2d_a6ad_f03cfed5d3c9;
/// Microsoft-Windows-D3D9.
pub const D3D9: u128 = 0x783aca0a_790e_4d7f_8451_aa850511c6b9;

/// No stack was recorded for an event.
pub const NO_STACK: u32 = u32::MAX;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub command_line: String,
    /// Nanoseconds since the start of the trace; `None` when it started before the trace.
    pub start_ns: Option<u64>,
    pub end_ns: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thread {
    pub tid: u32,
    pub pid: u32,
    pub name: String,
    pub start_ns: Option<u64>,
    pub end_ns: Option<u64>,
}

/// One context switch: `old` stops running on `cpu` and `new` starts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Switch {
    pub t: u64,
    pub cpu: u16,
    pub new_tid: u32,
    pub new_pid: u32,
    pub old_tid: u32,
    pub old_pid: u32,
    pub new_prio: i8,
    pub old_prio: i8,
    /// `KTHREAD_STATE` of the old thread: 1 ready (preempted), 5 waiting, ...
    pub old_state: u8,
    /// `KWAIT_REASON` of the old thread when it is waiting.
    pub wait_reason: u8,
    /// Stack of the new thread, i.e. where it had been waiting (index into `stacks`).
    pub stack: u32,
}

/// A thread became ready to run (its wait was satisfied).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ready {
    pub t: u64,
    pub cpu: u16,
    pub tid: u32,
    pub pid: u32,
    /// The thread that readied it, when the trace says.
    pub by_tid: u32,
}

/// A CPU sample (`PROFILE`): the instruction pointer of whatever ran on `cpu` at `t`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub t: u64,
    pub cpu: u16,
    pub tid: u32,
    pub pid: u32,
    pub ip: u64,
    pub stack: u32,
}

/// A page fault that had to read from disk.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HardFault {
    /// Start of the fault.
    pub t: u64,
    pub elapsed_ns: u64,
    pub tid: u32,
    pub pid: u32,
    pub file: u64,
    pub offset: u64,
    pub bytes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum IoKind {
    #[default]
    Read,
    Write,
    Flush,
}

impl IoKind {
    pub fn label(self) -> &'static str {
        match self {
            IoKind::Read => "read",
            IoKind::Write => "write",
            IoKind::Flush => "flush",
        }
    }
}

/// A completed disk request.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DiskIo {
    /// Completion time.
    pub t: u64,
    /// Time the disk took to serve it.
    pub elapsed_ns: u64,
    pub disk: u32,
    pub kind: IoKind,
    pub bytes: u32,
    pub offset: u64,
    pub file: u64,
    pub tid: u32,
    pub pid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Runtime {
    #[default]
    Dxgi,
    D3d9,
}

impl Runtime {
    pub fn label(self) -> &'static str {
        match self {
            Runtime::Dxgi => "DXGI",
            Runtime::D3d9 => "D3D9",
        }
    }
}

/// One call to `Present` by an application.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Present {
    /// When the call started.
    pub t: u64,
    /// When it returned (0 if the trace ended first).
    pub end: u64,
    pub pid: u32,
    pub tid: u32,
    pub runtime: Runtime,
    pub swap_chain: u64,
    pub sync_interval: i32,
    pub flags: u32,
}

/// How many events of each type were read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    pub switches: u64,
    pub readies: u64,
    pub samples: u64,
    pub stacks: u64,
    pub stacks_attached: u64,
    pub hard_faults: u64,
    pub disk_ios: u64,
    pub presents: u64,
}

/// Every system event of a trace. Times are nanoseconds since the start of the trace.
#[derive(Clone, Debug, Default)]
pub struct SysEvents {
    pub processes: Vec<Process>,
    pub threads: Vec<Thread>,
    pub switches: Vec<Switch>,
    pub readies: Vec<Ready>,
    pub samples: Vec<Sample>,
    pub hard_faults: Vec<HardFault>,
    pub disk_ios: Vec<DiskIo>,
    pub presents: Vec<Present>,
    /// File object to file name.
    pub file_names: HashMap<u64, String>,
    /// Distinct stacks, innermost frame first.
    pub stacks: Vec<Vec<u64>>,
    /// User-mode images per process id (kernel images stay in `Trace::images`).
    pub user_images: Vec<(u32, Image)>,
    /// Interval of the CPU sampling timer, when the trace records it.
    pub sample_interval_ns: Option<u64>,
    /// Raw clock value of time zero and clock ticks per second, to place external timestamps
    /// (PresentMon's `--qpc_time`) on the trace's time line.
    pub zero_ticks: i64,
    pub freq: u64,
    pub counts: Counts,
}

impl SysEvents {
    pub fn process_name(&self, pid: u32) -> String {
        if pid == 0 {
            return "Idle".to_string();
        }
        // The latest process with this id (ids are reused).
        match self.processes.iter().rev().find(|p| p.pid == pid) {
            Some(p) if !p.name.is_empty() => p.name.clone(),
            _ => format!("pid {pid}"),
        }
    }

    pub fn thread_name(&self, tid: u32) -> Option<&str> {
        self.threads.iter().rev().find(|t| t.tid == tid && !t.name.is_empty()).map(|t| t.name.as_str())
    }

    /// Converts a raw clock value (e.g. a QPC timestamp from PresentMon) into trace time.
    pub fn ticks_to_ns(&self, ticks: i64) -> Option<u64> {
        if self.freq == 0 || ticks < self.zero_ticks {
            return None;
        }
        Some(((ticks - self.zero_ticks) as u128 * 1_000_000_000 / self.freq as u128) as u64)
    }
}

/// `KWAIT_REASON` names, as WPA shows them.
pub fn wait_reason(r: u8) -> &'static str {
    const NAMES: [&str; 43] = [
        "Executive",
        "FreePage",
        "PageIn",
        "PoolAllocation",
        "DelayExecution",
        "Suspended",
        "UserRequest",
        "WrExecutive",
        "WrFreePage",
        "WrPageIn",
        "WrPoolAllocation",
        "WrDelayExecution",
        "WrSuspended",
        "WrUserRequest",
        "WrSpare0",
        "WrQueue",
        "WrLpcReceive",
        "WrLpcReply",
        "WrVirtualMemory",
        "WrPageOut",
        "WrRendezvous",
        "WrKeyedEvent",
        "WrTerminated",
        "WrProcessInSwap",
        "WrCpuRateControl",
        "WrCalloutStack",
        "WrKernel",
        "WrResource",
        "WrPushLock",
        "WrMutex",
        "WrQuantumEnd",
        "WrDispatchInt",
        "WrPreempted",
        "WrYieldExecution",
        "WrFastMutex",
        "WrGuardedMutex",
        "WrRundown",
        "WrAlertByThreadId",
        "WrDeferredPreempt",
        "WrPhysicalFault",
        "WrIoRing",
        "WrMdlCache",
        "WrRcu",
    ];
    NAMES.get(r as usize).copied().unwrap_or("Unknown")
}

/// `KTHREAD_STATE` names.
pub fn thread_state(s: u8) -> &'static str {
    const NAMES: [&str; 10] = [
        "Initialized",
        "Ready",
        "Running",
        "Standby",
        "Terminated",
        "Waiting",
        "Transition",
        "DeferredReady",
        "GateWait",
        "WaitingForProcessOutSwap",
    ];
    NAMES.get(s as usize).copied().unwrap_or("Unknown")
}

pub const STATE_READY: u8 = 1;
pub const STATE_WAITING: u8 = 5;

/// What the kernel's process event says, before the times are converted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessPayload {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub command_line: String,
}

/// Bounds-checked little-endian reader over a payload.
pub struct Reader<'a> {
    pub d: &'a [u8],
    pub at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(d: &'a [u8]) -> Reader<'a> {
        Reader { d, at: 0 }
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    pub fn uint(&mut self, n: usize) -> Option<u64> {
        let b = self.take(n)?;
        let mut v = [0u8; 8];
        v[..n].copy_from_slice(b);
        Some(u64::from_le_bytes(v))
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.uint(4).map(|v| v as u32)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.uint(1).map(|v| v as u8)
    }

    pub fn remaining(&self) -> usize {
        self.d.len().saturating_sub(self.at)
    }

    /// A NUL-terminated UTF-16 string; consumes the terminator.
    pub fn wstr(&mut self) -> String {
        let rest = &self.d[self.at.min(self.d.len())..];
        let units: Vec<u16> =
            rest.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
        self.at = (self.at + units.len() * 2 + 2).min(self.d.len());
        String::from_utf16_lossy(&units)
    }

    /// A NUL-terminated 8-bit string; consumes the terminator.
    pub fn astr(&mut self) -> String {
        let rest = &self.d[self.at.min(self.d.len())..];
        let n = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
        self.at = (self.at + n + 1).min(self.d.len());
        String::from_utf8_lossy(&rest[..n]).into_owned()
    }

    /// Skips a `SID` field as the kernel logs it: 4 zero bytes for no SID, otherwise a
    /// `TOKEN_USER` (two pointers) followed by the SID.
    pub fn skip_sid(&mut self, ptr: usize) -> Option<()> {
        let first = u32::from_le_bytes(self.d.get(self.at..self.at + 4)?.try_into().ok()?);
        if first == 0 {
            self.at += 4;
            return Some(());
        }
        let token = 2 * ptr;
        let sub_authorities = *self.d.get(self.at + token + 1)? as usize;
        let n = token + 8 + 4 * sub_authorities;
        self.take(n).map(|_| ())
    }
}

/// `Process_V3/V4_TypeGroup1` (start, end and rundown).
pub fn parse_process(data: &[u8], version: u8, ptr: usize) -> Option<ProcessPayload> {
    let mut r = Reader::new(data);
    r.uint(ptr)?; // UniqueProcessKey
    let pid = r.u32()?;
    let parent = r.u32()?;
    r.u32()?; // SessionId
    r.u32()?; // ExitStatus
    r.uint(ptr)?; // DirectoryTableBase
    if version >= 4 {
        r.u32()?; // Flags
    }
    r.skip_sid(ptr)?;
    let name = r.astr();
    let command_line = r.wstr();
    Some(ProcessPayload { pid, parent, name, command_line })
}

/// `Thread_TypeGroup1` (start, end and rundown): process and thread id, and the thread name that
/// version 4 and later append.
pub fn parse_thread(data: &[u8], ptr: usize) -> Option<(u32, u32, String)> {
    let mut r = Reader::new(data);
    let pid = r.u32()?;
    let tid = r.u32()?;
    // StackBase, StackLimit, UserStackBase, UserStackLimit, Affinity, Win32StartAddr, TebBase,
    // SubProcessTag, then four bytes of priorities and flags.
    let fixed = 8 + 7 * ptr + 4 + 4;
    let name = if data.len() > fixed + 1 {
        r.at = fixed;
        r.wstr()
    } else {
        String::new()
    };
    Some((pid, tid, name))
}

/// `Thread_SetName`: process id, thread id, name.
pub fn parse_thread_name(data: &[u8]) -> Option<(u32, u32, String)> {
    let mut r = Reader::new(data);
    let pid = r.u32()?;
    let tid = r.u32()?;
    Some((pid, tid, r.wstr()))
}

/// `CSwitch`: new and old thread, priorities, old thread's state and wait reason.
pub fn parse_cswitch(data: &[u8]) -> Option<(u32, u32, i8, i8, u8, u8)> {
    let mut r = Reader::new(data);
    let new_tid = r.u32()?;
    let old_tid = r.u32()?;
    let new_prio = r.u8()? as i8;
    let old_prio = r.u8()? as i8;
    r.u8()?; // PreviousCState
    r.u8()?; // SpareByte
    let wait_reason = r.u8()?;
    r.u8()?; // OldThreadWaitMode
    let old_state = r.u8()?;
    Some((new_tid, old_tid, new_prio, old_prio, old_state, wait_reason))
}

/// `SampledProfile`: instruction pointer and thread.
pub fn parse_sample(data: &[u8], ptr: usize) -> Option<(u64, u32)> {
    let mut r = Reader::new(data);
    let ip = r.uint(ptr)?;
    let tid = r.u32()?;
    Some((ip, tid))
}

/// `StackWalk_Event`: the timestamp and thread of the event the stack belongs to, and the frames.
pub fn parse_stack(data: &[u8], ptr: usize) -> Option<(i64, u32, Vec<u64>)> {
    let mut r = Reader::new(data);
    let ts = r.uint(8)? as i64;
    r.u32()?; // StackProcess
    let tid = r.u32()?;
    let mut frames = Vec::with_capacity(r.remaining() / ptr);
    while r.remaining() >= ptr {
        frames.push(r.uint(ptr)?);
    }
    Some((ts, tid, frames))
}

/// `PageFault_HardFault`: start time, file offset, file object, thread, bytes.
pub fn parse_hard_fault(data: &[u8], ptr: usize) -> Option<(i64, u64, u64, u32, u32)> {
    let mut r = Reader::new(data);
    let initial = r.uint(8)? as i64;
    let offset = r.uint(8)?;
    r.uint(ptr)?; // VirtualAddress
    let file = r.uint(ptr)?;
    let tid = r.u32()?;
    let bytes = r.u32()?;
    Some((initial, offset, file, tid, bytes))
}

/// `DiskIo_TypeGroup1` (read and write) or `DiskIo_TypeGroup3` (flush).
pub struct DiskPayload {
    pub disk: u32,
    pub bytes: u32,
    pub offset: u64,
    pub file: u64,
    pub response_ticks: u64,
    pub tid: Option<u32>,
}

pub fn parse_disk(data: &[u8], ptr: usize, flush: bool) -> Option<DiskPayload> {
    let mut r = Reader::new(data);
    let disk = r.u32()?;
    r.u32()?; // IrpFlags
    if flush {
        let response_ticks = r.uint(8)?;
        r.uint(ptr)?; // Irp
        let tid = r.u32();
        return Some(DiskPayload { disk, bytes: 0, offset: 0, file: 0, response_ticks, tid });
    }
    let bytes = r.u32()?;
    r.u32()?; // Reserved
    let offset = r.uint(8)?;
    let file = r.uint(ptr)?;
    r.uint(ptr)?; // Irp
    let response_ticks = r.uint(8)?;
    let tid = r.u32();
    Some(DiskPayload { disk, bytes, offset, file, response_ticks, tid })
}

/// `FileIo_Name`: file object and name.
pub fn parse_file_name(data: &[u8], ptr: usize) -> Option<(u64, String)> {
    let mut r = Reader::new(data);
    let file = r.uint(ptr)?;
    Some((file, r.wstr()))
}

/// DXGI `Present_Start` (swap chain, flags, sync interval) or D3D9 `Present_Start` (swap chain,
/// flags).
pub fn parse_present(data: &[u8], ptr: usize, runtime: Runtime) -> Option<(u64, u32, i32)> {
    let mut r = Reader::new(data);
    let swap_chain = r.uint(ptr)?;
    let flags = r.u32()?;
    let sync = match runtime {
        Runtime::Dxgi => r.u32()? as i32,
        Runtime::D3d9 => -1,
    };
    Some((swap_chain, flags, sync))
}

/// `PerfInfo` sample-interval events: (source, new interval in 100 ns units).
pub fn parse_interval(data: &[u8]) -> Option<(u32, u32)> {
    let mut r = Reader::new(data);
    let source = r.u32()?;
    let new = r.u32()?;
    Some((source, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn process_payload_with_and_without_a_sid() {
        // Version 4 with a SID of one sub-authority (S-1-5-18).
        let mut p = Vec::new();
        p.extend(0xffffu64.to_le_bytes()); // UniqueProcessKey
        p.extend(1234u32.to_le_bytes());
        p.extend(4u32.to_le_bytes());
        p.extend(1u32.to_le_bytes());
        p.extend(0u32.to_le_bytes());
        p.extend(0u64.to_le_bytes()); // DirectoryTableBase
        p.extend(0u32.to_le_bytes()); // Flags
        p.extend(0xdeadu64.to_le_bytes()); // TOKEN_USER: SID pointer
        p.extend(0u64.to_le_bytes()); //             attributes
        p.extend([1, 1, 0, 0, 0, 0, 0, 5]); // revision 1, 1 sub-authority, authority 5
        p.extend(18u32.to_le_bytes());
        p.extend(b"game.exe\0");
        p.extend(wide("game.exe -dx12"));
        let got = parse_process(&p, 4, 8).unwrap();
        assert_eq!(got, ProcessPayload { pid: 1234, parent: 4, name: "game.exe".into(), command_line: "game.exe -dx12".into() });

        // Version 3 (no Flags) with no SID.
        let mut q = Vec::new();
        q.extend(0u64.to_le_bytes());
        q.extend(8u32.to_le_bytes());
        q.extend(0u32.to_le_bytes());
        q.extend(0u32.to_le_bytes());
        q.extend(0u32.to_le_bytes());
        q.extend(0u64.to_le_bytes());
        q.extend(0u32.to_le_bytes()); // no SID
        q.extend(b"x.exe\0");
        q.extend(wide(""));
        assert_eq!(parse_process(&q, 3, 8).unwrap().name, "x.exe");
        assert!(parse_process(&q[..10], 3, 8).is_none());
    }

    #[test]
    fn thread_payload_reads_ids_and_an_optional_name() {
        let mut t = Vec::new();
        t.extend(10u32.to_le_bytes());
        t.extend(20u32.to_le_bytes());
        t.extend([0u8; 7 * 8 + 8]);
        assert_eq!(parse_thread(&t, 8).unwrap(), (10, 20, String::new()));
        t.extend(wide("RenderThread"));
        assert_eq!(parse_thread(&t, 8).unwrap(), (10, 20, "RenderThread".to_string()));
        assert!(parse_thread(&t[..6], 8).is_none());
    }

    #[test]
    fn cswitch_fields_are_in_kernel_order() {
        let p = [
            0x10, 0, 0, 0, // new
            0x20, 0, 0, 0, // old
            12, 9, // priorities
            1, 0, // c-state, spare
            13, 1, 5, 0, // wait reason, mode, state, ideal
            0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(parse_cswitch(&p).unwrap(), (0x10, 0x20, 12, 9, 5, 13));
        assert!(parse_cswitch(&p[..10]).is_none());
    }

    #[test]
    fn stacks_hold_every_frame() {
        let mut p = 777u64.to_le_bytes().to_vec();
        p.extend(4u32.to_le_bytes());
        p.extend(99u32.to_le_bytes());
        for f in [0xfffff800_00001000u64, 0x7ff6_0000_1000] {
            p.extend(f.to_le_bytes());
        }
        p.extend([1, 2, 3]); // a stray partial frame is ignored
        let (ts, tid, frames) = parse_stack(&p, 8).unwrap();
        assert_eq!((ts, tid), (777, 99));
        assert_eq!(frames, vec![0xfffff800_00001000, 0x7ff6_0000_1000]);
    }

    #[test]
    fn disk_hard_fault_and_file_name_payloads() {
        let mut d = Vec::new();
        d.extend(1u32.to_le_bytes()); // disk
        d.extend(0u32.to_le_bytes()); // flags
        d.extend(4096u32.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend(8192u64.to_le_bytes());
        d.extend(0xf11eu64.to_le_bytes());
        d.extend(0x1u64.to_le_bytes());
        d.extend(5000u64.to_le_bytes());
        d.extend(42u32.to_le_bytes());
        let p = parse_disk(&d, 8, false).unwrap();
        assert_eq!((p.disk, p.bytes, p.offset, p.file, p.response_ticks, p.tid), (1, 4096, 8192, 0xf11e, 5000, Some(42)));
        // Older versions have no issuing thread.
        assert_eq!(parse_disk(&d[..d.len() - 4], 8, false).unwrap().tid, None);

        let mut h = 100u64.to_le_bytes().to_vec();
        h.extend(4096u64.to_le_bytes());
        h.extend(0x7ff0u64.to_le_bytes());
        h.extend(0xf11eu64.to_le_bytes());
        h.extend(7u32.to_le_bytes());
        h.extend(65536u32.to_le_bytes());
        assert_eq!(parse_hard_fault(&h, 8).unwrap(), (100, 4096, 0xf11e, 7, 65536));

        let mut f = 0xf11eu64.to_le_bytes().to_vec();
        f.extend(wide(r"\Device\HarddiskVolume3\game\data.pak"));
        assert_eq!(parse_file_name(&f, 8).unwrap(), (0xf11e, r"\Device\HarddiskVolume3\game\data.pak".to_string()));
    }

    #[test]
    fn present_payloads_for_both_runtimes() {
        let mut p = 0xabcu64.to_le_bytes().to_vec();
        p.extend(0x200u32.to_le_bytes());
        p.extend(1u32.to_le_bytes());
        assert_eq!(parse_present(&p, 8, Runtime::Dxgi).unwrap(), (0xabc, 0x200, 1));
        assert_eq!(parse_present(&p[..12], 8, Runtime::D3d9).unwrap(), (0xabc, 0x200, -1));
        assert!(parse_present(&p[..12], 8, Runtime::Dxgi).is_none());
    }

    #[test]
    fn names_of_wait_reasons_and_states() {
        assert_eq!(wait_reason(13), "WrUserRequest");
        assert_eq!(wait_reason(15), "WrQueue");
        assert_eq!(wait_reason(200), "Unknown");
        assert_eq!(thread_state(STATE_READY), "Ready");
        assert_eq!(thread_state(STATE_WAITING), "Waiting");
    }

    #[test]
    fn external_clock_values_map_onto_the_trace() {
        let s = SysEvents { zero_ticks: 1_000, freq: 10_000_000, ..Default::default() };
        assert_eq!(s.ticks_to_ns(1_000), Some(0));
        assert_eq!(s.ticks_to_ns(11_000), Some(1_000_000));
        assert_eq!(s.ticks_to_ns(999), None);
    }
}
