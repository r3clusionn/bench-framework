//! A second trace session, next to xperf's kernel session, for the graphics providers that frame
//! analysis needs (DXGI, D3D9, DxgKrnl, Win32k, DWM).
//!
//! xperf can enable user-mode providers but cannot filter them by event id, and with every keyword
//! the graphics kernel alone writes about 50 MB per second. So the session is started here with
//! `StartTrace` and each provider enabled with `EnableTraceEx2` and an event-id filter: exactly the
//! events PresentMon 2.6 consumes (display, GPU and input tracking), so PresentMon can analyse the
//! merged trace with `--etl_file`, and benchlab reads the Present calls itself. The session uses
//! the performance counter as its clock, like the kernel session, so the two merge on one time line.

/// One provider and the events enabled on it.
pub struct Provider {
    pub name: &'static str,
    pub guid: u128,
    pub level: u8,
    pub any_keyword: u64,
    pub all_keyword: u64,
    pub event_ids: &'static [u16],
}

/// The providers, levels, keywords and event ids PresentMon 2.6 enables on Windows 11 (from its
/// `PresentMonTraceSession.cpp` and the generated `ETW/*.h` headers; see THIRD_PARTY_NOTICES).
pub const GRAPHICS: &[Provider] = &[
    Provider {
        name: "Microsoft-Windows-DxgKrnl",
        guid: 0x802ec45a_1e99_4b83_9920_87c98277ba9d,
        level: 4,
        any_keyword: 0x4000_0000_0800_0840,
        all_keyword: 0x4000_0000_0000_0000,
        event_ids: &[
            17, 27, 28, 29, 30, 31, 32, 116, 166, 168, 171, 172, 175, 177, 178, 180, 184, 215, 244, 252, 259, 266, 273, 382, 422,
            424, 501,
        ],
    },
    Provider {
        name: "Microsoft-Windows-Win32k",
        guid: 0x8c416c79_d49b_4f01_a467_e56d3aa8234c,
        level: 4,
        any_keyword: 0x8400_0004_40c0_1000,
        all_keyword: 0,
        event_ids: &[63, 73, 201, 225, 301],
    },
    Provider {
        name: "Microsoft-Windows-Dwm-Core",
        guid: 0x9e9bba3c_2e38_40cb_99f4_9e8281425164,
        level: 5,
        any_keyword: 0x8000_0000_0000_0081,
        all_keyword: 0,
        event_ids: &[15, 64, 69, 70, 101, 196],
    },
    Provider {
        name: "Microsoft-Windows-DXGI",
        guid: 0xca11c036_0102_4a2d_a6ad_f03cfed5d3c9,
        level: 0,
        any_keyword: 0x8000_0000_0000_0002,
        all_keyword: 0x8000_0000_0000_0002,
        event_ids: &[42, 43, 55, 56],
    },
    Provider {
        name: "Microsoft-Windows-D3D9",
        guid: 0x783aca0a_790e_4d7f_8451_aa850511c6b9,
        level: 0,
        any_keyword: 0x8000_0000_0000_0002,
        all_keyword: 0x8000_0000_0000_0002,
        event_ids: &[1, 2],
    },
];

/// DxgKrnl events to request a rundown of (devices and contexts that existed before the trace),
/// which PresentMon needs to attribute GPU work.
pub const DXGKRNL_RUNDOWN_IDS: &[u16] = &[29, 32];

#[cfg(windows)]
pub use win::UserSession;

#[cfg(not(windows))]
pub struct UserSession;

#[cfg(not(windows))]
impl UserSession {
    pub fn start(_name: &str, _file: &std::path::Path) -> Result<UserSession, String> {
        Err("trace sessions are only supported on Windows".to_string())
    }

    pub fn stop(self) -> Result<(), String> {
        Ok(())
    }

    pub fn stop_by_name(_name: &str) -> bool {
        false
    }
}

#[cfg(windows)]
mod win {
    use std::path::Path;

    use windows_sys::core::GUID;
    use windows_sys::Win32::System::Diagnostics::Etw::{
        ControlTraceW, EnableTraceEx2, StartTraceW, CONTROLTRACE_HANDLE, ENABLE_TRACE_PARAMETERS,
        ENABLE_TRACE_PARAMETERS_VERSION_2, EVENT_CONTROL_CODE_CAPTURE_STATE, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
        EVENT_ENABLE_PROPERTY_IGNORE_KEYWORD_0, EVENT_FILTER_DESCRIPTOR, EVENT_FILTER_TYPE_EVENT_ID, EVENT_TRACE_CONTROL_STOP,
        EVENT_TRACE_FILE_MODE_SEQUENTIAL, EVENT_TRACE_PROPERTIES, WNODE_FLAG_TRACED_GUID,
    };

    use super::{Provider, DXGKRNL_RUNDOWN_IDS, GRAPHICS};

    const ERROR_ALREADY_EXISTS: u32 = 183;
    const ERROR_WMI_INSTANCE_NOT_FOUND: u32 = 4201;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `EVENT_TRACE_PROPERTIES` followed by room for the session and file names.
    #[repr(C)]
    struct Props {
        p: EVENT_TRACE_PROPERTIES,
        names: [u16; 2 * 1024],
    }

    fn props(file: Option<&Path>) -> Box<Props> {
        let mut b: Box<Props> = Box::new(unsafe { std::mem::zeroed() });
        b.p.Wnode.BufferSize = std::mem::size_of::<Props>() as u32;
        b.p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        b.p.Wnode.ClientContext = 1; // performance counter, like the kernel session
        b.p.LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        b.p.LogFileNameOffset = b.p.LoggerNameOffset + 1024 * 2;
        if let Some(f) = file {
            let w = wide(&f.to_string_lossy());
            let n = w.len().min(1023);
            b.names[1024..1024 + n].copy_from_slice(&w[..n]);
        }
        b
    }

    fn guid(g: u128) -> GUID {
        GUID::from_u128(g)
    }

    /// Enables one provider with an event-id filter. `ids` must not be empty.
    unsafe fn enable(handle: CONTROLTRACE_HANDLE, session: GUID, p: &Provider, ids: &[u16], code: u32) -> u32 {
        // EVENT_FILTER_EVENT_ID: FilterIn, Reserved, Count, then the ids.
        let mut buf: Vec<u16> = Vec::with_capacity(2 + ids.len());
        buf.push(1); // FilterIn = TRUE, Reserved = 0
        buf.push(ids.len() as u16);
        buf.extend_from_slice(ids);
        let mut filter =
            EVENT_FILTER_DESCRIPTOR { Ptr: buf.as_ptr() as u64, Size: (buf.len() * 2) as u32, Type: EVENT_FILTER_TYPE_EVENT_ID };
        let params = ENABLE_TRACE_PARAMETERS {
            Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
            EnableProperty: EVENT_ENABLE_PROPERTY_IGNORE_KEYWORD_0,
            ControlFlags: 0,
            SourceId: session,
            EnableFilterDesc: &mut filter,
            FilterDescCount: 1,
        };
        EnableTraceEx2(handle, &guid(p.guid), code, p.level, p.any_keyword, p.all_keyword, 0, &params)
    }

    /// A running user-mode trace session writing to a file.
    pub struct UserSession {
        name: String,
        stopped: bool,
    }

    impl UserSession {
        /// Stops a session of this name if one is left over from an interrupted run.
        pub fn stop_by_name(name: &str) -> bool {
            let mut p = props(None);
            let n = wide(name);
            unsafe { ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, n.as_ptr(), &mut p.p, EVENT_TRACE_CONTROL_STOP) == 0 }
        }

        pub fn start(name: &str, file: &Path) -> Result<UserSession, String> {
            let n = wide(name);
            let mut p = props(Some(file));
            p.p.LogFileMode = EVENT_TRACE_FILE_MODE_SEQUENTIAL;
            p.p.BufferSize = 1024;
            p.p.MinimumBuffers = 64;
            p.p.MaximumBuffers = 256;
            let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
            let mut status = unsafe { StartTraceW(&mut handle, n.as_ptr(), &mut p.p) };
            if status == ERROR_ALREADY_EXISTS {
                Self::stop_by_name(name);
                p = props(Some(file));
                p.p.LogFileMode = EVENT_TRACE_FILE_MODE_SEQUENTIAL;
                p.p.BufferSize = 1024;
                p.p.MinimumBuffers = 64;
                p.p.MaximumBuffers = 256;
                status = unsafe { StartTraceW(&mut handle, n.as_ptr(), &mut p.p) };
            }
            if status != 0 {
                return Err(format!("could not start the graphics trace session (error {status})"));
            }
            let session = UserSession { name: name.to_string(), stopped: false };
            let sid = p.p.Wnode.Guid;
            for prov in GRAPHICS {
                let s = unsafe { enable(handle, sid, prov, prov.event_ids, EVENT_CONTROL_CODE_ENABLE_PROVIDER) };
                if s != 0 {
                    return Err(format!("could not enable {} (error {s})", prov.name));
                }
            }
            // Devices and contexts that already exist (a game that is already running).
            let s = unsafe { enable(handle, sid, &GRAPHICS[0], DXGKRNL_RUNDOWN_IDS, EVENT_CONTROL_CODE_CAPTURE_STATE) };
            if s != 0 {
                return Err(format!("could not request the DxgKrnl rundown (error {s})"));
            }
            Ok(session)
        }

        pub fn stop(mut self) -> Result<(), String> {
            self.stopped = true;
            let mut p = props(None);
            let n = wide(&self.name);
            let s = unsafe { ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, n.as_ptr(), &mut p.p, EVENT_TRACE_CONTROL_STOP) };
            if s != 0 && s != ERROR_WMI_INSTANCE_NOT_FOUND {
                return Err(format!("could not stop the graphics trace session (error {s})"));
            }
            Ok(())
        }
    }

    impl Drop for UserSession {
        fn drop(&mut self) {
            if !self.stopped {
                Self::stop_by_name(&self.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_table_is_consistent() {
        for p in GRAPHICS {
            assert!(!p.event_ids.is_empty(), "{}", p.name);
            assert!(p.event_ids.windows(2).all(|w| w[0] < w[1]), "{} ids sorted and unique", p.name);
            assert_eq!(p.any_keyword & p.all_keyword, p.all_keyword, "{}", p.name);
        }
        // The Present events benchlab reads itself are part of the set.
        let dxgi = GRAPHICS.iter().find(|p| p.guid == crate::trace::sys::DXGI).unwrap();
        assert!(dxgi.event_ids.contains(&42) && dxgi.event_ids.contains(&43));
        let d3d9 = GRAPHICS.iter().find(|p| p.guid == crate::trace::sys::D3D9).unwrap();
        assert_eq!(d3d9.event_ids, &[1, 2]);
        for id in DXGKRNL_RUNDOWN_IDS {
            assert!(GRAPHICS[0].event_ids.contains(id));
        }
    }
}
