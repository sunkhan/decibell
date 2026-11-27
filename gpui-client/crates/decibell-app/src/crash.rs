//! Crash reports (`decibell_release::crash`) in the app: started right after the profile is
//! claimed (Electron starts Sentry before `whenReady`), and told the names a report must never
//! carry as the model learns them (the signed-in account, the communities joined).

use std::path::Path;

use decibell_model::Model;
use gpui::App;

/// Electron's three gates, then the hook and the handlers (nothing at all when a gate is shut).
pub fn start(profile: &Path) {
    if let Some(config) = decibell_release::crash::gate(profile) {
        decibell_release::crash::init(config);
    }
    // A self-test's child panics on request, after reporting started (development builds).
    #[cfg(feature = "dev")]
    decibell_release::crash::panic_for_test();
}

/// Keeps the scrubber's name list current: the account's name and every community's.
pub fn watch_names(model: &Model, cx: &mut App) {
    if !decibell_release::crash::active() {
        return;
    }
    let (auth, servers) = (model.auth.clone(), model.servers.clone());
    let update = {
        let (auth, servers) = (auth.clone(), servers.clone());
        move |cx: &mut App| {
            let mut names: Vec<String> = auth.read(cx).username.iter().map(|n| n.to_string()).collect();
            names.extend(servers.read(cx).servers().iter().map(|s| s.name.to_string()));
            decibell_release::crash::set_names(names);
        }
    };
    let on_auth = update.clone();
    cx.observe(&auth, move |_, cx| on_auth(cx)).detach();
    let on_servers = update.clone();
    cx.observe(&servers, move |_, cx| on_servers(cx)).detach();
    update(cx);
}
