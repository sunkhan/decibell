//! Writes the shell's fixture as the Electron capture pages replay it:
//! `cargo run -p decibell-shell --features fixtures --bin decibell-shell-fixture -- <dir>` →
//! `<dir>/shell-main-server.json`, `<dir>/shell-main-home.json`.

use decibell_shell::fixture::{Scene, json};

fn main() -> std::io::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir)?;
    for (scene, name) in [(Scene::Server, "shell-main-server.json"), (Scene::Home, "shell-main-home.json")] {
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_vec_pretty(&json(scene)).expect("serialisable"))?;
        println!("{}", path.display());
    }
    Ok(())
}
