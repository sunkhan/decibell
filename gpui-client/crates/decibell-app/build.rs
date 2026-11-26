//! Refuses to build the client against an FFmpeg without its software codecs: the SVT-AV1
//! encoder (`libsvtav1`, the screen share of a machine with no working hardware encoder) and
//! the software decoders (`h264`, `hevc`, `libdav1d` for AV1: the stream of a machine with no
//! working hardware decoder). The client has no Chromium to fall back to (CLAUDE.md, "Video
//! codecs"), so a build that lacks them is missing a feature, not slower.
//!
//! - Linux: the static prefix pkg-config names (`gpui-client/vendor/ffmpeg`, built by
//!   `tools/build-ffmpeg.sh`): the archive index of its `libavcodec.a` must define the four
//!   codecs (FFmpeg compiles a codec's object only when it is enabled).
//! - Windows: the vcpkg FFmpeg at `VCPKG_ROOT` must be installed with the `svt-av1` and `dav1d`
//!   features (the overlay in `gpui-client/tools/vcpkg-overlay`; vcpkg's FFmpeg always has its
//!   own H.264 / HEVC decoders). An FFmpeg from elsewhere (`FFMPEG_DIR`, no `VCPKG_ROOT`) is
//!   not checked here: a warning says so, and `--selftest encoder-software` checks the binary.
//!
//! The check reads the prefix this build is pointed at. A target directory that linked an
//! older prefix keeps it inside ffmpeg-sys-next's rlib until that crate rebuilds
//! (`cargo clean -p ffmpeg-sys-next`): `decibell --selftest encoder-software` is what proves the
//! binary itself (HANDOFF.md §2).

use std::path::{Path, PathBuf};

/// Symbols of FFmpeg's codec descriptors that must be in libavcodec.
const REQUIRED: [(&str, &str); 4] = [
    ("ff_libsvtav1_encoder", "the SVT-AV1 encoder (libsvtav1)"),
    ("ff_libdav1d_decoder", "the AV1 software decoder (libdav1d)"),
    ("ff_h264_decoder", "the H.264 software decoder (h264)"),
    ("ff_hevc_decoder", "the HEVC software decoder (hevc)"),
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => linux(),
        Ok("windows") => windows(),
        _ => {}
    }
}

fn fail(message: String) -> ! {
    panic!("{message}");
}

// ── Linux: the static prefix ────────────────────────────────────────────────

fn linux() {
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
    let library = match pkg_config::Config::new()
        .statik(true)
        .cargo_metadata(false)
        .env_metadata(false)
        .probe("libavcodec")
    {
        Ok(library) => library,
        Err(e) => fail(format!(
            "libavcodec.pc not found ({e}); build the client's FFmpeg: gpui-client/tools/build-ffmpeg.sh"
        )),
    };
    let Some(archive) = library
        .link_paths
        .iter()
        .map(|dir| dir.join("libavcodec.a"))
        .find(|path| path.is_file())
    else {
        fail(format!(
            "no libavcodec.a in {:?}: the client links FFmpeg statically (gpui-client/tools/build-ffmpeg.sh)",
            library.link_paths
        ))
    };
    println!("cargo::rerun-if-changed={}", archive.display());
    let symbols = match archive_symbols(&archive) {
        Ok(symbols) => symbols,
        Err(e) => fail(format!("{}: {e}", archive.display())),
    };
    let missing: Vec<&str> = REQUIRED
        .iter()
        .filter(|(symbol, _)| !symbols.iter().any(|s| s == symbol))
        .map(|(_, what)| *what)
        .collect();
    if !missing.is_empty() {
        fail(format!(
            "the FFmpeg in {} lacks {}: rebuild it with gpui-client/tools/build-ffmpeg.sh \
             (a machine without a working hardware encoder / decoder would have no screen share / \
             no picture)",
            archive
                .parent()
                .and_then(Path::parent)
                .unwrap_or(&archive)
                .display(),
            missing.join(", ")
        ));
    }
}

/// The global symbols an `ar` archive's index defines (the GNU `/` or `/SYM64/` member, which
/// `ranlib` writes and FFmpeg's build runs).
fn archive_symbols(path: &Path) -> Result<Vec<String>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let body = bytes
        .strip_prefix(b"!<arch>\n")
        .ok_or("not an ar archive")?;
    if body.len() < 60 {
        return Err("empty archive".into());
    }
    let (header, rest) = body.split_at(60);
    let name = std::str::from_utf8(&header[..16])
        .map_err(|_| "bad member header")?
        .trim_end();
    let size: usize = std::str::from_utf8(&header[48..58])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or("bad member size")?;
    let word = match name {
        "/" => 4,
        "/SYM64/" => 8,
        _ => {
            return Err(format!(
                "no symbol index (first member {name:?}): run ranlib on it"
            ));
        }
    };
    let index = rest.get(..size).ok_or("truncated symbol index")?;
    let read = |at: usize| -> Option<usize> {
        let bytes = index.get(at..at + word)?;
        Some(bytes.iter().fold(0usize, |n, b| n << 8 | *b as usize))
    };
    let count = read(0).ok_or("truncated symbol index")?;
    let names = index
        .get(word * (count + 1)..)
        .ok_or("truncated symbol index")?;
    Ok(names
        .split(|b| *b == 0)
        .take(count)
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect())
}

// ── Windows: the vcpkg FFmpeg ───────────────────────────────────────────────

fn windows() {
    for var in ["VCPKG_ROOT", "VCPKGRS_TRIPLET", "FFMPEG_DIR"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    if std::env::var_os("FFMPEG_DIR").is_some() {
        println!(
            "cargo::warning=FFMPEG_DIR is set: its FFmpeg is not checked for SVT-AV1 and dav1d; \
             run `decibell --selftest encoder-software`"
        );
        return;
    }
    let Some(root) = std::env::var_os("VCPKG_ROOT").map(PathBuf::from) else {
        println!(
            "cargo::warning=VCPKG_ROOT is not set: the FFmpeg this links is not checked for \
             SVT-AV1 and dav1d; run `decibell --selftest encoder-software`"
        );
        return;
    };
    let triplet = std::env::var("VCPKGRS_TRIPLET").unwrap_or_else(|_| "x64-windows".into());
    let database = root.join("installed").join("vcpkg");
    let features = match installed_features(&database, "ffmpeg", &triplet) {
        Ok(Some(features)) => features,
        Ok(None) => fail(format!(
            "vcpkg at {} has no ffmpeg:{triplet}: install it with the client's overlay \
             (gpui-client/HANDOFF.md §2, Windows)",
            root.display()
        )),
        Err(e) => fail(format!(
            "reading vcpkg's database in {}: {e}",
            database.display()
        )),
    };
    let missing: Vec<&str> = ["svt-av1", "dav1d"]
        .into_iter()
        .filter(|f| !features.iter().any(|have| have == f))
        .collect();
    if !missing.is_empty() {
        fail(format!(
            "vcpkg's ffmpeg:{triplet} in {} lacks the feature(s) {}: the client needs SVT-AV1 (the \
             software encoder) and dav1d (the AV1 software decoder). Install \
             ffmpeg[nvcodec,amf,qsv,dav1d,svt-av1] with --overlay-ports=gpui-client/tools/vcpkg-overlay \
             (gpui-client/HANDOFF.md §2, Windows)",
            root.display(),
            missing.join(", ")
        ));
    }
}

/// The installed features of `package` for `triplet` in vcpkg's database (`status`, then the
/// `updates/` it has not folded in yet, in order; a later paragraph replaces an earlier one).
/// `None`: the package is not installed.
fn installed_features(
    database: &Path,
    package: &str,
    triplet: &str,
) -> Result<Option<Vec<String>>, String> {
    let mut files = vec![database.join("status")];
    println!(
        "cargo::rerun-if-changed={}",
        database.join("updates").display()
    );
    if let Ok(entries) = std::fs::read_dir(database.join("updates")) {
        let mut updates: Vec<(u64, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, e.path())))
            .collect();
        updates.sort();
        files.extend(updates.into_iter().map(|(_, path)| path));
    }
    // (feature, installed); the core package is the feature "".
    let mut state: Vec<(String, bool)> = Vec::new();
    for file in &files {
        println!("cargo::rerun-if-changed={}", file.display());
        // vcpkg may hold everything in updates/ and no status file yet.
        if !file.exists() {
            continue;
        }
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        for paragraph in text.replace("\r\n", "\n").split("\n\n") {
            let field = |name: &str| {
                paragraph
                    .lines()
                    .find_map(|line| line.strip_prefix(name)?.strip_prefix(':').map(str::trim))
            };
            if field("Package") != Some(package) || field("Architecture") != Some(triplet) {
                continue;
            }
            let feature = field("Feature").unwrap_or("").to_string();
            let installed = field("Status") == Some("install ok installed");
            state.retain(|(f, _)| *f != feature);
            state.push((feature, installed));
        }
    }
    if !state
        .iter()
        .any(|(f, installed)| f.is_empty() && *installed)
    {
        return Ok(None);
    }
    Ok(Some(
        state
            .into_iter()
            .filter(|(f, i)| *i && !f.is_empty())
            .map(|(f, _)| f)
            .collect(),
    ))
}
