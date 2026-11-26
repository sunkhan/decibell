//! Writes the people fixture's scenes as JSON for the Electron capture pages:
//! `decibell-home-fixture <dir>` → `<dir>/home-<screen>.json`.

fn main() -> std::process::ExitCode {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: decibell-home-fixture <dir>");
        return std::process::ExitCode::from(2);
    };
    let dir = std::path::PathBuf::from(dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("{}: {e}", dir.display());
        return std::process::ExitCode::FAILURE;
    }
    use decibell_home::fixture::Scene;
    // The nickname and ban dialogs have no twin in the renderer's user menu (they are Server
    // Settings → Members' there): no page.
    for scene in Scene::ALL.into_iter().filter(|s| !matches!(s, Scene::NicknameModal | Scene::BanModal)) {
        let path = dir.join(format!("home-{}.json", scene.name()));
        let text = serde_json::to_string_pretty(&decibell_home::fixture::json(scene)).expect("JSON");
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("{}: {e}", path.display());
            return std::process::ExitCode::FAILURE;
        }
        println!("{}", path.display());
    }
    std::process::ExitCode::SUCCESS
}
