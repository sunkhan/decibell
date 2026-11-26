//! Writes the voice fixtures as the Electron capture pages replay them:
//! `cargo run -p decibell-voice --features fixtures --bin decibell-voice-fixture -- <dir>` →
//! `<dir>/voice-<screen>.json`, one per voice screen.

use decibell_voice::fixture::{Scene, json};

fn main() -> std::io::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir)?;
    let scenes: &[(&str, Scene)] = &[
        ("voice-stage-1", Scene::Stage { people: 1, streams: 0, watched: 0 }),
        ("voice-stage-4", Scene::Stage { people: 4, streams: 0, watched: 0 }),
        ("voice-stage-9", Scene::Stage { people: 9, streams: 0, watched: 0 }),
        ("voice-stage-25", Scene::Stage { people: 25, streams: 0, watched: 0 }),
        ("voice-stage-live", Scene::Stage { people: 4, streams: 2, watched: 0 }),
        ("voice-stage-stream", Scene::Stage { people: 4, streams: 1, watched: 1 }),
        ("voice-stage-streams", Scene::Stage { people: 9, streams: 2, watched: 2 }),
        ("voice-focus", Scene::Focus),
        ("voice-mini", Scene::Mini),
        ("go-live", Scene::GoLive { preset: "smooth", advanced: false, software: false }),
        ("go-live-sharp", Scene::GoLive { preset: "sharp", advanced: false, software: false }),
        ("go-live-slides", Scene::GoLive { preset: "slides", advanced: false, software: false }),
        ("go-live-saver", Scene::GoLive { preset: "saver", advanced: false, software: false }),
        ("go-live-advanced", Scene::GoLive { preset: "smooth", advanced: true, software: false }),
        ("go-live-software", Scene::GoLive { preset: "slides", advanced: true, software: true }),
        ("call-incoming", Scene::CallIncoming),
        ("call-stage", Scene::CallStage),
    ];
    for (name, scene) in scenes {
        let path = dir.join(format!("voice-{name}.json").replace("voice-voice-", "voice-"));
        std::fs::write(&path, serde_json::to_vec_pretty(&json(*scene, name)).expect("serialisable"))?;
        println!("{}", path.display());
    }
    Ok(())
}
