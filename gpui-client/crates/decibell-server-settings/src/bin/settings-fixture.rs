//! Writes the settings fixture as the Electron capture pages replay it:
//! `cargo run -p decibell-server-settings --features fixtures --bin decibell-server-settings-fixture -- tools/electron-captures/pages/fixtures`
//! → `settings.json`.

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let path = std::path::Path::new(&dir).join("settings.json");
    let json = decibell_server_settings::fixture::json();
    std::fs::write(&path, serde_json::to_string_pretty(&json).expect("json")).expect("write");
    println!("{}", path.display());
}
