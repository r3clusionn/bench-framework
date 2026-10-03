//! Drives the Windows Performance Toolkit: finds `xperf.exe` and `wpa.exe`, builds the command
//! lines for recording, merging and stopping kernel traces, and opens traces in WPA.
//!
//! The command builders are plain functions from options to argument lists so they can be tested
//! without a toolkit installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A named set of kernel flags for a common question.
pub struct Preset {
    pub name: &'static str,
    pub flags: &'static str,
    pub stackwalk: &'static str,
    pub about: &'static str,
    /// Also record the graphics providers (Present calls, GPU work) in a second session.
    pub graphics: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "dpc",
        flags: "PROC_THREAD+LOADER+DPC+INTERRUPT",
        stackwalk: "",
        about: "ISR and DPC durations per driver. Small traces, what `benchlab trace report` reads.",
        graphics: false,
    },
    Preset {
        name: "game",
        flags: "PROC_THREAD+LOADER+DPC+INTERRUPT+CSWITCH+DISPATCHER+PROFILE+HARD_FAULTS+DISK_IO+DISK_IO_INIT+FILENAME",
        stackwalk: "Profile+CSwitch+ReadyThread",
        about: "Everything `trace analyze` reads: frames (Present calls and GPU work), scheduling, CPU samples with stacks, DPC/ISR, hard faults, disk.",
        graphics: true,
    },
    Preset {
        name: "latency",
        flags: "PROC_THREAD+LOADER+DPC+INTERRUPT+CSWITCH+PROFILE",
        stackwalk: "Profile+CSwitch+ReadyThread",
        about: "DPC and ISR plus scheduling and CPU sampling with stacks: why a thread was late in WPA.",
        graphics: false,
    },
    Preset {
        name: "cpu",
        flags: "PROC_THREAD+LOADER+PROFILE+CSWITCH+DISPATCHER",
        stackwalk: "Profile+CSwitch+ReadyThread",
        about: "Where CPU time goes and what threads wait on.",
        graphics: false,
    },
    Preset {
        name: "disk",
        flags: "PROC_THREAD+LOADER+DISK_IO+DISK_IO_INIT+FILE_IO+FILE_IO_INIT+FILENAME+HARD_FAULTS",
        stackwalk: "DiskReadInit+DiskWriteInit",
        about: "Disk and file activity, hard faults.",
        graphics: false,
    },
    Preset {
        name: "power",
        flags: "PROC_THREAD+LOADER+POWER+IDLE_STATES+PROFILE+CLOCKINT",
        stackwalk: "",
        about: "Idle states, clock interrupts and power events.",
        graphics: false,
    },
    Preset {
        name: "full",
        flags: "Diag",
        stackwalk: "Profile+CSwitch+ReadyThread",
        about: "xperf's own Diag group: everything useful for a general investigation, large traces.",
        graphics: false,
    },
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Kernel flags and groups `xperf -on` accepts (from `xperf -providers KF KG`).
const KNOWN_FLAGS: &[&str] = &[
    "PROC_THREAD",
    "LOADER",
    "PROFILE",
    "CSWITCH",
    "COMPACT_CSWITCH",
    "DISPATCHER",
    "DPC",
    "IDEAL_PROC",
    "INTERRUPT",
    "INTERRUPT_STEER",
    "WDF_DPC",
    "WDF_INTERRUPT",
    "SYSCALL",
    "PRIORITY",
    "SPINLOCK",
    "KQUEUE",
    "ALPC",
    "PERF_COUNTER",
    "DISK_IO",
    "DISK_IO_INIT",
    "FILE_IO",
    "FILE_IO_INIT",
    "HARD_FAULTS",
    "FILENAME",
    "SPLIT_IO",
    "REGISTRY",
    "REG_HIVE",
    "DRIVERS",
    "POWER",
    "CC",
    "NETWORKTRACE",
    "VIRT_ALLOC",
    "MEMINFO",
    "ALL_FAULTS",
    "MEMINFO_WS",
    "VAMAP",
    "FOOTPRINT",
    "MEMORY",
    "NONTRADEABLE_MEMORY",
    "REFSET",
    "HIBERRUNDOWN",
    "CONTMEMGEN",
    "POOL",
    "SHOULDYIELD",
    "VTL_CHANGE",
    "CPU_CONFIG",
    "SESSION",
    "IDLE_STATES",
    "TIMER",
    "CLOCKINT",
    "IPI",
    "OPTICAL_IO",
    "OPTICAL_IO_INIT",
    "FLT_IO_INIT",
    "FLT_IO",
    "FLT_FASTIO",
    "FLT_IO_FAILURE",
    "OB_HANDLE",
    "OB_OBJECT",
    "KE_CLOCK",
    "PMC_PROFILE",
    "DPC_QUEUE",
    "CACHE_FLUSH",
    "DEBUG_EVENTS",
    "HV_CALLOUTS",
    "BYPASSIO_VETO",
    "DISABLE_PRIQ",
    "BASE",
    "DIAG",
    "DIAGEASY",
    "LATENCY",
    "FILEIO",
    "IOTRACE",
    "RESUMETRACE",
    "SYSPROF",
    "RESIDENTSET",
    "REFERENCESET",
    "NETWORK",
];

/// Checks a `+` separated flag list and returns it normalised (no spaces, no empty parts).
pub fn validate_flags(flags: &str) -> Result<String, String> {
    let parts: Vec<&str> = flags.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err("no kernel flags given".to_string());
    }
    for p in &parts {
        let hex = p.strip_prefix("0x").is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()));
        if !hex && !KNOWN_FLAGS.contains(&p.to_ascii_uppercase().as_str()) {
            return Err(format!("unknown kernel flag `{p}` (see `benchlab trace presets` or `xperf -providers KF KG`)"));
        }
    }
    Ok(parts.join("+"))
}

/// Everything that goes into starting a kernel trace.
#[derive(Clone, Debug, PartialEq)]
pub struct StartPlan {
    pub flags: String,
    pub stackwalk: String,
    pub file: PathBuf,
    pub buffer_kb: u32,
    pub min_buffers: u32,
    pub max_buffers: u32,
}

impl StartPlan {
    pub fn new(flags: &str, stackwalk: &str, file: &Path) -> StartPlan {
        // Big buffers: a DPC and CSWITCH trace of 24 CPUs loses events with the 64 KB default.
        StartPlan {
            flags: flags.to_string(),
            stackwalk: stackwalk.to_string(),
            file: file.to_path_buf(),
            buffer_kb: 1024,
            min_buffers: 128,
            max_buffers: 512,
        }
    }

    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["-on".to_string(), self.flags.clone()];
        if !self.stackwalk.is_empty() {
            a.push("-stackwalk".into());
            a.push(self.stackwalk.clone());
        }
        a.push("-f".into());
        a.push(self.file.to_string_lossy().into_owned());
        a.push("-BufferSize".into());
        a.push(self.buffer_kb.to_string());
        a.push("-MinBuffers".into());
        a.push(self.min_buffers.to_string());
        a.push("-MaxBuffers".into());
        a.push(self.max_buffers.to_string());
        a
    }
}

/// `xperf -d out.etl`: stop every logger and merge the result, which adds the image and process
/// rundown a trace needs to be analysed on another machine.
pub fn stop_and_merge_args(out: &Path) -> Vec<String> {
    vec!["-d".to_string(), out.to_string_lossy().into_owned()]
}

pub fn merge_args(inputs: &[PathBuf], out: &Path) -> Vec<String> {
    let mut a = vec!["-merge".to_string()];
    a.extend(inputs.iter().map(|p| p.to_string_lossy().into_owned()));
    a.push(out.to_string_lossy().into_owned());
    a
}

/// A symbol path for WPA: a local cache plus Microsoft's symbol server, unless one is set already.
pub fn symbol_path(existing: Option<&str>) -> String {
    match existing {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => r"srv*C:\Symbols*https://msdl.microsoft.com/download/symbols".to_string(),
    }
}

pub fn wpa_args(etl: &Path, profile: Option<&Path>) -> Vec<String> {
    let mut a = vec!["-i".to_string(), etl.to_string_lossy().into_owned()];
    if let Some(p) = profile {
        a.push("-profile".into());
        a.push(p.to_string_lossy().into_owned());
    }
    a
}

/// Paths of the installed tools.
#[derive(Clone, Debug)]
pub struct Tools {
    pub xperf: PathBuf,
    pub wpa: Option<PathBuf>,
}

fn candidates(name: &str) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("BENCHLAB_WPT") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(pf) = std::env::var_os(var) {
            let kits = PathBuf::from(pf).join("Windows Kits");
            for ver in ["10", "11"] {
                dirs.push(kits.join(ver).join("Windows Performance Toolkit"));
            }
        }
    }
    dirs.into_iter().map(|d| d.join(name)).collect()
}

fn find(name: &str) -> Option<PathBuf> {
    candidates(name).into_iter().find(|p| p.is_file())
}

pub fn find_tools() -> Result<Tools, String> {
    let xperf = find("xperf.exe").ok_or_else(|| {
        "xperf.exe not found. Install the Windows Performance Toolkit (part of the Windows ADK) or set BENCHLAB_WPT to its folder".to_string()
    })?;
    Ok(Tools { xperf, wpa: find("wpa.exe") })
}

/// Result of running a tool.
pub struct Output {
    pub ok: bool,
    pub text: String,
}

pub fn run(exe: &Path, args: &[String]) -> Result<Output, String> {
    let out = Command::new(exe).args(args).stdin(Stdio::null()).output().map_err(|e| format!("{}: {e}", exe.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(Output { ok: out.status.success(), text: text.trim().to_string() })
}

/// Whether a trace logger by this name already runs, from `xperf -loggers` output.
pub fn kernel_logger_running(loggers_output: &str) -> bool {
    loggers_output.lines().any(|l| l.trim_start().starts_with("Logger Name") && l.contains("NT Kernel Logger"))
}

/// Opens a trace in WPA and returns immediately.
pub fn open_in_wpa(tools: &Tools, etl: &Path, profile: Option<&Path>, with_symbols: bool) -> Result<(), String> {
    let wpa = tools.wpa.as_ref().ok_or("wpa.exe not found next to xperf.exe (install Windows Performance Analyzer)")?;
    let mut cmd = Command::new(wpa);
    cmd.args(wpa_args(etl, profile)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if with_symbols {
        cmd.env("_NT_SYMBOL_PATH", symbol_path(std::env::var("_NT_SYMBOL_PATH").ok().as_deref()));
    }
    cmd.spawn().map_err(|e| format!("{}: {e}", wpa.display()))?;
    Ok(())
}

/// True if this process runs with administrator rights.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut e as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_has_valid_flags() {
        for p in PRESETS {
            assert!(validate_flags(p.flags).is_ok(), "{}", p.name);
        }
        assert!(preset("LATENCY").is_some() && preset("nope").is_none());
    }

    #[test]
    fn flags_are_validated_and_normalised() {
        assert_eq!(validate_flags(" dpc + interrupt ").unwrap(), "dpc+interrupt");
        assert_eq!(validate_flags("PROC_THREAD+0x0001").unwrap(), "PROC_THREAD+0x0001");
        assert!(validate_flags("DPC+NOT_A_FLAG").unwrap_err().contains("NOT_A_FLAG"));
        assert!(validate_flags("").is_err());
        assert!(validate_flags("+").is_err());
        assert!(validate_flags("0x").is_err());
    }

    #[test]
    fn start_arguments_are_in_the_order_xperf_wants() {
        let plan = StartPlan::new("DPC+INTERRUPT", "Profile", Path::new(r"C:\t\k.etl"));
        assert_eq!(
            plan.args(),
            [
                "-on",
                "DPC+INTERRUPT",
                "-stackwalk",
                "Profile",
                "-f",
                r"C:\t\k.etl",
                "-BufferSize",
                "1024",
                "-MinBuffers",
                "128",
                "-MaxBuffers",
                "512"
            ]
        );
        let none = StartPlan::new("DPC", "", Path::new("k.etl"));
        assert!(!none.args().contains(&"-stackwalk".to_string()));
    }

    #[test]
    fn merge_and_stop_arguments() {
        assert_eq!(stop_and_merge_args(Path::new("o.etl")), ["-d", "o.etl"]);
        assert_eq!(
            merge_args(&[PathBuf::from("a.etl"), PathBuf::from("b.etl")], Path::new("m.etl")),
            ["-merge", "a.etl", "b.etl", "m.etl"]
        );
    }

    #[test]
    fn wpa_arguments_and_symbol_path() {
        assert_eq!(wpa_args(Path::new("t.etl"), None), ["-i", "t.etl"]);
        assert_eq!(wpa_args(Path::new("t.etl"), Some(Path::new("p.wpaProfile"))), ["-i", "t.etl", "-profile", "p.wpaProfile"]);
        assert_eq!(symbol_path(Some("srv*D:\\s")), "srv*D:\\s");
        assert!(symbol_path(None).contains("msdl.microsoft.com"));
        assert!(symbol_path(Some("  ")).contains("msdl.microsoft.com"));
    }

    #[test]
    fn running_kernel_logger_is_recognised() {
        assert!(kernel_logger_running("Logger Name           : NT Kernel Logger\nLogger Id : 1"));
        assert!(!kernel_logger_running("Logger Name           : Circular Kernel Context Logger"));
        assert!(!kernel_logger_running(""));
    }
}
