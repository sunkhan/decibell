//! Writes the search screens' fixtures as the Electron capture pages replay them
//! (`tools/electron-captures/pages/fixtures/search-*.json`, one per page):
//!
//! ```sh
//! cargo run -p decibell-search --features fixtures --bin decibell-search-fixture -- tools/electron-captures/pages/fixtures
//! ```

use decibell_search::fixture::{Stage, json};

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    for stage in Stage::ALL {
        let path = std::path::Path::new(&dir).join(format!("search-{}.json", stage.page().trim_start_matches("search-")));
        std::fs::write(&path, serde_json::to_vec(&json(stage)).map_err(std::io::Error::other)?)?;
        println!("{}", path.display());
    }
    Ok(())
}
