//! Writes the chat fixtures as the Electron capture pages replay them
//! (`tools/electron-captures/pages/fixtures/chat-*.json`):
//!
//! ```sh
//! cargo run -p decibell-chat --features fixtures --bin decibell-chat-fixture -- tools/electron-captures/pages/fixtures
//! ```

use decibell_chat::fixture::{Which, json};

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    for (name, which) in [("chat-fixture", Which::General), ("chat-features", Which::Features)] {
        // `#general`: its three newest pages (150 rows: the most a list holds without trimming).
        let path = std::path::Path::new(&dir).join(format!("{name}.json"));
        std::fs::write(&path, serde_json::to_vec(&json(which, 3)).map_err(std::io::Error::other)?)?;
        println!("{}", path.display());
    }
    Ok(())
}
