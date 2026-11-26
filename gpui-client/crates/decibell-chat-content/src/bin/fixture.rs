//! Writes the content fixtures as the Electron capture pages replay them, and the pictures they
//! show:
//!
//! ```sh
//! cargo run -p decibell-chat-content --features fixtures --bin decibell-content-fixture -- \
//!     tools/electron-captures/pages/fixtures tools/electron-captures/.work/content-files
//! ```
//!
//! `<pages dir>/content-attachments.json`, `content-embeds.json`; `<files dir>/manifest.json` and
//! one file per picture (what `shoot.cjs`'s `decibell-attachment:` protocol and https stub serve).

use decibell_chat_content::fixture::{ATTACHMENTS, EMBEDS, files, json};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let pages = args.next().unwrap_or_else(|| ".".into());
    let pictures = args.next().unwrap_or_else(|| "content-files".into());
    for (name, channel) in [("content-attachments", ATTACHMENTS), ("content-embeds", EMBEDS)] {
        let path = std::path::Path::new(&pages).join(format!("{name}.json"));
        std::fs::write(&path, serde_json::to_vec(&json(channel)).map_err(std::io::Error::other)?)?;
        println!("{}", path.display());
    }
    let dir = std::path::Path::new(&pictures);
    std::fs::create_dir_all(dir)?;
    let (manifest, list) = files();
    for (name, bytes) in &list {
        std::fs::write(dir.join(name), bytes)?;
    }
    std::fs::write(dir.join("manifest.json"), serde_json::to_vec_pretty(&manifest).map_err(std::io::Error::other)?)?;
    println!("{} ({} pictures)", dir.display(), list.len());
    Ok(())
}
