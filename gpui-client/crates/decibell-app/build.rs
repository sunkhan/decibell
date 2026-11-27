//! Refuses to build the client against an FFmpeg without its software codecs: the SVT-AV1
//! encoder (`libsvtav1`, the screen share of a machine with no working hardware encoder) and
//! the software decoders (`h264`, `hevc`, `libdav1d` for AV1: the stream of a machine with no
//! working hardware decoder). The client has no Chromium to fall back to (CLAUDE.md, "Video
//! codecs"), so a build that lacks them is missing a feature, not slower. The same for what
//! plays video and audio attachments (`decibell-media`; its codec matrix is
//! `docs/media-design.md` §2): the demuxers, VP8 / VP9 and the audio decoders.
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

/// What attachment playback opens, in libavcodec.
const MEDIA_CODECS: [(&str, &str); 8] = [
    ("ff_vp8_decoder", "the VP8 decoder"),
    ("ff_vp9_decoder", "the VP9 decoder"),
    ("ff_aac_decoder", "the AAC decoder"),
    ("ff_mp3float_decoder", "the MP3 decoder"),
    ("ff_opus_decoder", "the Opus decoder"),
    ("ff_vorbis_decoder", "the Vorbis decoder"),
    ("ff_flac_decoder", "the FLAC decoder"),
    ("ff_pcm_s16le_decoder", "the PCM decoders"),
];

/// And in libavformat.
const MEDIA_DEMUXERS: [(&str, &str); 7] = [
    ("ff_mov_demuxer", "the MP4 / MOV demuxer"),
    ("ff_matroska_demuxer", "the Matroska / WebM demuxer"),
    ("ff_ogg_demuxer", "the Ogg demuxer"),
    ("ff_mp3_demuxer", "the MP3 demuxer"),
    ("ff_wav_demuxer", "the WAV demuxer"),
    ("ff_flac_demuxer", "the FLAC demuxer"),
    ("ff_aac_demuxer", "the ADTS AAC demuxer"),
];
#[path = "build/winres.rs"]
mod winres;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => linux(),
        Ok("windows") => {
            windows();
            windows_resources();
        }
        _ => {}
    }
}

// ── Windows: the executable's icon, version and manifest ────────────────────────────────────

/// `decibell.exe`'s resources (build/winres.rs): written as a `.res` and handed to the linker for
/// the `decibell` binary only.
fn windows_resources() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let icon = manifest_dir.join("../../../electron-client/resources/icon.ico");
    let manifest = manifest_dir.join("windows/decibell.manifest.xml");
    println!("cargo::rerun-if-changed={}", icon.display());
    println!("cargo::rerun-if-changed={}", manifest.display());
    println!("cargo::rerun-if-changed=build/winres.rs");
    println!("cargo::rerun-if-env-changed=DEP_DECIBELL_RELEASE_VERSION");
    let text = std::env::var("DEP_DECIBELL_RELEASE_VERSION").unwrap_or_else(|_| fail("decibell-release did not say the version".into()));
    let mut numbers = [0u16; 4];
    for (slot, part) in numbers.iter_mut().zip(text.split(['-', '+']).next().unwrap_or("").split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    let ico = std::fs::read(&icon).unwrap_or_else(|e| fail(format!("{}: {e}", icon.display())));
    let manifest = std::fs::read(&manifest).unwrap_or_else(|e| fail(format!("{}: {e}", manifest.display())));
    let res = winres::resource_file(&ico, &manifest, &winres::Version { text: &text, numbers })
        .unwrap_or_else(|e| fail(format!("the Windows resources: {e}")));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("decibell.res");
    std::fs::write(&out, res).unwrap_or_else(|e| fail(format!("{}: {e}", out.display())));
    println!("cargo::rustc-link-arg-bin=decibell={}", out.display());
}

fn fail(message: String) -> ! {
    panic!("{message}");
}

// ── Linux: the static prefix ────────────────────────────────────────────────

fn linux() {
    // Relative relocations packed (DT_RELR, glibc 2.36+; the binary needs 2.39 anyway): the PIE's
    // ~100,000 relative relocations shrink from 2.6 MB of `.rela.dyn` to a few KB, which is less
    // to read and apply at every start.
    println!("cargo::rustc-link-arg-bins=-Wl,-z,pack-relative-relocs");
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
    let mut missing: Vec<&str> = REQUIRED
        .iter()
        .chain(MEDIA_CODECS.iter())
        .filter(|(symbol, _)| !symbols.iter().any(|s| s == symbol))
        .map(|(_, what)| *what)
        .collect();
    // Attachment playback: the demuxers, and the resampler beside them.
    let lib = archive.parent().unwrap_or(Path::new("."));
    let avformat = lib.join("libavformat.a");
    println!("cargo::rerun-if-changed={}", avformat.display());
    match archive_symbols(&avformat) {
        Ok(symbols) => missing.extend(
            MEDIA_DEMUXERS
                .iter()
                .filter(|(symbol, _)| !symbols.iter().any(|s| s == symbol))
                .map(|(_, what)| *what),
        ),
        Err(e) => fail(format!("{}: {e}", avformat.display())),
    }
    if !lib.join("libswresample.a").is_file() {
        missing.push("libswresample");
    }
    if !missing.is_empty() {
        fail(format!(
            "the FFmpeg in {} lacks {}: rebuild it with gpui-client/tools/build-ffmpeg.sh \
             (a machine without a working hardware encoder / decoder would have no screen share / \
             no picture, and attachments of the missing formats would not play; then \
             `cargo clean -p ffmpeg-sys-next` in this target directory)",
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
    // `avformat` and `swresample` are attachment playback's (vcpkg's FFmpeg is not built
    // with --disable-everything: with them it has every demuxer and decoder of the matrix).
    let missing: Vec<&str> = ["svt-av1", "dav1d", "avformat", "swresample"]
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
