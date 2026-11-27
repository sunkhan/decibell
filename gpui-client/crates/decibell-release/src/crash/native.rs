//! Native crashes: what a signal handler (Linux) or an unhandled-exception filter (Windows) can
//! write safely while the process dies, read back and sent at the next start (`mod.rs`).
//!
//! The record is a few `key=value` lines: a header prepared at start (release, os, this image's
//! address, size and ids) and what the crash adds (`signal`, `fault`, `pc`, `time`, and on Linux the
//! `/proc/self/maps` line holding `pc`, which names the library or driver). The Linux handler uses
//! only async-signal-safe calls (`open`, `read`, `write`, `close`, `time`, `sigaction`, `raise`) and
//! stack buffers; afterwards the previous action is put back and the crash goes on as it would
//! have (Rust's stack-overflow report, the default action, a core dump).

use std::path::Path;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

use super::Image;

/// A native crash as recorded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub release: String,
    pub os: String,
    pub image: ImageRecord,
    pub signal: u64,
    pub fault: usize,
    pub pc: usize,
    pub time: u64,
    pub module: Option<String>,
}

pub type ImageRecord = Image;

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        self.image_addr == other.image_addr && self.image_size == other.image_size && self.debug_id == other.debug_id
    }
}

impl Record {
    pub fn parse(text: &str) -> Option<Record> {
        let mut r = Record { image: Image { kind: "elf", ..Image::default() }, ..Record::default() };
        let num = |v: &str| -> Option<u64> {
            match v.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => v.parse().ok(),
            }
        };
        let mut saw_signal = false;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key {
                "release" => r.release = value.to_string(),
                "os" => {
                    r.os = value.to_string();
                    r.image.kind = if value == "windows" { "pe" } else { "elf" };
                }
                "image_addr" => r.image.image_addr = num(value)? as usize,
                "image_size" => r.image.image_size = num(value)? as usize,
                "debug_id" if !value.is_empty() => r.image.debug_id = Some(value.to_string()),
                "code_id" if !value.is_empty() => r.image.code_id = Some(value.to_string()),
                "signal" => {
                    r.signal = num(value)?;
                    saw_signal = true;
                }
                "fault" => r.fault = num(value)? as usize,
                "pc" => r.pc = num(value)? as usize,
                "time" => r.time = num(value)?,
                "module" => r.module = Some(value.split_whitespace().last().unwrap_or("").to_string()).filter(|m| m.starts_with('/') || m.contains('\\')),
                _ => {}
            }
        }
        saw_signal.then_some(r)
    }

    pub fn name(&self) -> &'static str {
        if self.os == "windows" {
            return match self.signal {
                0xC000_0005 => "EXCEPTION_ACCESS_VIOLATION",
                0xC000_001D => "EXCEPTION_ILLEGAL_INSTRUCTION",
                0xC000_00FD => "EXCEPTION_STACK_OVERFLOW",
                0xC000_0094 => "EXCEPTION_INT_DIVIDE_BY_ZERO",
                0xC000_0006 => "EXCEPTION_IN_PAGE_ERROR",
                0x8000_0003 => "EXCEPTION_BREAKPOINT",
                _ => "EXCEPTION",
            };
        }
        match self.signal {
            11 => "SIGSEGV",
            7 => "SIGBUS",
            4 => "SIGILL",
            8 => "SIGFPE",
            6 => "SIGABRT",
            _ => "SIGNAL",
        }
    }
}

/// The header every record starts with.
fn header(image: &Image) -> String {
    format!(
        "release=decibell@{}\nos={}\nimage_addr=0x{:x}\nimage_size=0x{:x}\ndebug_id={}\ncode_id={}\n",
        crate::VERSION,
        std::env::consts::OS,
        image.image_addr,
        image.image_size,
        image.debug_id.as_deref().unwrap_or(""),
        image.code_id.as_deref().unwrap_or("")
    )
}

/// Where this process's record goes.
fn record_path(dir: &Path) -> std::path::PathBuf {
    dir.join(format!("native-{}.native", std::process::id()))
}

// ── Linux ─────────────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
const SIGNALS: [libc::c_int; 5] = [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT];

#[cfg(target_os = "linux")]
struct Prepared {
    header: Vec<u8>,
    path: std::ffi::CString,
}

#[cfg(target_os = "linux")]
static PREPARED: OnceLock<Prepared> = OnceLock::new();
/// The actions in place before ours, by index in `SIGNALS` (written once at install, before any
/// handler can run; read only by the handler).
#[cfg(target_os = "linux")]
static mut PREVIOUS: [std::mem::MaybeUninit<libc::sigaction>; 5] = [std::mem::MaybeUninit::uninit(); 5];

#[cfg(target_os = "linux")]
pub fn install(dir: &Path, image: &Image) {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(record_path(dir).as_os_str().as_bytes()) else { return };
    if PREPARED.set(Prepared { header: header(image).into_bytes(), path }).is_err() {
        return;
    }
    for (i, signal) in SIGNALS.iter().enumerate() {
        // SAFETY: a zeroed sigaction is a valid starting point; we fill handler, flags and mask.
        // PREVIOUS[i] is written here, before the new action can deliver a signal to the handler.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut action.sa_mask);
            let previous = &raw mut PREVIOUS;
            libc::sigaction(*signal, &action, (*previous)[i].as_mut_ptr());
        }
    }
}

#[cfg(target_os = "linux")]
extern "C" fn handler(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    if !super::REPORTING.swap(true, std::sync::atomic::Ordering::SeqCst)
        && let Some(prepared) = PREPARED.get()
    {
        // SAFETY: the kernel hands a valid siginfo and ucontext to an SA_SIGINFO handler.
        unsafe { write_record(prepared, signal, info, context) };
    }
    let Some(i) = SIGNALS.iter().position(|s| *s == signal) else { return };
    // SAFETY: PREVIOUS[i] was initialised by `install` before this handler was installed; putting it
    // back is what lets the crash proceed as without us.
    unsafe {
        let previous = &raw const PREVIOUS;
        libc::sigaction(signal, (*previous)[i].as_ptr(), std::ptr::null_mut());
        // A signal sent (kill, raise, abort) is not re-raised by returning; a fault is (the
        // instruction runs again).
        if (*info).si_code <= 0 {
            libc::raise(signal);
        }
    }
}

/// # Safety
/// `info` and `context` come from the kernel for this signal.
#[cfg(target_os = "linux")]
unsafe fn write_record(prepared: &Prepared, signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    // SAFETY (whole body): only async-signal-safe calls on stack buffers and pre-built data.
    unsafe {
        let fd = libc::open(prepared.path.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC, 0o600);
        if fd < 0 {
            return;
        }
        put(fd, &prepared.header);
        let fault = (*info).si_addr() as usize;
        let pc = program_counter(context);
        let mut line = Line::new();
        line.text(b"signal=").dec(signal as u64).text(b"\nfault=").hex(fault).text(b"\npc=").hex(pc).text(b"\ntime=").dec(libc::time(std::ptr::null_mut()) as u64).text(b"\n");
        put(fd, line.bytes());
        module_line(fd, pc);
        libc::close(fd);
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn program_counter(context: *mut libc::c_void) -> usize {
    // SAFETY: an SA_SIGINFO handler's third argument is the interrupted ucontext.
    unsafe { (*(context as *const libc::ucontext_t)).uc_mcontext.gregs[libc::REG_RIP as usize] as usize }
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
unsafe fn program_counter(context: *mut libc::c_void) -> usize {
    // SAFETY: as above.
    unsafe { (*(context as *const libc::ucontext_t)).uc_mcontext.pc as usize }
}

#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
unsafe fn program_counter(_: *mut libc::c_void) -> usize {
    0
}

#[cfg(target_os = "linux")]
fn put(fd: libc::c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is valid for its length.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n <= 0 {
            return;
        }
        bytes = &bytes[n as usize..];
    }
}

/// Writes `module=<the /proc/self/maps line holding pc>`.
#[cfg(target_os = "linux")]
fn module_line(out: libc::c_int, pc: usize) {
    // SAFETY: a constant NUL-terminated path.
    let fd = unsafe { libc::open(c"/proc/self/maps".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return;
    }
    let mut chunk = [0u8; 512];
    let mut line = [0u8; 512];
    let mut len = 0;
    'read: loop {
        // SAFETY: `chunk` is valid for its length.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        for &b in &chunk[..n as usize] {
            if b != b'\n' {
                if len < line.len() {
                    line[len] = b;
                    len += 1;
                }
                continue;
            }
            if let Some((start, end)) = range(&line[..len])
                && (start..end).contains(&pc)
            {
                put(out, b"module=");
                put(out, &line[..len]);
                put(out, b"\n");
                break 'read;
            }
            len = 0;
        }
    }
    // SAFETY: `fd` is open.
    unsafe { libc::close(fd) };
}

/// `start-end` at the head of a maps line.
#[cfg(any(target_os = "linux", test))]
fn range(line: &[u8]) -> Option<(usize, usize)> {
    let mut start = 0usize;
    let mut end = 0usize;
    let mut i = 0;
    let digit = |b: u8| (b as char).to_digit(16);
    while i < line.len() && line[i] != b'-' {
        start = start.checked_mul(16)?.checked_add(digit(line[i])? as usize)?;
        i += 1;
    }
    i += 1;
    while i < line.len() && line[i] != b' ' {
        end = end.checked_mul(16)?.checked_add(digit(line[i])? as usize)?;
        i += 1;
    }
    (end > start).then_some((start, end))
}

/// A fixed buffer formatted without allocating.
#[cfg(target_os = "linux")]
struct Line {
    buf: [u8; 256],
    len: usize,
}

#[cfg(target_os = "linux")]
impl Line {
    fn new() -> Line {
        Line { buf: [0; 256], len: 0 }
    }

    fn text(&mut self, s: &[u8]) -> &mut Line {
        for &b in s {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        self
    }

    fn dec(&mut self, mut v: u64) -> &mut Line {
        let mut digits = [0u8; 20];
        let mut n = 0;
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            n += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        digits[..n].reverse();
        self.text(&digits[..n])
    }

    fn hex(&mut self, v: usize) -> &mut Line {
        self.text(b"0x");
        let mut started = false;
        for shift in (0..usize::BITS / 4).rev() {
            let nibble = (v >> (shift * 4)) & 0xf;
            if nibble != 0 || started || shift == 0 {
                started = true;
                self.text(&[b"0123456789abcdef"[nibble]]);
            }
        }
        self
    }

    fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

// ── Windows ───────────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
pub fn install(dir: &Path, image: &Image) {
    crate::win::install_exception_filter(record_path(dir), header(image));
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn install(_: &Path, _: &Image) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let text = "release=decibell@0.9.4\nos=linux\nimage_addr=0x55aa00000000\nimage_size=0x5000000\ndebug_id=03020100-0504-0706-0809-0a0b0c0d0e0f\ncode_id=000102\nsignal=11\nfault=0x0\npc=0x7f0012345678\ntime=1700000000\nmodule=7f0012300000-7f0012400000 r-xp 00000000 08:01 1234 /usr/lib/libnvidia-glcore.so.620.73\n";
        let r = Record::parse(text).unwrap();
        assert_eq!(r.name(), "SIGSEGV");
        assert_eq!(r.pc, 0x7f00_1234_5678);
        assert_eq!(r.image.image_addr, 0x55aa_0000_0000);
        assert_eq!(r.module.as_deref(), Some("/usr/lib/libnvidia-glcore.so.620.73"));
        assert!(Record::parse("release=x\n").is_none(), "no signal: not a record");
        assert_eq!(range(b"7f00-7f10 r-xp 0 0:0 0 /x"), Some((0x7f00, 0x7f10)));
        assert_eq!(range(b"garbage"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn formats_without_allocating() {
        let mut line = Line::new();
        line.text(b"a=").dec(0).text(b" ").dec(1_700_000_000).text(b" ").hex(0).text(b" ").hex(0x7f00_1234);
        assert_eq!(line.bytes(), b"a=0 1700000000 0x0 0x7f001234");
    }

    /// A child process that really segfaults leaves a record naming the signal and its pc.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_segfault_leaves_a_record() {
        if std::env::var_os("DECIBELL_TEST_SEGV_CHILD").is_some() {
            let dir = std::path::PathBuf::from(std::env::var_os("DECIBELL_TEST_SEGV_CHILD").unwrap());
            install(&dir, &super::super::image::main_image());
            // SAFETY: not safe at all: this child is meant to crash.
            unsafe { std::ptr::write_volatile(std::ptr::without_provenance_mut::<u64>(8), 1) };
            unreachable!();
        }
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash::native::tests::a_segfault_leaves_a_record", "--nocapture", "--test-threads=1"])
            .env("DECIBELL_TEST_SEGV_CHILD", dir.path())
            .status()
            .unwrap();
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(status.signal(), Some(libc::SIGSEGV), "the crash still ends the process as a SIGSEGV: {status}");
        let record = std::fs::read_dir(dir.path()).unwrap().flatten().find(|e| e.path().extension().is_some_and(|x| x == "native")).expect("a record");
        let r = Record::parse(&std::fs::read_to_string(record.path()).unwrap()).unwrap();
        assert_eq!(r.name(), "SIGSEGV");
        assert_eq!(r.fault, 8);
        assert!(r.image.image_size > 0 && r.image.contains(r.pc), "the pc is in our image: {r:?}");
        assert!(r.module.is_some_and(|m| m.contains("decibell_release")), "the maps line names the test binary");
    }
}
