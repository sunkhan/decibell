//! The running executable as Sentry's `debug_meta` describes it, so the instruction addresses of a
//! report can be symbolicated later from the debug file the release workflow keeps
//! (`decibell.debug`, uploaded with `sentry-cli debug-files upload` when the owner sets that up):
//! where the image is loaded, how big it is, and its identity (ELF: the GNU build id; Windows: the
//! PE's timestamp and size, plus the PDB's GUID and age when the build wrote one).

use serde_json::{Value, json};

#[derive(Debug, Clone, Default)]
pub struct Image {
    pub kind: &'static str,
    pub code_file: String,
    pub image_addr: usize,
    pub image_size: usize,
    pub debug_id: Option<String>,
    pub code_id: Option<String>,
}

impl Image {
    pub fn to_json(&self) -> Value {
        let mut image = json!({
            "type": self.kind,
            "code_file": self.code_file,
            "image_addr": format!("0x{:x}", self.image_addr),
            "image_size": self.image_size,
        });
        if let Some(id) = &self.debug_id {
            image["debug_id"] = json!(id);
        }
        if let Some(id) = &self.code_id {
            image["code_id"] = json!(id);
        }
        image
    }

    pub fn contains(&self, address: usize) -> bool {
        address >= self.image_addr && address < self.image_addr + self.image_size
    }
}

/// The executable's name only: its path holds the user's name.
fn file_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "decibell".into())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// Sentry's ELF debug id: the build id's first 16 bytes as a GUID (the first three fields
/// byte-swapped, as `symbolic` reads them).
pub fn elf_debug_id(build_id: &[u8]) -> Option<String> {
    let mut b: [u8; 16] = build_id.get(..16)?.try_into().ok()?;
    if build_id.len() < 16 {
        return None;
    }
    b[..4].reverse();
    b[4..6].reverse();
    b[6..8].reverse();
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

#[cfg(target_os = "linux")]
pub fn main_image() -> Image {
    use std::ffi::c_void;

    struct Found {
        image: Option<Image>,
    }

    unsafe extern "C" fn visit(info: *mut libc::dl_phdr_info, _size: libc::size_t, data: *mut c_void) -> libc::c_int {
        // SAFETY: the loader passes a valid info record; `data` is our `Found`.
        let (info, found) = unsafe { (&*info, &mut *(data as *mut Found)) };
        // The first object the loader lists is the executable itself.
        let bias = info.dlpi_addr as usize;
        let (mut low, mut high) = (usize::MAX, 0usize);
        let mut build_id = None;
        for i in 0..info.dlpi_phnum as usize {
            // SAFETY: `dlpi_phdr` points at `dlpi_phnum` program headers.
            let ph = unsafe { &*info.dlpi_phdr.add(i) };
            match ph.p_type {
                libc::PT_LOAD => {
                    low = low.min(ph.p_vaddr as usize);
                    high = high.max(ph.p_vaddr as usize + ph.p_memsz as usize);
                }
                libc::PT_NOTE if build_id.is_none() => {
                    // SAFETY: the note segment is mapped at bias + vaddr for p_memsz bytes.
                    let notes = unsafe { std::slice::from_raw_parts((bias + ph.p_vaddr as usize) as *const u8, ph.p_memsz as usize) };
                    build_id = gnu_build_id(notes);
                }
                _ => {}
            }
        }
        if low < high {
            found.image = Some(Image {
                kind: "elf",
                code_file: file_name(),
                image_addr: bias + low,
                image_size: high - low,
                debug_id: build_id.as_deref().and_then(elf_debug_id),
                code_id: build_id.map(|id| id.iter().map(|b| format!("{b:02x}")).collect()),
            });
        }
        1 // stop after the executable
    }

    let mut found = Found { image: None };
    // SAFETY: `visit` only reads what the loader hands it and writes into `found`, which outlives
    // the call.
    unsafe { libc::dl_iterate_phdr(Some(visit), &mut found as *mut Found as *mut c_void) };
    found.image.unwrap_or(Image { kind: "elf", code_file: file_name(), ..Image::default() })
}

/// The descriptor of the `NT_GNU_BUILD_ID` note among `notes`.
#[cfg(any(target_os = "linux", test))]
pub fn gnu_build_id(notes: &[u8]) -> Option<Vec<u8>> {
    let mut at = 0;
    let word = |at: usize| -> Option<usize> { Some(u32::from_ne_bytes(notes.get(at..at + 4)?.try_into().ok()?) as usize) };
    let align = |n: usize| (n + 3) & !3;
    while at + 12 <= notes.len() {
        let (namesz, descsz, kind) = (word(at)?, word(at + 4)?, word(at + 8)?);
        let name = notes.get(at + 12..at + 12 + namesz)?;
        let desc_at = at + 12 + align(namesz);
        let desc = notes.get(desc_at..desc_at + descsz)?;
        if kind == 3 && name == b"GNU\0" {
            return Some(desc.to_vec());
        }
        at = desc_at + align(descsz);
    }
    None
}

#[cfg(windows)]
pub fn main_image() -> Image {
    crate::win::main_image(file_name())
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn main_image() -> Image {
    Image { kind: "unknown", code_file: file_name(), ..Image::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_ids_as_sentry_reads_them() {
        let build_id: Vec<u8> = (0u8..20).collect();
        assert_eq!(elf_debug_id(&build_id).unwrap(), "03020100-0504-0706-0809-0a0b0c0d0e0f");
        assert!(elf_debug_id(&[1, 2, 3]).is_none());
        let mut note = Vec::new();
        for (name, kind, desc) in [(&b"XYZ\0"[..], 1u32, &[9u8; 3][..]), (&b"GNU\0"[..], 3u32, &[0xab; 20][..])] {
            note.extend((name.len() as u32).to_ne_bytes());
            note.extend((desc.len() as u32).to_ne_bytes());
            note.extend(kind.to_ne_bytes());
            note.extend(name);
            note.extend(desc);
            while note.len() % 4 != 0 {
                note.push(0);
            }
        }
        assert_eq!(gnu_build_id(&note).unwrap(), vec![0xab; 20]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_this_executable() {
        let image = main_image();
        assert!(image.image_size > 0);
        let here = finds_this_executable as fn() as usize;
        assert!(image.contains(here), "{image:?} should contain {here:x}");
    }
}
