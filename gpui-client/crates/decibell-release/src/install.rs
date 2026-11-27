//! What this install is: the executable's directory, its `resources/` (the files a release bakes
//! in: `install.json`, `sentry.json`, `gifs.json`; Electron's `process.resourcesPath`), and the
//! package kind the packaging step stamped into `resources/install.json`.
//!
//! "Packaged" is Electron's `app.isPackaged`: a ship build (feature `ship`) running from a package,
//! i.e. with an install stamp. A development build, or a ship binary run straight from `target/`,
//! is not packaged: no updater, no crash reports.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

/// The package formats the release workflow builds (`gpui-client/packaging/`), as stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `Decibell-<v>-x86_64.AppImage`: updates itself (Electron's Linux self-update format).
    AppImage,
    /// `decibell-<v>-linux-x86_64.tar.gz`, unpacked anywhere.
    Tarball,
    /// The `.deb`: dpkg / apt own the files.
    Deb,
    /// The AUR package (built from the tarball by makepkg): pacman owns the files.
    Aur,
    /// The Windows installer (NSIS, per user): updates itself by running the next installer.
    Nsis,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "appimage" => Kind::AppImage,
            "tarball" => Kind::Tarball,
            "deb" => Kind::Deb,
            "aur" => Kind::Aur,
            "nsis" => Kind::Nsis,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::AppImage => "appimage",
            Kind::Tarball => "tarball",
            Kind::Deb => "deb",
            Kind::Aur => "aur",
            Kind::Nsis => "nsis",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Install {
    /// The directory the running executable is in (symlinks resolved).
    pub dir: PathBuf,
    /// `<dir>/resources`.
    pub resources: PathBuf,
    /// The stamp's kind; `None` outside a package.
    pub kind: Option<Kind>,
}

#[derive(Deserialize)]
struct Stamp {
    kind: String,
    #[serde(default)]
    version: String,
}

/// The running install (read once).
pub fn current() -> &'static Install {
    static INSTALL: OnceLock<Install> = OnceLock::new();
    INSTALL.get_or_init(|| {
        let exe = std::env::current_exe().ok();
        let exe = exe.map(|e| e.canonicalize().unwrap_or(e));
        let dir = exe.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
        read(&dir)
    })
}

/// The install whose executable lives in `dir`.
pub fn read(dir: &Path) -> Install {
    let resources = dir.join("resources");
    let kind = std::fs::read(resources.join("install.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Stamp>(strip_bom(&bytes)).ok())
        .and_then(|stamp| {
            if !stamp.version.is_empty() && stamp.version != crate::VERSION {
                // A binary from one release beside another release's resources: trust neither.
                log::warn!("[install] the stamp says {} but this is {}", stamp.version, crate::VERSION);
                return None;
            }
            Kind::parse(&stamp.kind)
        });
    #[cfg(feature = "test-hooks")]
    let kind = match std::env::var("DECIBELL_TEST_INSTALL_KIND") {
        Ok(value) => Kind::parse(&value),
        Err(_) => kind,
    };
    Install { dir: dir.to_path_buf(), resources, kind }
}

/// Electron's `app.isPackaged`: a shipped build running from a package. Development builds are
/// packaged only when a self-test says so (`test-hooks`: `DECIBELL_TEST_INSTALL_KIND`).
pub fn packaged() -> bool {
    let install = current();
    (cfg!(feature = "ship") || cfg!(feature = "test-hooks")) && install.kind.is_some()
}

/// A file of `resources/` with a UTF-8 BOM taken off (a hand-written file on Windows: PowerShell's
/// `Out-File -Encoding utf8` writes one; `sentry.ts` strips it too).
pub fn resource(name: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(current().resources.join(name)).ok()?;
    Some(strip_bom(&bytes).to_vec())
}

pub fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

/// The operating system as Electron's `process.platform` names it.
pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

/// The CPU as Electron's `process.arch` names it.
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// Electron's `process.getSystemVersion()`: the kernel release on Linux, `major.minor.build` on
/// Windows.
pub fn os_release() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `uname` fills the zeroed struct; `release` is NUL-terminated within it.
        unsafe {
            let mut name: libc::utsname = std::mem::zeroed();
            if libc::uname(&mut name) == 0 {
                return std::ffi::CStr::from_ptr(name.release.as_ptr()).to_string_lossy().into_owned();
            }
        }
        "unknown".into()
    }
    #[cfg(windows)]
    {
        crate::win::os_version().unwrap_or_else(|| "unknown".into())
    }
    #[cfg(not(any(unix, windows)))]
    {
        "unknown".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).kind, None);
        std::fs::create_dir(dir.path().join("resources")).unwrap();
        let stamp = dir.path().join("resources/install.json");
        std::fs::write(&stamp, format!("\u{feff}{{\"kind\":\"appimage\",\"version\":\"{}\"}}", crate::VERSION)).unwrap();
        assert_eq!(read(dir.path()).kind, Some(Kind::AppImage));
        std::fs::write(&stamp, r#"{"kind":"nsis","version":"0.0.0-other"}"#).unwrap();
        assert_eq!(read(dir.path()).kind, None, "another release's stamp");
        std::fs::write(&stamp, r#"{"kind":"zip"}"#).unwrap();
        assert_eq!(read(dir.path()).kind, None);
    }

    #[test]
    fn names_the_platform_as_electron() {
        assert!(["linux", "win32", "darwin"].contains(&platform()) || !platform().is_empty());
        assert!(!os_release().is_empty());
    }
}
