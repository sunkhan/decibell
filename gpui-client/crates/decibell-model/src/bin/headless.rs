//! `decibell-headless`: the core and the stores with no window, driven by a script on
//! the command line. The tool for end-to-end tests of the model against the local stack
//! (`tests/stack.rs`) and for measuring what the core + model cost by themselves
//! (`tools/perf/measure.sh --pid`). docs/model.md §8.
//!
//! It only ever talks to the local test stack: it refuses to start unless
//! `DECIBELL_CENTRAL` names a loopback address **and** this build honours that variable,
//! and `login` only takes accounts listed in the stack's `accounts.json` (`register` /
//! `login-as` make and use throwaway accounts on that same local central).

#[path = "headless/live.rs"]
mod live;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

// The message steps (M2).
#[path = "headless/chat.rs"]
mod chat;

// The test-only streamer (`stream-file`: a .dbv recording into a voice channel).
#[path = "headless/stream.rs"]
mod stream;

use decibell_model::local_key::{FixedKey, KeychainKey, LocalKey, LocalKeyProvider};
use decibell_model::stores::community::{ChannelKind, Community};
use decibell_model::stores::permissions::perm;
use decibell_model::{CoreOptions, Model, ServerId, actions, core};
use gpui::{App, AsyncApp, SharedString};
use serde_json::{Value, json};

const USAGE: &str = "\
decibell-headless [options] <step>...

Runs the Decibell core and the client's whole state model without a window: the account and
community stores, messages and DMs, voice / people / calls. Against the local test stack only
(DECIBELL_CENTRAL must be a loopback address; tools/dev/local-stack.sh).

Options
  --user-data-dir <dir>   the profile (default: $DECIBELL_USER_DATA_DIR, else the platform's)
  --keychain              keep the E2EE at-rest key in the OS keychain (default: no key,
                          the core's machine-derived one; tests never touch the keychain)
  --timeout <seconds>     how long a `wait` may take (default 20)

Steps run in order, in one process (the boot — start the core, read config.json, sign in
with a saved sign-in — always comes first); any mix of the groups below.

Account and communities:
  login <account>                 password sign-in as an account of the stack's accounts.json
  register <name> <password>      sign up a throwaway account on the local stack (the core's
                                  `register`, email <name>@example.test); waits for central's
                                  answer and fails on a refusal; prints `REGISTERED <name>`
  login-as <name> <password>      password sign-in as any account of the local stack (a
                                  throwaway one `register` made); `wait logged-in` follows
  logout                          Settings → Account's Log Out: hangs up a call first, then signs out
  wait booted | logged-in | logged-out
  wait connected <community>      it answered our authentication
  wait roster <community>         its first roster page is in
  wait online <community> <user>  that member shows as online (offline: `wait offline …`)
  open-server <community>         as clicking its tile
  open-channel <channel>          in the open community (navigation only: `history` loads it)
  idle <seconds>                  do nothing (measure it from outside)
  print <text>                    one line `MARK <text>` (a test's cue)
  wait-file <path>                until that file exists (a cue from a test)";

const DUMPS: &str = "\
Dumps (each prints one line `DUMP <what> <json>`):
  dump all                        every dump below that applies now: the account stores, the open
                                  community's channels / roles / roster and the open channel's
                                  permissions and messages, the open DM, search, every live store
  dump auth | users | servers | connections | nav | settings | layout | toasts | pump | core
  dump channels <community> | roles <community> | roster <community>
  dump permissions <community> <channel>
  dump chat <channel> | dm <user> | search
  dump voice | presence | own | friends | e2ee | call | transfers | devices | hotkeys | cues | streambot
  dump codecs                     the core's encoder probe, run now (each codec's backend, FFmpeg
                                  encoder and limits; DECIBELL_ENCODER leaves one backend), and the
                                  codec caps the model holds (the `codecs` step loads them); not
                                  part of `dump all`

Example: one client through both halves (perf3 has E2EE keys on the stack: voice needs them)
  decibell-headless --user-data-dir /tmp/p login perf3 wait connected \"Daily Gaming\" \\
      open-server \"Daily Gaming\" e2ee-ready decibell-m3-test-passphrase open-channel general \\
      history general send off-topic hello wait-sent off-topic deafen voice-join Lounge \\
      wait mls ready status dnd watch-roster \"Daily Gaming\" 100 dump all \\
      voice-leave status online delete off-topic last wait-deleted off-topic logout wait logged-out

Exit status: 0 when every step ran, 1 when one failed or timed out, 2 for bad usage or an
unsafe environment.";

fn usage() -> String {
    format!("{USAGE}\n\n{}\n\n{}\n\n{}\n\n{DUMPS}", chat::USAGE, live::USAGE, stream::USAGE)
}

#[derive(Debug, Clone)]
enum Step {
    Login(String),
    Register { name: String, password: String },
    LoginAs { name: String, password: String },
    Logout,
    Wait(Wait),
    OpenServer(String),
    OpenChannel(String),
    Dump(Vec<String>),
    Idle(f64),
    Print(String),
    WaitFile(PathBuf),
    Chat(chat::ChatStep),
    Live(live::LiveStep),
    Stream(stream::StreamStep),
}

#[derive(Debug, Clone)]
enum Wait {
    Booted,
    LoggedIn,
    LoggedOut,
    Connected(String),
    Roster(String),
    Presence { community: String, user: String, online: bool },
    Live(live::LiveWait),
}

struct Args {
    user_data_dir: Option<PathBuf>,
    keychain: bool,
    timeout: Duration,
    steps: Vec<Step>,
}

fn parse(args: Vec<String>) -> Result<Args, String> {
    let mut out = Args { user_data_dir: None, keychain: false, timeout: Duration::from_secs(20), steps: Vec::new() };
    let mut it = args.into_iter();
    let next = |what: &str, it: &mut std::vec::IntoIter<String>| it.next().ok_or(format!("{what}: missing argument"));
    while let Some(arg) = it.next() {
        let step = match arg.as_str() {
            "--user-data-dir" => {
                out.user_data_dir = Some(next(&arg, &mut it)?.into());
                continue;
            }
            "--keychain" => {
                out.keychain = true;
                continue;
            }
            "--timeout" => {
                let secs: f64 = next(&arg, &mut it)?.parse().map_err(|_| "--timeout: not a number")?;
                out.timeout = Duration::from_secs_f64(secs);
                continue;
            }
            "login" => Step::Login(next(&arg, &mut it)?),
            "register" => Step::Register { name: next(&arg, &mut it)?, password: next(&arg, &mut it)? },
            "login-as" => Step::LoginAs { name: next(&arg, &mut it)?, password: next(&arg, &mut it)? },
            "logout" => Step::Logout,
            "open-server" => Step::OpenServer(next(&arg, &mut it)?),
            "open-channel" => Step::OpenChannel(next(&arg, &mut it)?),
            "idle" => Step::Idle(next(&arg, &mut it)?.parse().map_err(|_| "idle: not a number")?),
            "print" => Step::Print(next(&arg, &mut it)?),
            "wait-file" => Step::WaitFile(next(&arg, &mut it)?.into()),
            "wait" => {
                let what = next(&arg, &mut it)?;
                Step::Wait(match what.as_str() {
                    "booted" => Wait::Booted,
                    "logged-in" => Wait::LoggedIn,
                    "logged-out" => Wait::LoggedOut,
                    "connected" => Wait::Connected(next("wait connected", &mut it)?),
                    "roster" => Wait::Roster(next("wait roster", &mut it)?),
                    "online" | "offline" => Wait::Presence {
                        community: next("wait online", &mut it)?,
                        user: next("wait online", &mut it)?,
                        online: what == "online",
                    },
                    other => match live::parse_wait(other, &mut it)? {
                        Some(w) => Wait::Live(w),
                        None => return Err(format!("wait: unknown condition {other:?}")),
                    },
                })
            }
            "dump" => {
                let what = next(&arg, &mut it)?;
                let extra = match what.as_str() {
                    "channels" | "roles" | "roster" | "chat" | "dm" => 1,
                    "permissions" => 2,
                    _ => 0,
                };
                let mut parts = vec![what];
                for _ in 0..extra {
                    parts.push(next("dump", &mut it)?);
                }
                Step::Dump(parts)
            }
            cmd if chat::is_step(cmd) => Step::Chat(chat::parse(cmd, &mut it)?),
            other => match stream::parse(other, &mut it)? {
                Some(step) => Step::Stream(step),
                None => match live::parse(other, &mut it)? {
                    Some(step) => Step::Live(step),
                    None => return Err(format!("unknown step or option {other:?}")),
                },
            },
        };
        out.steps.push(step);
    }
    Ok(out)
}

// ── Safety ──────────────────────────────────────────────────────────────────

/// The loopback central this run talks to, or why it must not run.
fn local_central() -> Result<String, String> {
    if !cfg!(any(debug_assertions, feature = "local-central")) {
        return Err("this build ignores DECIBELL_CENTRAL (a release build without the `local-central` \
                    feature) and would talk to the live central: refusing to run"
            .into());
    }
    let value = std::env::var("DECIBELL_CENTRAL").map_err(|_| {
        "DECIBELL_CENTRAL is not set: refusing to run against the live central \
         (eval \"$(tools/dev/local-stack.sh client-env)\")"
            .to_string()
    })?;
    let (host, port) = value.rsplit_once(':').ok_or("DECIBELL_CENTRAL must be host:port")?;
    port.parse::<u16>().map_err(|_| "DECIBELL_CENTRAL: bad port")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !loopback {
        return Err(format!("DECIBELL_CENTRAL={value} is not a loopback address: refusing to run"));
    }
    Ok(value)
}

/// The password of a stack account, from the stack's `accounts.json`. Accounts that are
/// not listed there are refused.
fn stack_password(account: &str) -> Result<String, String> {
    let dev_dir = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/decibell-dev")))
        .ok_or("no DECIBELL_DEV_DIR and no HOME")?;
    let path = dev_dir.join("run/accounts.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let accounts: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    accounts["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == account)
        .and_then(|a| a["password"].as_str().map(str::to_string))
        .ok_or(format!("{account:?} is not an account of {}", path.display()))
}

// ── Logging ─────────────────────────────────────────────────────────────────

struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "[{}] {}", record.level(), record.args());
    }
    fn flush(&self) {}
}

fn init_logging() {
    let level = match std::env::var("DECIBELL_LOG").as_deref() {
        Ok("trace") => log::LevelFilter::Trace,
        Ok("debug") => log::LevelFilter::Debug,
        Ok("info") => log::LevelFilter::Info,
        Ok("off") => log::LevelFilter::Off,
        _ => log::LevelFilter::Warn,
    };
    let _ = log::set_logger(&StderrLog);
    log::set_max_level(level);
}

// ── Running the script ──────────────────────────────────────────────────────

pub(crate) fn say(line: String) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn community_named(model: &Model, name: &str, cx: &App) -> Option<(ServerId, Option<Community>)> {
    let id = model.servers.read(cx).servers().iter().find(|s| &*s.name == name).map(|s| s.id)?;
    Some((id, model.community(id, cx)))
}

fn holds(wait: &Wait, model: &Model, cx: &App) -> bool {
    match wait {
        Wait::Booted => model.auth.read(cx).booted,
        Wait::LoggedIn => model.auth.read(cx).is_authenticated,
        Wait::LoggedOut => {
            let auth = model.auth.read(cx);
            auth.booted && !auth.is_authenticated && !auth.is_logging_in
        }
        Wait::Connected(name) => community_named(model, name, cx)
            .is_some_and(|(id, community)| community.is_some() && model.connections.read(cx).is_connected(id)),
        Wait::Roster(name) => community_named(model, name, cx)
            .and_then(|(_, community)| community)
            .is_some_and(|c| c.roster.read(cx).meta().is_some_and(|m| !m.refetching)),
        Wait::Presence { community, user, online } => community_named(model, community, cx)
            .and_then(|(_, community)| community)
            .is_some_and(|c| {
                let roster = c.roster.read(cx);
                roster.meta().is_some()
                    && roster.members().values().any(|m| &*m.username == user.as_str() && m.is_online == *online)
            }),
        Wait::Live(w) => live::holds(w, model, cx),
    }
}

/// Wait for a condition on the stores. The 20 ms tick exists only while a `wait` step
/// runs; an `idle` step holds nothing but its own timer.
async fn wait_for(wait: &Wait, model: &Model, timeout: Duration, cx: &mut AsyncApp) -> bool {
    let started = Instant::now();
    loop {
        if cx.update(|cx| holds(wait, model, cx)) {
            return true;
        }
        if started.elapsed() > timeout {
            return false;
        }
        cx.background_executor().timer(Duration::from_millis(20)).await;
    }
}

fn dump(parts: &[String], model: &Model, cx: &App) -> Result<Value, String> {
    let community = |name: &String| {
        community_named(model, name, cx)
            .and_then(|(id, community)| Some((id, community?)))
            .ok_or(format!("no community named {name:?} has answered"))
    };
    let s = |v: &SharedString| v.to_string();
    Ok(match parts[0].as_str() {
        "auth" => {
            let a = model.auth.read(cx);
            json!({
                "authenticated": a.is_authenticated, "username": a.username.as_ref().map(s), "uid": a.uid,
                "renames": a.renames, "devices": a.devices, "loggingIn": a.is_logging_in,
                "loginError": a.login_error.as_ref().map(s), "loginNotice": a.login_notice.as_ref().map(s),
                "savedUsername": a.saved_username.as_ref().map(s), "booted": a.booted,
            })
        }
        "users" => {
            let users = model.users.read(cx);
            let mut names: Vec<_> = users.iter().map(|(uid, name)| (uid, name.to_string())).collect();
            names.sort();
            json!({ "count": users.len(), "names": names.into_iter().map(|(uid, name)| json!([uid, name])).collect::<Vec<_>>() })
        }
        "servers" => {
            let servers = model.servers.read(cx);
            let connections = model.connections.read(cx);
            let row = |sv: &decibell_model::stores::servers::Server| {
                json!({
                    "id": sv.id, "name": s(&sv.name), "host": s(&sv.host), "port": sv.port, "memberCount": sv.member_count,
                    "pictureVersion": servers.picture_version(sv.id), "hasPicture": servers.picture(sv.id).is_some(),
                    "connected": connections.is_connected(sv.id), "pending": connections.is_pending(sv.id),
                })
            };
            json!({
                "servers": servers.servers().iter().map(row).collect::<Vec<_>>(),
                "discoverable": servers.discoverable().iter().map(row).collect::<Vec<_>>(),
            })
        }
        "connections" => {
            let c = model.connections.read(cx);
            let mut connected: Vec<_> = c.connected().collect();
            let mut pending: Vec<_> = c.pending().collect();
            connected.sort();
            pending.sort();
            json!({ "central": c.central, "connected": connected, "pending": pending })
        }
        "nav" => {
            let nav = model.nav.read(cx);
            json!({ "view": nav.active_view, "server": nav.active_server, "channel": nav.active_channel.as_ref().map(s) })
        }
        "settings" => serde_json::to_value(model.settings.read(cx).get()).map_err(|e| e.to_string())?,
        "layout" => serde_json::to_value(model.layout.read(cx).prefs()).map_err(|e| e.to_string())?,
        "toasts" => Value::Array(
            model
                .toasts
                .read(cx)
                .toasts()
                .iter()
                .map(|t| json!({ "severity": t.severity, "title": s(&t.title), "body": t.body.as_ref().map(s) }))
                .collect(),
        ),
        "pump" => serde_json::to_value(model.pump_stats()).map_err(|e| e.to_string())?,
        "core" => json!({
            "running": core::is_running(),
            "userDataDir": core::user_data_dir().map(|d| d.display().to_string()),
            "central": std::env::var("DECIBELL_CENTRAL").ok(),
        }),
        "channels" => {
            let (_, community) = community(&parts[1])?;
            let channels = community.channels.read(cx);
            Value::Array(
                channels
                    .list()
                    .iter()
                    .map(|c| {
                        json!({
                            "id": s(&c.id), "name": s(&c.name), "kind": c.kind, "myPermissions": c.my_permissions,
                            "slowmodeSeconds": c.slowmode_seconds, "encrypted": c.encrypted, "topic": s(&c.topic),
                            "permissionsSynced": c.permissions_synced, "categorySync": c.category_sync,
                            "category": channels.category_of(&c.id).map(|cat| s(&cat.name)),
                        })
                    })
                    .collect(),
            )
        }
        "roles" => {
            let (_, community) = community(&parts[1])?;
            Value::Array(
                community
                    .roles
                    .read(cx)
                    .list()
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.id, "name": s(&r.name), "position": r.position, "permissions": r.permissions,
                            "isDefault": r.is_default, "manageEachOther": r.manage_each_other, "memberCount": r.member_count,
                            "color": r.color,
                        })
                    })
                    .collect(),
            )
        }
        "roster" => {
            let (id, community) = community(&parts[1])?;
            let roster = community.roster.read(cx);
            let visible = roster.visible();
            let row = |uid: &i64| {
                let m = roster.member(*uid)?;
                Some(json!({
                    "uid": m.uid, "username": s(&m.username), "nickname": s(&m.nickname),
                    "display": model.display_name(Some(id), m.uid, &m.username, cx).to_string(),
                    "isOwner": m.is_owner, "roleIds": &*m.role_ids,
                }))
            };
            let meta = roster.meta().map(|m| {
                json!({
                    "revision": m.revision, "totalMembers": m.total_members, "onlineCount": m.online_count,
                    "onlineComplete": m.online_complete, "hasMore": m.has_more, "generation": m.generation,
                    "refetching": m.refetching, "loadingMore": m.loading_more,
                })
            });
            json!({
                "meta": meta, "held": roster.members().len(),
                "owner": community.meta.read(cx).owner,
                "online": visible.online.iter().filter_map(row).collect::<Vec<_>>(),
                "offlineCount": visible.offline.len(),
                "offlineFirst": visible.offline.iter().take(10).filter_map(row).collect::<Vec<_>>(),
            })
        }
        "permissions" => {
            let (id, community) = community(&parts[1])?;
            let channels = community.channels.read(cx);
            let channel = channels.list().iter().find(|c| &*c.name == parts[2].as_str());
            let channel_id = channel.map(|c| c.id.clone());
            let can = |bit: u64| channel_id.as_ref().is_some_and(|c| model.channel_permission(id, c, bit, cx));
            json!({
                "isOwner": model.is_owner(id, cx),
                "visible": channel.is_some(),
                "myPermissions": channel.map(|c| c.my_permissions),
                "channel": {
                    "view": can(perm::VIEW_CHANNEL), "send": can(perm::SEND_MESSAGES), "attach": can(perm::ATTACH_FILES),
                    "readHistory": can(perm::READ_HISTORY), "manageMessages": can(perm::MANAGE_MESSAGES),
                    "manageChannels": can(perm::MANAGE_CHANNELS),
                },
                "server": {
                    "administrator": model.has_permission(id, perm::ADMINISTRATOR, cx),
                    "manageServer": model.has_permission(id, perm::MANAGE_SERVER, cx),
                    "kickMembers": model.has_permission(id, perm::KICK_MEMBERS, cx),
                    "createInvite": model.has_permission(id, perm::CREATE_INVITE, cx),
                    "changeNickname": model.has_permission(id, perm::CHANGE_NICKNAME, cx),
                    "sendMessages": model.has_permission(id, perm::SEND_MESSAGES, cx),
                },
            })
        }
        "chat" | "dm" | "search" => chat::dump(parts, model, cx)?,
        "streambot" => stream::dump(),
        other => return live::dump(other, model, cx).ok_or(format!("dump: unknown store {other:?}")),
    })
}

/// `dump all`: every dump that applies in the current state, one `(what, json)` each; one
/// that can't be made (a community that hasn't answered) carries `{"error": …}` instead.
fn dump_all(model: &Model, cx: &App) -> Vec<(String, Value)> {
    let mut what: Vec<Vec<String>> = ["auth", "users", "servers", "connections", "nav", "settings", "layout", "toasts", "pump", "core"]
        .iter()
        .map(|w| vec![w.to_string()])
        .collect();
    let nav = model.nav.read(cx);
    if let Some(server) = nav.active_server
        && let Some(name) = model.servers.read(cx).get(server).map(|s| s.name.to_string())
    {
        for w in ["channels", "roles", "roster"] {
            what.push(vec![w.into(), name.clone()]);
        }
        let channel = nav.active_channel.as_ref().and_then(|id| {
            let community = model.community(server, cx)?;
            let channels = community.channels.read(cx);
            channels.get(id).map(|c| (c.name.to_string(), c.kind))
        });
        if let Some((channel, kind)) = channel {
            what.push(vec!["permissions".into(), name.clone(), channel.clone()]);
            if kind == ChannelKind::Text {
                what.push(vec!["chat".into(), channel]);
            }
        }
    }
    if let Some(peer) = model.dm.list.read(cx).active_peer {
        what.push(vec!["dm".into(), model.name_of(peer, &SharedString::default(), cx).to_string()]);
    }
    what.push(vec!["search".into()]);
    for w in ["voice", "presence", "own", "friends", "e2ee", "call", "transfers", "devices", "hotkeys", "cues"] {
        what.push(vec![w.into()]);
    }
    what.into_iter()
        .map(|parts| {
            let value = dump(&parts, model, cx).unwrap_or_else(|e| json!({ "error": e }));
            (parts.join(" "), value)
        })
        .collect()
}

async fn run_step(step: &Step, model: &Model, timeout: Duration, cx: &mut AsyncApp) -> Result<(), String> {
    match step {
        Step::Login(account) => {
            let password = stack_password(account)?;
            cx.update(|cx| actions::auth::sign_in(account.clone(), password, cx));
        }
        Step::Register { name, password } => {
            // A fresh answer: an earlier one (another step's) doesn't count.
            cx.update(|cx| model.auth.update(cx, |a, cx| a.set_register_result(None, cx)));
            let email = format!("{name}@example.test");
            cx.update(|cx| actions::auth::register(name.clone(), email, password.clone(), cx));
            let started = Instant::now();
            let result = loop {
                if let Some(result) = cx.update(|cx| model.auth.read(cx).register_result.clone()) {
                    break result;
                }
                if started.elapsed() > timeout {
                    return Err(format!("register {name}: no answer within {timeout:?}"));
                }
                cx.background_executor().timer(Duration::from_millis(20)).await;
            };
            if !result.success {
                return Err(format!("register {name}: {}", result.message));
            }
            say(format!("REGISTERED {name}"));
        }
        // Any account of the local stack (central is loopback: checked before anything ran).
        Step::LoginAs { name, password } => cx.update(|cx| actions::auth::sign_in(name.clone(), password.clone(), cx)),
        Step::Logout => cx.update(actions::auth::log_out),
        Step::Wait(wait) => {
            if !wait_for(wait, model, timeout, cx).await {
                let auth = cx.update(|cx| dump(&["auth".to_string()], model, cx));
                return Err(format!("timed out after {timeout:?} waiting for {wait:?}; auth = {auth:?}"));
            }
        }
        Step::OpenServer(name) => cx.update(|cx| match community_named(model, name, cx) {
            Some((id, _)) => {
                actions::servers::open_server(id, cx);
                Ok(())
            }
            None => Err(format!("open-server: no community named {name:?}")),
        })?,
        Step::OpenChannel(name) => cx.update(|cx| {
            let server = model.nav.read(cx).active_server.ok_or("open-channel: no community is open")?;
            let community = model.community(server, cx).ok_or("open-channel: that community has not answered")?;
            let id = community
                .channels
                .read(cx)
                .list()
                .iter()
                .find(|c| &*c.name == name.as_str() && c.kind != ChannelKind::Category)
                .map(|c| c.id.clone())
                .ok_or(format!("open-channel: no channel named {name:?}"))?;
            model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(id), cx));
            Ok::<_, String>(())
        })?,
        Step::Dump(parts) if parts[0] == "codecs" => live::dump_codecs(model, cx).await?,
        Step::Dump(parts) if parts[0] == "all" => {
            for (what, value) in cx.update(|cx| dump_all(model, cx)) {
                say(format!("DUMP {what} {value}"));
            }
        }
        Step::Dump(parts) => {
            let value = cx.update(|cx| dump(parts, model, cx))?;
            say(format!("DUMP {} {value}", parts.join(" ")));
        }
        Step::Idle(seconds) => {
            say(format!("IDLE {seconds} pid={}", std::process::id()));
            cx.background_executor().timer(Duration::from_secs_f64(*seconds)).await;
        }
        Step::Print(text) => say(format!("MARK {text}")),
        Step::Chat(step) => chat::run(step, model, timeout, cx).await?,
        Step::Live(step) => live::run(step, model, timeout, cx).await?,
        Step::Stream(step) => stream::run(step, model, cx).await?,
        Step::WaitFile(path) => {
            let started = Instant::now();
            while !path.exists() {
                if started.elapsed() > timeout {
                    return Err(format!("timed out waiting for {}", path.display()));
                }
                cx.background_executor().timer(Duration::from_millis(50)).await;
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    let args = match parse(raw) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("decibell-headless: {e}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    let central = match local_central() {
        Ok(central) => central,
        Err(e) => {
            eprintln!("decibell-headless: {e}");
            return ExitCode::from(2);
        }
    };
    init_logging();
    say(format!("CENTRAL {central}"));

    let exit = Arc::new(AtomicU8::new(0));
    // The platform with no windowing.
    gpui::Application::with_platform(gpui_platform::current_platform(true)).run({
        let exit = exit.clone();
        move |cx| {
            let user_data_dir = args.user_data_dir.clone().or_else(|| core::default_user_data_dir().ok());
            let model = Model::init(cx, user_data_dir.clone());
            live::install(&model, cx);
            let local_key: Box<dyn LocalKeyProvider> =
                if args.keychain { Box::new(KeychainKey) } else { Box::new(FixedKey(LocalKey::none())) };
            let started = Model::start_core(
                cx,
                CoreOptions { user_data_dir, app_version: env!("CARGO_PKG_VERSION").to_string(), local_key },
            );
            cx.spawn(async move |cx| {
                let outcome: Result<(), String> = async {
                    started.await.map_err(|e| format!("starting the core: {e:#}"))?;
                    for step in &args.steps {
                        run_step(step, &model, args.timeout, cx).await.map_err(|e| format!("{step:?}: {e}"))?;
                    }
                    Ok(())
                }
                .await;
                // A stream from a file ends with the script (announced as ended).
                stream::stop(&model, cx).await;
                match outcome {
                    Ok(()) => say("DONE".to_string()),
                    Err(e) => {
                        say(format!("FAILED {e}"));
                        exit.store(1, Ordering::Relaxed);
                    }
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
    // The event loop is gone: stopping the core may block here.
    if !core::shutdown(Duration::from_secs(3)) {
        eprintln!("decibell-headless: the core did not stop within its limit");
    }
    ExitCode::from(exit.load(Ordering::Relaxed))
}
