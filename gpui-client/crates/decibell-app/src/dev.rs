//! Development and measurement hooks, compiled only with the `dev-local` feature (never in the
//! shipped binary, which also must not honour `DECIBELL_CENTRAL`):
//!
//! - `--login <account>`: sign in to the local test stack without typing. Refused unless
//!   `DECIBELL_CENTRAL` is a loopback `host:port` and the account is one of the stack's
//!   (`$DECIBELL_DEV_DIR/run/accounts.json`); the password comes from `DECIBELL_DEV_PASSWORD`.
//! - Markers on stdout for scripts (`tools/perf/clients/gpui.sh`): `decibell: signed-in <name>`,
//!   `decibell: connected <community>`, `decibell: control <command> ok`.
//! - `DECIBELL_DEV_CONTROL=<fifo>`: one command per line — `home`, `channel <community>\t<channel>`,
//!   `scroll <seconds> <px/s> [<refresh Hz>]`, `minimize`, `quit`, and for the voice measurements
//!   (`tools/perf` watch scenarios): `unlock <passphrase>` (the E2EE keys, as the passphrase dialog
//!   would), `voice <community>\t<channel>` (muted and deafened first, then the sidebar's join),
//!   `watch <uid>` (focus a stream: watch it large), `leave`, and for the release tests
//!   (`packaging/tests/release_e2e.py`) `restart-to-update` (the update chip's Restart),
//!   `update-check` (About's Check now) and
//!   `update-notice-seen` (what opening About does with a failed-start notice) — read by a thread
//!   blocked on the pipe
//!   (no wakeups while nothing comes). `scroll` runs the scroll benchmark on the open channel
//!   (`decibell_chat::bench::run_live`) and prints `decibell: scroll-result <json>` when it ends.

use std::collections::HashSet;
use std::io::BufRead as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{ChannelId, Model, actions};
use futures::StreamExt as _;
use gpui::{AnyWindowHandle, App};

/// `DECIBELL_CENTRAL` names the local stack: a loopback host with a port.
pub fn local_central() -> Result<String> {
    let central = std::env::var("DECIBELL_CENTRAL").context("DECIBELL_CENTRAL is not set (the local stack: 127.0.0.1:18080)")?;
    let (host, port) = central.rsplit_once(':').context("DECIBELL_CENTRAL needs host:port")?;
    port.parse::<u16>().context("DECIBELL_CENTRAL's port")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !loopback {
        bail!("DECIBELL_CENTRAL={central} is not a loopback address: --login signs in to the local test stack only");
    }
    Ok(central)
}

fn accounts_file() -> PathBuf {
    let dev = std::env::var_os("DECIBELL_DEV_DIR").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev")
    });
    dev.join("run/accounts.json")
}

/// The account must be one the stack seeded.
pub fn check_account(account: &str) -> Result<()> {
    let path = accounts_file();
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let known = json["accounts"].as_array().is_some_and(|a| a.iter().any(|x| x["name"].as_str() == Some(account)));
    if !known {
        bail!("{account} is not an account of the local stack ({})", path.display());
    }
    Ok(())
}

/// Validate `--login` before anything starts.
pub fn validate_login(account: &str) -> Result<String> {
    local_central()?;
    check_account(account)?;
    std::env::var("DECIBELL_DEV_PASSWORD").context("--login needs DECIBELL_DEV_PASSWORD")
}

/// Sign in as `account` once the core has booted (unless a saved sign-in already did).
pub fn login_when_booted(account: String, password: String, cx: &mut App) {
    let model = Model::global(cx).clone();
    let mut done = false;
    let mut try_login = move |cx: &mut App| {
        let (booted, idle) = {
            let auth = Model::global(cx).auth.read(cx);
            (auth.booted, !auth.is_authenticated && !auth.is_logging_in)
        };
        if done || !booted {
            return;
        }
        done = true;
        if idle {
            actions::auth::sign_in(account.clone(), password.clone(), cx);
        }
    };
    try_login(cx);
    cx.observe(&model.auth, move |_, cx| try_login(cx)).detach();
}

/// Print the markers scripts wait for.
pub fn print_markers(cx: &mut App) {
    let model = Model::global(cx).clone();
    let mut signed_in = false;
    cx.observe(&model.auth, move |auth, cx| {
        let auth = auth.read(cx);
        if auth.is_authenticated != signed_in {
            signed_in = auth.is_authenticated;
            match &auth.username {
                Some(name) if signed_in => println!("decibell: signed-in {name}"),
                _ => println!("decibell: signed-out"),
            }
        }
    })
    .detach();
    let mut seen: HashSet<i64> = HashSet::new();
    cx.observe(&model.connections, move |connections, cx| {
        let model = Model::global(cx).clone();
        let now: HashSet<i64> = connections.read(cx).connected().map(|s| s.get()).collect();
        for id in now.difference(&seen) {
            let name = model.servers.read(cx).get(decibell_model::ServerId::from_central(*id as i32)).map(|s| s.name.clone());
            println!("decibell: connected {}", name.as_deref().unwrap_or("?"));
        }
        seen = now;
    })
    .detach();
}

/// Serve `DECIBELL_DEV_CONTROL`, if set.
pub fn serve_control(window: AnyWindowHandle, cx: &mut App) {
    let Some(path) = std::env::var_os("DECIBELL_DEV_CONTROL").map(PathBuf::from) else { return };
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
    let spawned = std::thread::Builder::new().name("decibell-devctl".into()).spawn(move || {
        // Each writer's lines, then wait (blocked in open) for the next writer.
        loop {
            let Ok(file) = std::fs::File::open(&path) else { return };
            for line in std::io::BufReader::new(file).lines() {
                let Ok(line) = line else { break };
                if tx.unbounded_send(line).is_err() {
                    return;
                }
            }
        }
    });
    if let Err(e) = spawned {
        log::error!("[dev] control thread: {e}");
        return;
    }
    cx.spawn(async move |cx| {
        while let Some(line) = rx.next().await {
            let ok = cx.update(|cx| run_command(line.trim(), window, cx));
            println!("decibell: control {} {}", line.trim(), if ok { "ok" } else { "failed" });
        }
    })
    .detach();
}

fn run_command(command: &str, window: AnyWindowHandle, cx: &mut App) -> bool {
    let Some(model) = cx.try_global::<Model>().cloned() else { return false };
    let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
    match verb {
        "home" => {
            actions::servers::go_home(cx);
            true
        }
        "channel" => {
            let (server_name, channel_name) = rest.split_once('\t').unwrap_or((rest, "general"));
            let Some(server) = model.servers.read(cx).servers().iter().find(|s| &*s.name == server_name).map(|s| s.id) else {
                return false;
            };
            let Some(community) = model.community(server, cx) else { return false };
            let channel = community.channels.read(cx).list().iter().find(|c| &*c.name == channel_name).map(|c| c.id.clone());
            let Some(channel): Option<ChannelId> = channel else { return false };
            actions::servers::open_server(server, cx);
            model.nav.update(cx, |nav, cx| {
                nav.set_active_channel(Some(channel), cx);
                nav.set_active_view(ActiveView::Server, cx);
            });
            true
        }
        "scroll" => {
            let mut args = rest.split_whitespace().map(str::parse::<f64>);
            let (Some(Ok(seconds)), Some(Ok(speed))) = (args.next(), args.next()) else { return false };
            let refresh_hz = args.next().and_then(Result::ok).filter(|hz| *hz > 0.);
            cx.spawn(async move |cx| {
                let result = decibell_chat::bench::run_live(cx.clone(), seconds, speed, refresh_hz).await;
                let line = result.unwrap_or_else(|| serde_json::json!({ "error": "no chat pane is shown" }));
                println!("decibell: scroll-result {line}");
            })
            .detach();
            true
        }
        "minimize" => window.update(cx, |_, window, _| window.minimize_window()).is_ok(),
        // The update chip's Restart (the release tests: the quit, the relaunch's arguments).
        "restart-to-update" => {
            cx.dispatch_action(&decibell_shell::RestartToUpdate);
            true
        }
        // About's "Check now" / "Try again".
        "update-check" => {
            let hooks = decibell_settings::SettingsHooks::get(cx).update.clone();
            hooks.map(|h| h.check(cx)).is_some()
        }
        // What opening Settings → About does with a failed-start notice (the release tests run
        // signed out, where Settings can't open).
        "update-notice-seen" => {
            let hooks = decibell_settings::SettingsHooks::get(cx).update.clone();
            hooks.map(|h| h.failed_notice_seen(cx)).is_some()
        }
        "unlock" => {
            use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
            let status = model.live.e2ee.read(cx).status;
            let mode = match status {
                E2eeStatus::Ready => return true,
                E2eeStatus::Locked => PassphraseMode::Unlock,
                E2eeStatus::NotSetUp => PassphraseMode::Setup,
                E2eeStatus::Unavailable => return false,
            };
            model.live.e2ee.update(cx, |e, cx| e.open(mode, cx));
            actions::e2ee::submit_passphrase(rest.to_string(), rest.to_string(), cx);
            true
        }
        "voice" => {
            let (server_name, channel_name) = rest.split_once('\t').unwrap_or((rest, "Lounge"));
            let Some(server) = model.servers.read(cx).servers().iter().find(|s| &*s.name == server_name).map(|s| s.id) else {
                return false;
            };
            let Some(community) = model.community(server, cx) else { return false };
            let channel = community.channels.read(cx).list().iter().find(|c| &*c.name == channel_name).map(|c| c.id.clone());
            let Some(channel): Option<ChannelId> = channel else { return false };
            // Nothing is captured or played: muted and deafened before the engine starts.
            actions::voice::set_mute(true, cx);
            actions::voice::set_deafen(true, cx);
            actions::servers::open_server(server, cx);
            actions::voice::click_voice_channel(server, channel, cx);
            true
        }
        "watch" => match rest.trim().parse::<decibell_model::Uid>() {
            Ok(uid) => {
                actions::voice::focus_stream(uid, cx);
                true
            }
            Err(_) => false,
        },
        "leave" => {
            actions::voice::leave_voice_channel(cx);
            true
        }
        // One line `decibell: video-stats <uid> <json>`: the hub's counters of a watched stream
        // (received / decoded / presented / dropped …), for the watch scenarios' rates.
        "video-stats" => match rest.trim().parse::<decibell_model::Uid>() {
            Ok(uid) => {
                let s = decibell_video::VideoHub::global(cx).stream(&uid.to_string()).stats();
                println!(
                    "decibell: video-stats {uid} {}",
                    serde_json::json!({
                        "codec": s.codec.map(|c| c.label()),
                        "path": s.path.map(|p| p.label()),
                        "width": s.width,
                        "height": s.height,
                        "received": s.received,
                        "decoded": s.decoded,
                        "published": s.published,
                        "presented": s.presented,
                        "replaced": s.replaced,
                        "dropped": s.dropped,
                        "errors": s.errors,
                        "keyframeRequests": s.keyframe_requests,
                        "queue": s.queue,
                        "monotonicMs": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
                    })
                );
                true
            }
            Err(_) => false,
        },
        "quit" => {
            cx.quit();
            true
        }
        _ => false,
    }
}
