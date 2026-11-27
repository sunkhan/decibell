//! A Win32 resource file (`.res`) written by hand, for `decibell.exe`: the app icon (from
//! `electron-client/resources/icon.ico`), the version information (from the one version site) and
//! the application manifest (`windows/decibell.manifest.xml`). link.exe (and lld-link) take a
//! `.res` as an input and convert it themselves, so the build needs no resource compiler: nothing
//! beyond the Rust toolchain on the CI runner, and the file can be checked on Linux
//! (`llvm-cvtres` + `llvm-readobj --coff-resources`).
//!
//! Layout (Microsoft's "Resource File Formats"): a 32-byte empty entry, then per resource a header
//! (data size, header size, type, name, data version, memory flags, language, version,
//! characteristics; DWORD-aligned) and the data (DWORD-aligned). GPUI loads the window icon as
//! resource 1 (`gpui_windows::platform::load_icon`), Windows the manifest as resource 1.

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;
/// en-US.
const LANGUAGE: u16 = 0x0409;
/// MOVEABLE | PURE | DISCARDABLE, as rc.exe writes them for icons; PURE | MOVEABLE otherwise.
const ICON_FLAGS: u16 = 0x1010;
const FLAGS: u16 = 0x0030;

pub struct Version<'a> {
    pub text: &'a str,
    pub numbers: [u16; 4],
}

pub fn resource_file(ico: &[u8], manifest: &[u8], version: &Version) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    // The empty first entry every .res starts with.
    entry(&mut out, 0, 0, 0, 0, &[]);
    let images = icon_images(ico)?;
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((images.len() as u16).to_le_bytes());
    for (i, image) in images.iter().enumerate() {
        let id = i as u16 + 1;
        entry(&mut out, RT_ICON, id, ICON_FLAGS, LANGUAGE, image.data);
        // GRPICONDIRENTRY: the ICONDIRENTRY's first 12 bytes, then the resource id.
        group.extend_from_slice(&image.directory[..12]);
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, 1, ICON_FLAGS, LANGUAGE, &group);
    entry(&mut out, RT_VERSION, 1, FLAGS, LANGUAGE, &version_info(version));
    entry(&mut out, RT_MANIFEST, 1, FLAGS, LANGUAGE, manifest);
    Ok(out)
}

fn entry(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, language: u16, data: &[u8]) {
    // Type and name as ordinals (0xFFFF, id): 4 bytes each, so the header is always 32 bytes.
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend(0xFFFFu16.to_le_bytes());
    out.extend(kind.to_le_bytes());
    out.extend(0xFFFFu16.to_le_bytes());
    out.extend(name.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // data version
    out.extend(flags.to_le_bytes());
    out.extend(language.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // version
    out.extend(0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    pad(out);
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

struct IconImage<'a> {
    /// The 16-byte ICONDIRENTRY.
    directory: &'a [u8],
    data: &'a [u8],
}

fn icon_images(ico: &[u8]) -> Result<Vec<IconImage<'_>>, String> {
    let u16_at = |at: usize| ico.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| ico.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if u16_at(0) != Some(0) || u16_at(2) != Some(1) {
        return Err("not an .ico file".into());
    }
    let count = u16_at(4).ok_or("truncated .ico")? as usize;
    (0..count)
        .map(|i| {
            let at = 6 + i * 16;
            let directory = ico.get(at..at + 16).ok_or("truncated .ico directory")?;
            let size = u32_at(at + 8).ok_or("truncated .ico")? as usize;
            let offset = u32_at(at + 12).ok_or("truncated .ico")? as usize;
            let data = ico.get(offset..offset + size).ok_or("an .ico image past the end")?;
            Ok(IconImage { directory, data })
        })
        .collect()
}

/// `VS_VERSIONINFO` with the fixed part, a `StringFileInfo` for en-US / Unicode and the
/// `VarFileInfo` translation.
fn version_info(version: &Version) -> Vec<u8> {
    let [a, b, c, d] = version.numbers.map(u32::from);
    let mut fixed = Vec::new();
    for value in [
        0xFEEF_04BD,       // signature
        0x0001_0000,       // structure version
        (a << 16) | b,     // file version
        (c << 16) | d,
        (a << 16) | b,     // product version
        (c << 16) | d,
        0x3F,              // flags mask
        0,                 // flags
        0x0004_0004,       // VOS_NT_WINDOWS32
        1,                 // VFT_APP
        0,                 // subtype
        0,                 // date
        0,
    ] {
        fixed.extend(u32::to_le_bytes(value));
    }
    let strings = [
        ("CompanyName", "Decibell"),
        ("FileDescription", "Decibell"),
        ("FileVersion", version.text),
        ("InternalName", "decibell"),
        ("LegalCopyright", "Copyright \u{a9} 2026 Decibell"),
        ("OriginalFilename", "decibell.exe"),
        ("ProductName", "Decibell"),
        ("ProductVersion", version.text),
    ];
    let table = node(
        "040904B0",
        Value::None,
        1,
        &strings.iter().map(|(k, v)| node(k, Value::Text(v), 1, &[])).collect::<Vec<_>>(),
    );
    let string_file_info = node("StringFileInfo", Value::None, 1, &[table]);
    let translation = node("Translation", Value::Binary(&[0x09, 0x04, 0xB0, 0x04]), 0, &[]);
    let var_file_info = node("VarFileInfo", Value::None, 1, &[translation]);
    node("VS_VERSION_INFO", Value::Binary(&fixed), 0, &[string_file_info, var_file_info])
}

enum Value<'a> {
    None,
    Binary(&'a [u8]),
    Text(&'a str),
}

/// One version-info block: length, value length (bytes; for text, characters including the NUL),
/// type (1 text, 0 binary), the key in UTF-16 with its NUL, padding, the value, padding, children.
fn node(key: &str, value: Value, kind: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0u8; 6];
    for unit in key.encode_utf16().chain([0]) {
        out.extend(unit.to_le_bytes());
    }
    pad(&mut out);
    let value_length = match value {
        Value::None => 0,
        Value::Binary(bytes) => {
            out.extend_from_slice(bytes);
            bytes.len()
        }
        Value::Text(text) => {
            let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
            for unit in &units {
                out.extend(unit.to_le_bytes());
            }
            units.len()
        }
    };
    for child in children {
        pad(&mut out);
        out.extend_from_slice(child);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out[2..4].copy_from_slice(&(value_length as u16).to_le_bytes());
    out[4..6].copy_from_slice(&kind.to_le_bytes());
    out
}
