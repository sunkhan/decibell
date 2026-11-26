//! Writes the screens' fixture as JSON for the Electron capture pages:
//! `cargo run -p decibell-servers --features fixtures --bin decibell-servers-fixture -- <dir>`
//! (`tools/electron-captures/pages/fixtures`): one `servers-<stage>.json` per screen.

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "tools/electron-captures/pages/fixtures".into());
    std::fs::create_dir_all(&dir)?;
    for (name, stage) in decibell_servers::fixture::stages() {
        let path = std::path::Path::new(&dir).join(format!("servers-{name}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&decibell_servers::fixture::json(stage))?)?;
        println!("{}", path.display());
    }
    Ok(())
}
