//! Function names for ISR and DPC routines (`--symbols`).
//!
//! DbgHelp does the work. It is loaded from the Windows Performance Toolkit folder when possible,
//! because only that copy sits next to `symsrv.dll` and can fetch PDBs from a symbol server; the
//! copy in System32 cannot. The driver file is read from disk, so symbols are only used when its
//! `TimeDateStamp` equals the one recorded in the trace (the file has not been updated since).

/// Turns the kernel's image path into a path on disk.
///
/// `\SystemRoot\system32\x.sys` and `\??\C:\x.sys` are handled here; `\Device\HarddiskVolumeN\...`
/// needs the drive map, so the caller passes `devices` as (device path, drive letter) pairs.
pub fn dos_path(nt: &str, system_root: &str, devices: &[(String, String)]) -> Option<String> {
    let lower = nt.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix(r"\systemroot") {
        return Some(format!("{}{}", system_root.trim_end_matches('\\'), &nt[nt.len() - rest.len()..]));
    }
    if let Some(rest) = nt.strip_prefix(r"\??\") {
        return Some(rest.to_string());
    }
    for (dev, drive) in devices {
        let d = dev.to_ascii_lowercase();
        if lower.starts_with(&d) && nt[d.len()..].starts_with('\\') {
            return Some(format!("{drive}{}", &nt[d.len()..]));
        }
    }
    None
}

/// `TimeDateStamp` from a PE file's header.
pub fn pe_timestamp(bytes: &[u8]) -> Option<u32> {
    let lfanew = u32::from_le_bytes(bytes.get(0x3c..0x40)?.try_into().ok()?) as usize;
    if bytes.get(lfanew..lfanew + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u32::from_le_bytes(bytes.get(lfanew + 8..lfanew + 12)?.try_into().ok()?))
}

/// Builds the label of a routine from what a symbol lookup returned.
pub fn label(module: &str, symbol: Option<&str>, displacement: u64, offset_in_image: u64) -> String {
    match symbol {
        Some(s) if displacement == 0 => format!("{module}!{s}"),
        Some(s) => format!("{module}!{s}+0x{displacement:x}"),
        None => format!("{module}+0x{offset_in_image:x}"),
    }
}

#[cfg(windows)]
pub use win::Resolver;

#[cfg(not(windows))]
pub struct Resolver {
    pub skipped: Vec<String>,
}

#[cfg(not(windows))]
impl Resolver {
    pub fn has_symbol_server_support(&self) -> bool {
        false
    }

    pub fn new(_images: &[super::model::Image], _symbol_path: Option<&str>) -> Result<Resolver, String> {
        Err("symbol resolution is only supported on Windows".to_string())
    }

    pub fn resolve(&mut self, _addr: u64) -> String {
        String::new()
    }

    pub fn image(&self, _addr: u64) -> Option<&super::model::Image> {
        None
    }
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    use super::{dos_path, label, pe_timestamp};
    use crate::trace::model::{Image, ImageMap};

    #[repr(C)]
    struct SymbolInfoW {
        size_of_struct: u32,
        type_index: u32,
        reserved: [u64; 2],
        index: u32,
        size: u32,
        mod_base: u64,
        flags: u32,
        value: u64,
        address: u64,
        register: u32,
        scope: u32,
        tag: u32,
        name_len: u32,
        max_name_len: u32,
        name: [u16; 1],
    }

    const MAX_NAME: usize = 512;
    const SYMOPT_UNDNAME: u32 = 0x2;
    const SYMOPT_DEFERRED_LOADS: u32 = 0x4;
    const SYMOPT_FAIL_CRITICAL_ERRORS: u32 = 0x0020_0000;
    /// Fake process handles: DbgHelp only uses them as keys for its own state, so each resolver
    /// (the kernel's, and one per traced process) gets its own.
    static NEXT_SESSION: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0x0bec_0001);

    type SymSetOptions = unsafe extern "system" fn(u32) -> u32;
    type SymInitializeW = unsafe extern "system" fn(isize, *const u16, i32) -> i32;
    type SymLoadModuleExW = unsafe extern "system" fn(isize, isize, *const u16, *const u16, u64, u32, *mut c_void, u32) -> u64;
    type SymFromAddrW = unsafe extern "system" fn(isize, u64, *mut u64, *mut SymbolInfoW) -> i32;
    type SymCleanup = unsafe extern "system" fn(isize) -> i32;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn device_map() -> Vec<(String, String)> {
        let mut out = Vec::new();
        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", letter as char);
            let mut buf = [0u16; 512];
            let n = unsafe { QueryDosDeviceW(wide(&drive).as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
            if n > 0 {
                let end = buf.iter().position(|c| *c == 0).unwrap_or(0);
                out.push((String::from_utf16_lossy(&buf[..end]), drive));
            }
        }
        out
    }

    /// Default symbol path: the environment's, else a local cache plus Microsoft's symbol server.
    pub fn default_symbol_path() -> String {
        if let Ok(p) = std::env::var("_NT_SYMBOL_PATH") {
            if !p.trim().is_empty() {
                return p;
            }
        }
        let cache = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("benchlab")
            .join("symbols");
        format!("srv*{}*https://msdl.microsoft.com/download/symbols", cache.display())
    }

    /// A `dbghelp.dll` that has `symsrv.dll` beside it, so it can download symbols: from
    /// `BENCHLAB_DBGHELP`, the Performance Toolkit, or the Debugging Tools for Windows.
    pub fn find_dbghelp() -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(d) = std::env::var_os("BENCHLAB_DBGHELP") {
            dirs.push(PathBuf::from(d));
        }
        if let Ok(t) = crate::trace::xperf::find_tools() {
            if let Some(d) = t.xperf.parent() {
                dirs.push(d.to_path_buf());
                if let Some(kit) = d.parent() {
                    dirs.push(kit.join("Debuggers").join("x64"));
                    dirs.push(kit.join("App Certification Kit"));
                }
            }
        }
        dirs.into_iter()
            .find(|d| d.join("dbghelp.dll").is_file() && d.join("symsrv.dll").is_file())
            .map(|d| d.join("dbghelp.dll"))
    }

    pub struct Resolver {
        session: isize,
        map: ImageMap,
        images: Vec<Image>,
        loaded: HashMap<u64, bool>,
        cache: HashMap<u64, String>,
        from_wpt: bool,
        load_module: SymLoadModuleExW,
        from_addr: SymFromAddrW,
        cleanup: SymCleanup,
        system_root: String,
        devices: Vec<(String, String)>,
        /// Modules skipped because the file on disk is not the one that ran, with the reason.
        pub skipped: Vec<String>,
    }

    impl Resolver {
        /// `symbol_path` of `None` means the default; `Some("")` means "no symbol files, exports only".
        pub fn new(images: &[Image], symbol_path: Option<&str>) -> Result<Resolver, String> {
            unsafe {
                let mut from_wpt = false;
                let mut lib: HMODULE = std::ptr::null_mut();
                if let Some(p) = find_dbghelp() {
                    lib = LoadLibraryW(wide(&p.to_string_lossy()).as_ptr());
                    from_wpt = !lib.is_null();
                }
                if lib.is_null() {
                    lib = LoadLibraryW(wide("dbghelp.dll").as_ptr());
                }
                if lib.is_null() {
                    return Err("dbghelp.dll could not be loaded".to_string());
                }
                let get = |name: &[u8]| {
                    GetProcAddress(lib, name.as_ptr())
                        .ok_or_else(|| format!("dbghelp.dll has no {}", String::from_utf8_lossy(&name[..name.len() - 1])))
                };
                let set_options: SymSetOptions = std::mem::transmute(get(b"SymSetOptions\0")?);
                let initialize: SymInitializeW = std::mem::transmute(get(b"SymInitializeW\0")?);
                let load_module: SymLoadModuleExW = std::mem::transmute(get(b"SymLoadModuleExW\0")?);
                let from_addr: SymFromAddrW = std::mem::transmute(get(b"SymFromAddrW\0")?);
                let cleanup: SymCleanup = std::mem::transmute(get(b"SymCleanup\0")?);

                set_options(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_FAIL_CRITICAL_ERRORS);
                let path = match symbol_path {
                    Some(p) => p.to_string(),
                    None => default_symbol_path(),
                };
                let path_w = wide(&path);
                let session = NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if initialize(session, if path.is_empty() { std::ptr::null() } else { path_w.as_ptr() }, 0) == 0 {
                    return Err("SymInitialize failed".to_string());
                }
                let mut r = Resolver {
                    session,
                    map: ImageMap::new(images),
                    images: images.to_vec(),
                    loaded: HashMap::new(),
                    cache: HashMap::new(),
                    from_wpt,
                    load_module,
                    from_addr,
                    cleanup,
                    system_root: std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string()),
                    devices: device_map(),
                    skipped: Vec::new(),
                };
                r.images.sort_by_key(|i| i.base);
                Ok(r)
            }
        }

        /// The image that contains an address.
        pub fn image(&self, addr: u64) -> Option<&Image> {
            self.map.find(addr)
        }

        /// True when the symbol-server-capable DbgHelp from the toolkit is in use.
        pub fn has_symbol_server_support(&self) -> bool {
            self.from_wpt
        }

        fn load(&mut self, image: &Image) -> bool {
            if let Some(ok) = self.loaded.get(&image.base) {
                return *ok;
            }
            let ok = self.try_load(image);
            self.loaded.insert(image.base, ok);
            ok
        }

        fn try_load(&mut self, image: &Image) -> bool {
            let Some(path) = dos_path(&image.path, &self.system_root, &self.devices) else {
                self.skipped.push(format!("{}: cannot locate {}", image.name, image.path));
                return false;
            };
            match std::fs::read(&path) {
                Ok(bytes) => {
                    if image.timestamp != 0 && pe_timestamp(&bytes).is_some_and(|t| t != image.timestamp) {
                        self.skipped.push(format!("{}: the file on disk is not the version that ran", image.name));
                        return false;
                    }
                }
                Err(e) => {
                    self.skipped.push(format!("{}: {path}: {e}", image.name));
                    return false;
                }
            }
            let base = unsafe {
                (self.load_module)(
                    self.session,
                    0,
                    wide(&path).as_ptr(),
                    std::ptr::null(),
                    image.base,
                    image.size as u32,
                    std::ptr::null_mut(),
                    0,
                )
            };
            base != 0
        }

        /// `module!Function` for a routine address; `module+0xoffset` when no symbol is known.
        pub fn resolve(&mut self, addr: u64) -> String {
            if let Some(s) = self.cache.get(&addr) {
                return s.clone();
            }
            let text = match self.map.find(addr).cloned() {
                None => format!("unknown+0x{addr:x}"),
                Some(image) => {
                    let mut name = None;
                    let mut disp = 0u64;
                    const SYMFLAG_EXPORT: u32 = 0x200;
                    if self.load(&image) {
                        let mut buf = vec![0u64; (std::mem::size_of::<SymbolInfoW>() + MAX_NAME * 2) / 8 + 1];
                        let info = buf.as_mut_ptr() as *mut SymbolInfoW;
                        unsafe {
                            (*info).size_of_struct = std::mem::size_of::<SymbolInfoW>() as u32;
                            (*info).max_name_len = MAX_NAME as u32;
                            if (self.from_addr)(self.session, addr, &mut disp, info) != 0 {
                                let n = ((*info).name_len as usize).min(MAX_NAME);
                                let p = std::ptr::addr_of!((*info).name) as *const u16;
                                // The nearest export far below an address is not its function.
                                if !((*info).flags & SYMFLAG_EXPORT != 0 && disp != 0) {
                                    name = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                                }
                            }
                        }
                    }
                    label(&image.name, name.as_deref(), disp, addr - image.base)
                }
            };
            self.cache.insert(addr, text.clone());
            text
        }
    }

    impl Drop for Resolver {
        fn drop(&mut self) {
            unsafe {
                (self.cleanup)(self.session);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use crate::trace::model::Image;

    #[test]
    fn nt_paths_become_dos_paths() {
        let devs = vec![
            (r"\Device\HarddiskVolume3".to_string(), "C:".to_string()),
            (r"\Device\HarddiskVolume30".to_string(), "D:".to_string()),
        ];
        let root = r"C:\Windows";
        assert_eq!(
            dos_path(r"\SystemRoot\System32\drivers\ndis.sys", root, &devs).unwrap(),
            r"C:\Windows\System32\drivers\ndis.sys"
        );
        assert_eq!(
            dos_path(r"\systemroot\system32\ntoskrnl.exe", "C:\\Windows\\", &devs).unwrap(),
            r"C:\Windows\system32\ntoskrnl.exe"
        );
        assert_eq!(dos_path(r"\??\C:\x\y.sys", root, &devs).unwrap(), r"C:\x\y.sys");
        assert_eq!(dos_path(r"\Device\HarddiskVolume3\a\b.sys", root, &devs).unwrap(), r"C:\a\b.sys");
        // Volume 30 is not volume 3.
        assert_eq!(dos_path(r"\Device\HarddiskVolume30\a.sys", root, &devs).unwrap(), r"D:\a.sys");
        assert_eq!(dos_path(r"\Device\HarddiskVolume9\a.sys", root, &devs), None);
        assert_eq!(dos_path("", root, &devs), None);
    }

    #[test]
    fn pe_timestamp_reads_the_file_header() {
        let mut f = vec![0u8; 0x100];
        f[0x3c] = 0x80;
        f[0x80..0x84].copy_from_slice(b"PE\0\0");
        f[0x88..0x8c].copy_from_slice(&0xe24a_e1f8u32.to_le_bytes());
        assert_eq!(pe_timestamp(&f), Some(0xe24a_e1f8));
        f[0x80] = b'X';
        assert_eq!(pe_timestamp(&f), None);
        assert_eq!(pe_timestamp(&[0u8; 8]), None);
        assert_eq!(pe_timestamp(&[]), None);
    }

    #[test]
    fn labels() {
        assert_eq!(label("a.sys", Some("Foo"), 0, 0x10), "a.sys!Foo");
        assert_eq!(label("a.sys", Some("Foo"), 0x24, 0x10), "a.sys!Foo+0x24");
        assert_eq!(label("a.sys", None, 0, 0x10), "a.sys+0x10");
    }

    /// Resolves an address inside this very process's kernel32.dll using only its export table
    /// (an empty symbol path means no symbol files and no network).
    #[cfg(windows)]
    #[test]
    fn resolves_an_exported_function_without_symbol_files() {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        let name: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
        let (base, addr) = unsafe {
            let m = GetModuleHandleW(name.as_ptr());
            assert!(!m.is_null());
            (m as u64, GetProcAddress(m, c"CreateFileW".as_ptr().cast()).unwrap() as usize as u64)
        };
        let sys32 = format!(r"{}\System32\kernel32.dll", std::env::var("SystemRoot").unwrap());
        let bytes = std::fs::read(&sys32).unwrap();
        let lfanew = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(bytes[lfanew + 24 + 56..lfanew + 24 + 60].try_into().unwrap()) as u64;
        let image = Image {
            base,
            size,
            name: "kernel32.dll".into(),
            path: format!(r"\??\{sys32}"),
            timestamp: pe_timestamp(&bytes).unwrap(),
        };
        let mut r = Resolver::new(std::slice::from_ref(&image), Some("")).unwrap();
        let got = r.resolve(addr);
        assert!(got.starts_with("kernel32.dll!") && got.contains("CreateFile"), "{got}");
        assert!(r.skipped.is_empty(), "{:?}", r.skipped);
        // An address outside every image.
        assert_eq!(r.resolve(0x10), "unknown+0x10");
        // A wrong timestamp means the file on disk is not what ran: fall back to module+offset.
        let stale = Image { timestamp: image.timestamp ^ 1, ..image };
        let mut r = Resolver::new(&[stale], Some("")).unwrap();
        assert_eq!(r.resolve(addr), format!("kernel32.dll+0x{:x}", addr - base));
        assert!(r.skipped[0].contains("not the version that ran"));
    }
}
