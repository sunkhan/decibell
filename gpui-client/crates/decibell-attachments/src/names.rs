//! File names that came from another user (`downloads.ts` `sanitizeFilename` / `uniquePath`)
//! and the "can run code" check the Transfers panel asks before opening (`format.ts`
//! `isRiskyFile`).

use std::path::{Path, PathBuf};

pub(crate) const PART: &str = ".part";
const MAX_NAME_BYTES: usize = 200;

fn is_windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Node's `path.extname`: from the last `.` of the name, unless that dot is its first character.
pub(crate) fn extname(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i..],
    }
}

/// Make a sender's file name a plain, portable one: path separators, characters Windows
/// refuses and control characters become `_`; no leading dots or spaces (no hidden files, no
/// `..`), no trailing dots or spaces (Windows drops them); Windows device names get a `_`
/// prefix; at most 200 bytes, keeping a short extension; `attachment` when nothing is left.
/// (Electron also NFC-normalised first; names reach this crate as the sender's client wrote
/// them, which is NFC on every platform but macOS.)
pub fn sanitize_filename(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 || c as u32 == 0x7f => '_',
            c => c,
        })
        .collect();
    let trimmed = replaced.trim();
    let trimmed = trimmed.trim_start_matches(|c: char| c == '.' || c.is_whitespace());
    let trimmed = trimmed.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let mut name = trimmed.to_string();
    if is_windows_reserved(&name) {
        name = format!("_{name}");
    }
    if name.len() > MAX_NAME_BYTES {
        let ext = extname(&name);
        let keep_ext = if ext.len() > 1 && ext.len() <= 20 { ext.to_string() } else { String::new() };
        let mut base: String = name[..name.len() - keep_ext.len()].to_string();
        while base.len() + keep_ext.len() > MAX_NAME_BYTES {
            base.pop();
        }
        let base = base.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
        name = format!("{base}{keep_ext}");
    }
    if name.is_empty() {
        "attachment".into()
    } else {
        name
    }
}

/// `name`, `name (1).ext`, … in `dir`: the first that is free on disk and has no `.part` of its
/// own, shortened so the whole path stays within Windows' `MAX_PATH`.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let ext = extname(name).to_string();
    let mut base: String = name[..name.len() - ext.len()].to_string();
    let dir_len = dir.to_string_lossy().chars().count() as i64;
    let room = 240 - dir_len - ext.chars().count() as i64 - PART.len() as i64 - 8;
    if (base.chars().count() as i64) > room {
        let keep = room.max(8) as usize;
        base = base.chars().take(keep).collect();
    }
    for n in 0..10_000u32 {
        let candidate = if n == 0 { dir.join(format!("{base}{ext}")) } else { dir.join(format!("{base} ({n}){ext}")) };
        if !candidate.exists() && !with_part(&candidate).exists() {
            return candidate;
        }
    }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    dir.join(format!("{base} ({ms}){ext}"))
}

/// `<path>.part`.
pub fn with_part(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(PART);
    PathBuf::from(s)
}

const RISKY: &[&str] = &[
    "exe", "msi", "msix", "appx", "bat", "cmd", "com", "scr", "pif", "cpl", "ps1", "psm1", "vbs", "vbe", "js", "jse",
    "wsf", "wsh", "hta", "lnk", "reg", "jar", "sh", "bash", "zsh", "command", "app", "dmg", "pkg", "run", "bin",
    "appimage", "deb", "rpm", "desktop", "py", "pl", "rb",
];

/// Files that can run code when opened: opening one from the Transfers panel asks first.
pub fn is_risky_file(filename: &str) -> bool {
    match filename.rfind('.') {
        Some(i) => RISKY.contains(&filename[i + 1..].to_ascii_lowercase().as_str()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_table() {
        let cases = [
            ("../x", "_x"),
            ("..\\..\\boot.ini", "_.._boot.ini"),
            ("CON.txt", "_CON.txt"),
            ("con", "_con"),
            ("lpt9.log", "_lpt9.log"),
            ("com10.txt", "com10.txt"),
            ("report.pdf. . ", "report.pdf"),
            ("  .hidden", "hidden"),
            ("a:b*c?d\"e<f>g|h", "a_b_c_d_e_f_g_h"),
            ("tab\there", "tab_here"),
            ("", "attachment"),
            ("...", "attachment"),
            ("ünïcødé 名前.png", "ünïcødé 名前.png"),
        ];
        for (raw, want) in cases {
            assert_eq!(sanitize_filename(raw), want, "{raw:?}");
        }
        let long = format!("{}.tar.gz", "é".repeat(300));
        let s = sanitize_filename(&long);
        assert!(s.len() <= 200 && s.ends_with(".gz"), "{s}");
        let long_ext = format!("x.{}", "e".repeat(40));
        assert!(sanitize_filename(&format!("{}{long_ext}", "y".repeat(300))).len() <= 200);
    }

    #[test]
    fn extname_like_node() {
        assert_eq!(extname("a.tar.gz"), ".gz");
        assert_eq!(extname(".bashrc"), "");
        assert_eq!(extname("name."), ".");
        assert_eq!(extname("noext"), "");
    }

    #[test]
    fn unique_and_risky() {
        let dir = std::env::temp_dir().join(format!("dba-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "a.txt");
        assert_eq!(first, dir.join("a.txt"));
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(with_part(&dir.join("a (1).txt")), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(is_risky_file("setup.EXE"));
        assert!(!is_risky_file("photo.jpg"));
        assert!(!is_risky_file("exe"));
    }
}
