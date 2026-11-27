//! Windows pieces: the OS version, this image's identity for crash reports, the native crash
//! filter, and the Start menu shortcut with the app's AppUserModelID (written for the installer,
//! which has no way of its own to set that property). Written against `windows` 0.62.2 (the
//! version GPUI's Windows platform uses), compiled by the Windows checks only.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, WriteFile};
use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::core::HSTRING;

use crate::crash::Image;

/// `major.minor.build` from `RtlGetVersion` (not subject to the compatibility shims of
/// `GetVersionEx`).
pub fn os_version() -> Option<String> {
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut info = OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: `info` is a valid OSVERSIONINFOW with its size set, as the call requires.
    let status = unsafe { RtlGetVersion(&mut info) };
    status.is_ok().then(|| format!("{}.{}.{}", info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber))
}

/// This executable's load address, size and identity, read from its own PE headers in memory.
pub fn main_image(code_file: String) -> Image {
    let mut image = Image { kind: "pe", code_file, ..Image::default() };
    // SAFETY: GetModuleHandleW(None) is this process's executable, mapped for the process's life.
    let Ok(module) = (unsafe { GetModuleHandleW(None) }) else { return image };
    let base = module.0 as usize;
    // SAFETY: every read below stays inside the image's headers and its debug directory, whose
    // offsets the loader validated when it mapped the image.
    unsafe {
        let read32 = |at: usize| std::ptr::read_unaligned((base + at) as *const u32);
        let read16 = |at: usize| std::ptr::read_unaligned((base + at) as *const u16);
        if read16(0) != 0x5A4D {
            return image; // "MZ"
        }
        let nt = read32(0x3C) as usize;
        if read32(nt) != 0x0000_4550 {
            return image; // "PE\0\0"
        }
        let timestamp = read32(nt + 8);
        let optional = nt + 24;
        let size_of_image = read32(optional + 56);
        image.image_addr = base;
        image.image_size = size_of_image as usize;
        image.code_id = Some(format!("{timestamp:08X}{size_of_image:x}"));
        // PE32+: the data directories start at offset 112; the debug directory is entry 6.
        if read16(optional) == 0x20B {
            let (rva, size) = (read32(optional + 112 + 6 * 8) as usize, read32(optional + 112 + 6 * 8 + 4) as usize);
            for entry in (0..size / 28).map(|i| rva + i * 28) {
                let (kind, data_size, data_rva) = (read32(entry + 12), read32(entry + 16) as usize, read32(entry + 20) as usize);
                // IMAGE_DEBUG_TYPE_CODEVIEW with an "RSDS" record: GUID + age.
                if kind == 2 && data_size >= 24 && read32(data_rva) == 0x5344_5352 {
                    let guid = std::slice::from_raw_parts((base + data_rva + 4) as *const u8, 16);
                    let age = read32(data_rva + 20);
                    let d1 = u32::from_le_bytes(guid[0..4].try_into().unwrap_or_default());
                    let d2 = u16::from_le_bytes(guid[4..6].try_into().unwrap_or_default());
                    let d3 = u16::from_le_bytes(guid[6..8].try_into().unwrap_or_default());
                    let rest: String = guid[8..].iter().map(|b| format!("{b:02x}")).collect();
                    image.debug_id = Some(format!("{d1:08x}-{d2:04x}-{d3:04x}-{}-{}-{age:x}", &rest[..4], &rest[4..]));
                }
            }
        }
    }
    image
}

struct Prepared {
    path: HSTRING,
    header: Vec<u8>,
}

static PREPARED: OnceLock<Prepared> = OnceLock::new();

/// The unhandled-exception filter: writes the record (`crash::native`) and lets Windows go on
/// (Windows Error Reporting, the process ends). Rust's own aborts (`__fastfail`) never reach it.
pub fn install_exception_filter(path: PathBuf, header: String) {
    if PREPARED.set(Prepared { path: HSTRING::from(path.as_os_str()), header: header.into_bytes() }).is_err() {
        return;
    }
    // SAFETY: `filter` matches LPTOP_LEVEL_EXCEPTION_FILTER and only reads what Windows passes.
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
    }
}

unsafe extern "system" fn filter(pointers: *const EXCEPTION_POINTERS) -> i32 {
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    if crate::crash::REPORTING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let Some(prepared) = PREPARED.get() else { return EXCEPTION_CONTINUE_SEARCH };
    // SAFETY: Windows passes valid exception pointers to a top-level filter.
    let record = unsafe { pointers.as_ref().and_then(|p| p.ExceptionRecord.as_ref()) };
    let Some(record) = record else { return EXCEPTION_CONTINUE_SEARCH };
    let code = record.ExceptionCode.0 as u32;
    let pc = record.ExceptionAddress as usize;
    // An access violation's second parameter is the address it touched.
    let fault = if code == 0xC000_0005 && record.NumberParameters >= 2 { record.ExceptionInformation[1] } else { pc };
    let mut text = [0u8; 160];
    let len = format_into(&mut text, code, fault, pc);
    // SAFETY: the path is a valid wide string; the handle is closed below.
    unsafe {
        if let Ok(file) = CreateFileW(&prepared.path, GENERIC_WRITE.0, FILE_SHARE_NONE, None, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, None) {
            let _ = WriteFile(file, Some(&prepared.header), None, None);
            let _ = WriteFile(file, Some(&text[..len]), None, None);
            let _ = CloseHandle(file);
        }
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// `signal=…\nfault=…\npc=…\ntime=…\n` without allocating.
fn format_into(out: &mut [u8], code: u32, fault: usize, pc: usize) -> usize {
    use std::io::Write as _;
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut cursor = std::io::Cursor::new(out);
    let _ = write!(cursor, "signal=0x{code:x}\nfault=0x{fault:x}\npc=0x{pc:x}\ntime={time}\n");
    cursor.position() as usize
}

/// Creates (or replaces) the shortcut `link` to `target` carrying `app_id` as its
/// `System.AppUserModel.ID`, so the taskbar groups and pins the running window (which sets the
/// same id at start) with it. The installer runs `decibell.exe --create-shortcut <link>`.
pub fn create_shortcut(link: &Path, target: &Path, app_id: &str, description: &str) -> windows::core::Result<()> {
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantClear};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, IPersistFile};
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{Interface as _, PWSTR};

    // SAFETY: COM is initialised on this thread before any COM call; every pointer passed lives
    // across its call; the PROPVARIANT's string is CoTaskMemAlloc'd, as PropVariantClear frees it.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        shell_link.SetPath(&HSTRING::from(target.as_os_str()))?;
        if let Some(dir) = target.parent() {
            shell_link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        shell_link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
        shell_link.SetDescription(&HSTRING::from(description))?;

        let wide: Vec<u16> = app_id.encode_utf16().chain(std::iter::once(0)).collect();
        let buffer = CoTaskMemAlloc(wide.len() * 2) as *mut u16;
        if buffer.is_null() {
            return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_OUTOFMEMORY));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), buffer, wide.len());
        let mut value = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_LPWSTR,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 { pwszVal: PWSTR(buffer) },
                }),
            },
        };
        let store: IPropertyStore = shell_link.cast()?;
        let set = store.SetValue(&PKEY_AppUserModel_ID, &value).and_then(|_| store.Commit());
        let _ = PropVariantClear(&mut value);
        set?;
        let file: IPersistFile = shell_link.cast()?;
        file.Save(&HSTRING::from(link.as_os_str()), true)?;
    }
    Ok(())
}

/// Sets this process's AppUserModelID (the same id as the Start menu shortcut's).
pub fn set_app_user_model_id(app_id: &str) {
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    // SAFETY: a valid, NUL-terminated wide string for the call's duration.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(app_id)) } {
        log::warn!("[install] AppUserModelID: {e}");
    }
}
