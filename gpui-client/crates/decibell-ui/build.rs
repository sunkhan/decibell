//! Embeds every SVG under gpui-client/assets/icons: writes `$OUT_DIR/icons.rs`, a table of
//! (asset path, bytes) sorted by path, which `assets.rs` serves to GPUI.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let icons = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
    let icons = icons.canonicalize().expect("gpui-client/assets/icons");
    let mut files = Vec::new();
    collect(&icons, &mut files);
    files.sort();

    let mut table = String::from("pub static ICONS: &[(&str, &[u8])] = &[\n");
    for file in &files {
        let name = file
            .strip_prefix(&icons)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        writeln!(
            table,
            "    (\"icons/{name}\", include_bytes!({:?})),",
            file.to_str().unwrap()
        )
        .unwrap();
    }
    table.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("icons.rs");
    std::fs::write(out, table).unwrap();
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    // A new or removed file changes the directory's mtime.
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "svg") {
            println!("cargo:rerun-if-changed={}", path.display());
            files.push(path);
        }
    }
}
