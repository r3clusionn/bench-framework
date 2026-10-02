//! Pinning the calling thread to one logical CPU, so a measurement is not moved between cores
//! (on a hybrid CPU, between a fast and a slow core) halfway through.

use std::io;

/// Pins the current thread to logical CPU `cpu`.
pub fn pin_current_thread(cpu: usize) -> io::Result<()> {
    imp::pin(cpu)
}

/// The logical CPU the current thread is running on right now, if the OS can say.
pub fn current_cpu() -> Option<usize> {
    imp::current()
}

#[cfg(windows)]
mod imp {
    use std::io;

    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadAffinityMask(thread: isize, mask: usize) -> usize;
        fn GetCurrentProcessorNumber() -> u32;
    }

    pub fn pin(cpu: usize) -> io::Result<()> {
        if cpu >= usize::BITS as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("CPU {cpu} is outside the first processor group (0 to {})", usize::BITS - 1),
            ));
        }
        // SAFETY: plain Win32 calls with a pseudo handle for the current thread.
        let previous = unsafe { SetThreadAffinityMask(GetCurrentThread(), 1usize << cpu) };
        if previous == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn current() -> Option<usize> {
        // SAFETY: no arguments, no side effects.
        Some(unsafe { GetCurrentProcessorNumber() } as usize)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;

    extern "C" {
        fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u64) -> i32;
        fn sched_getcpu() -> i32;
    }

    pub fn pin(cpu: usize) -> io::Result<()> {
        const WORDS: usize = 16; // 1024 CPUs, the size of glibc's cpu_set_t
        if cpu >= WORDS * 64 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("CPU {cpu} is out of range")));
        }
        let mut set = [0u64; WORDS];
        set[cpu / 64] = 1 << (cpu % 64);
        // SAFETY: `set` is a valid cpu_set_t of the stated size; pid 0 means the calling thread.
        let r = unsafe { sched_setaffinity(0, std::mem::size_of_val(&set), set.as_ptr()) };
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn current() -> Option<usize> {
        // SAFETY: no arguments.
        let c = unsafe { sched_getcpu() };
        (c >= 0).then_some(c as usize)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use std::io;

    pub fn pin(_cpu: usize) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "CPU pinning is only implemented for Windows and Linux"))
    }

    pub fn current() -> Option<usize> {
        None
    }
}
