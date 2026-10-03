//! A tiny Direct3D 11 program with known frame times, for checking `benchlab trace` end to end.
//!
//! It opens a window, clears and presents a frame every `--frame-ms` milliseconds, and every
//! `--stall-every` frames stalls the render thread for `--stall-ms` milliseconds, either busy
//! (CPU bound) or sleeping (a wait). It prints the performance-counter time of every stalled frame's
//! Present, so a test can find exactly those frames in a trace.
//!
//! ```text
//! cargo run --release --example frames -- --seconds 10 --stall-every 100 --stall-ms 40 --mode alternate
//! ```

#[cfg(not(windows))]
fn main() {
    eprintln!("this example needs Windows");
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::run() {
        eprintln!("frames: {e}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod win {
    use std::time::{Duration, Instant};

    use windows::core::{w, Interface};
    use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDeviceAndSwapChain, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
        D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{
        IDXGISwapChain, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
        DXGI_USAGE_RENDER_TARGET_OUTPUT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Performance::QueryPerformanceCounter;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW, ShowWindow, TranslateMessage,
        CS_HREDRAW, CS_VREDRAW, MSG, PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };

    struct Args {
        seconds: f64,
        frame_ms: f64,
        stall_every: u64,
        stall_ms: f64,
        mode: String,
    }

    fn args() -> Result<Args, String> {
        let mut a = Args { seconds: 10.0, frame_ms: 5.0, stall_every: 100, stall_ms: 40.0, mode: "alternate".into() };
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            let mut v = || it.next().ok_or(format!("{k} needs a value"));
            match k.as_str() {
                "--seconds" => a.seconds = v()?.parse().map_err(|e| format!("{e}"))?,
                "--frame-ms" => a.frame_ms = v()?.parse().map_err(|e| format!("{e}"))?,
                "--stall-every" => a.stall_every = v()?.parse().map_err(|e| format!("{e}"))?,
                "--stall-ms" => a.stall_ms = v()?.parse().map_err(|e| format!("{e}"))?,
                "--mode" => a.mode = v()?,
                _ => return Err(format!("unknown argument {k}")),
            }
        }
        if !["spin", "sleep", "alternate"].contains(&a.mode.as_str()) {
            return Err("--mode is spin, sleep or alternate".into());
        }
        Ok(a)
    }

    extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    fn qpc() -> i64 {
        let mut v = 0i64;
        unsafe {
            let _ = QueryPerformanceCounter(&mut v);
        }
        v
    }

    fn spin(d: Duration) {
        let end = Instant::now() + d;
        let mut x = 0u64;
        while Instant::now() < end {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }

    pub fn run() -> Result<(), String> {
        let a = args()?;
        unsafe {
            let inst: HMODULE = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = w!("benchlab-frames");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: inst.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("benchlab frames"),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                640,
                360,
                None,
                None,
                Some(inst.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            let _ = ShowWindow(hwnd, SW_SHOW);

            let desc = DXGI_SWAP_CHAIN_DESC {
                BufferDesc: DXGI_MODE_DESC { Width: 640, Height: 360, Format: DXGI_FORMAT_B8G8R8A8_UNORM, ..Default::default() },
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                OutputWindow: hwnd,
                Windowed: true.into(),
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                Flags: DXGI_SWAP_CHAIN_FLAG(0).0 as u32,
            };
            let mut sc: Option<IDXGISwapChain> = None;
            let mut dev: Option<ID3D11Device> = None;
            let mut ctx: Option<ID3D11DeviceContext> = None;
            D3D11CreateDeviceAndSwapChain(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&desc),
                Some(&mut sc),
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )
            .map_err(|e| e.to_string())?;
            let (sc, dev, ctx) = (sc.unwrap(), dev.unwrap(), ctx.unwrap());
            let back: ID3D11Texture2D = sc.GetBuffer(0).map_err(|e| e.to_string())?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            dev.CreateRenderTargetView(
                &back.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>().unwrap(),
                None,
                Some(&mut rtv),
            )
            .map_err(|e| e.to_string())?;
            let rtv = rtv.unwrap();

            println!("pid {}", std::process::id());
            let start = Instant::now();
            let frame = Duration::from_secs_f64(a.frame_ms / 1000.0);
            let mut next = Instant::now();
            let mut i: u64 = 0;
            let mut stalls = 0u64;
            while start.elapsed().as_secs_f64() < a.seconds {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                i += 1;
                let shade = (i % 255) as f32 / 255.0;
                ctx.ClearRenderTargetView(&rtv, &[shade, 0.2, 0.4, 1.0]);
                let stalled = a.stall_every > 0 && i.is_multiple_of(a.stall_every);
                if stalled {
                    stalls += 1;
                    let d = Duration::from_secs_f64(a.stall_ms / 1000.0);
                    let kind = match a.mode.as_str() {
                        "spin" => "spin",
                        "sleep" => "sleep",
                        _ if stalls % 2 == 1 => "spin",
                        _ => "sleep",
                    };
                    if kind == "spin" {
                        spin(d);
                    } else {
                        std::thread::sleep(d);
                    }
                    // The Present right after the stall ends the slow frame.
                    println!("stall {kind} qpc {}", qpc());
                }
                let _ = sc.Present(0, DXGI_PRESENT(0));
                next += frame;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
            println!("frames {i}");
        }
        Ok(())
    }
}
