//! The stores driven through the real core against the local test stack
//! (`tools/dev/local-stack.sh up`; docs/model.md §9).
//!
//! ```sh
//! eval "$(../tools/dev/local-stack.sh client-env)"     # DECIBELL_CENTRAL=127.0.0.1:18080
//! DECIBELL_STACK=1 cargo test -p decibell-model --test stack -- --nocapture
//! ```
//!
//! Without `DECIBELL_STACK=1` every test here passes without doing anything. With it, a
//! test refuses to run unless `DECIBELL_CENTRAL` is a loopback address (and so does the
//! `decibell-headless` binary it drives, which also only signs in accounts of the
//! stack's `accounts.json`). Each client runs in its own process — the core is one per
//! process — with its own temporary user data directory. Accounts: `perf2`, `perf3` and
//! whichever of `perf4` / `perf1` is offline for Daily Gaming; `dave` (a member) and
//! `bob` (the owner) for Rustaceans, where no `perf*` account is a member.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::Value;

const HEADLESS: &str = env!("CARGO_BIN_EXE_decibell-headless");
const GAMING: &str = "Daily Gaming";

/// Whether to run, after checking the environment is the local stack.
fn stack() -> bool {
    if std::env::var("DECIBELL_STACK").as_deref() != Ok("1") {
        eprintln!("skipped: set DECIBELL_STACK=1 (and DECIBELL_CENTRAL) to run against the local stack");
        return false;
    }
    let central = std::env::var("DECIBELL_CENTRAL").expect("DECIBELL_STACK=1 needs DECIBELL_CENTRAL");
    let host = central.rsplit_once(':').expect("DECIBELL_CENTRAL must be host:port").0;
    let loopback = host == "localhost"
        || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    assert!(loopback, "DECIBELL_CENTRAL={central} is not a loopback address: refusing to run");
    true
}

fn command(profile: &Path, steps: &[&str]) -> Command {
    let mut command = Command::new(HEADLESS);
    command.arg("--user-data-dir").arg(profile).arg("--timeout").arg("30").args(steps);
    command.env("DECIBELL_LOG", "warn").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

/// What one finished run printed.
struct Run {
    dumps: HashMap<String, Value>,
    stdout: String,
    stderr: String,
    ok: bool,
}

impl Run {
    fn dump(&self, what: &str) -> &Value {
        self.dumps.get(what).unwrap_or_else(|| panic!("no `dump {what}` in:\n{}\n{}", self.stdout, self.stderr))
    }
}

fn parse_dump(line: &str) -> Option<(String, Value)> {
    let rest = line.strip_prefix("DUMP ")?;
    let json_at = rest.find(['{', '['])?;
    Some((rest[..json_at].trim().to_string(), serde_json::from_str(&rest[json_at..]).ok()?))
}

/// Run a script to its end.
fn run(profile: &Path, steps: &[&str]) -> Run {
    let output = command(profile, steps).output().expect("starting decibell-headless");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Run {
        dumps: stdout.lines().filter_map(parse_dump).collect(),
        ok: output.status.success() && stdout.lines().any(|l| l == "DONE"),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        stdout,
    }
}

fn run_ok(profile: &Path, steps: &[&str]) -> Run {
    let run = run(profile, steps);
    assert!(run.ok, "the script failed:\n--- stdout\n{}\n--- stderr\n{}", run.stdout, run.stderr);
    run
}

/// A client that stays up while the test talks to another one.
struct Live {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl Live {
    fn start(profile: &Path, steps: &[&str]) -> Live {
        let mut child = command(profile, steps).stderr(Stdio::inherit()).spawn().expect("starting decibell-headless");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::Builder::new()
            .name("stack-test-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .unwrap();
        Live { child, lines, seen: Vec::new() }
    }

    /// Read until `line` is printed.
    fn until(&mut self, line: &str, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            match self.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(got) => {
                    let found = got == line;
                    assert!(!got.starts_with("FAILED"), "the client failed: {got}\nbefore it: {:#?}", self.seen);
                    self.seen.push(got);
                    if found {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("no {line:?} within {limit:?}; seen: {:#?}", self.seen),
                Err(RecvTimeoutError::Disconnected) => panic!("the client exited before {line:?}; seen: {:#?}", self.seen),
            }
        }
    }

    fn last_dump(&self, what: &str) -> Value {
        self.seen
            .iter()
            .rev()
            .filter_map(|l| parse_dump(l))
            .find(|(name, _)| name == what)
            .unwrap_or_else(|| panic!("no `dump {what}` yet; seen: {:#?}", self.seen))
            .1
    }

    fn finish(mut self) {
        self.until("DONE", Duration::from_secs(40));
        assert!(self.child.wait().expect("waiting for the client").success());
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array().unwrap().iter().map(|v| v[key].as_str().unwrap().to_string()).collect()
}

fn by_name<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array().unwrap().iter().find(|v| v["name"] == name).unwrap_or_else(|| panic!("no {name:?} in {list}"))
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// Password sign-in, what the stores hold for Daily Gaming, then the saved sign-in on
/// the next start, then Log out forgetting it.
#[test]
fn sign_in_daily_gaming_saved_sign_in_and_logout() {
    if !stack() {
        return;
    }
    let profile = tempfile::tempdir().unwrap();
    let gaming_dumps = |what: &str| format!("{what} {GAMING}");

    // ── First start: nothing saved; a password sign-in. ──
    let first = run_ok(
        profile.path(),
        &[
            "wait", "booted", "dump", "auth", "print", "before-login",
            "login", "perf2", "wait", "logged-in", "wait", "connected", GAMING, "wait", "roster", GAMING,
            "dump", "auth", "dump", "core", "dump", "servers", "dump", "connections", "dump", "nav",
            "dump", "channels", GAMING, "dump", "roles", GAMING, "dump", "roster", GAMING,
            "dump", "permissions", GAMING, "general", "dump", "permissions", GAMING, "announcements",
            "dump", "permissions", GAMING, "mod-chat",
            "open-server", GAMING, "dump", "nav", "open-channel", "general", "dump", "nav",
            "dump", "users", "dump", "pump", "dump", "toasts",
        ],
    );
    // `dump auth` ran twice: the map keeps the last (signed in). The first is in stdout.
    let before = first.stdout.lines().find_map(parse_dump).unwrap().1;
    assert_eq!(before["authenticated"], false);
    assert_eq!(before["savedUsername"], Value::Null);
    assert_eq!(before["booted"], true);

    let auth = first.dump("auth");
    assert_eq!((auth["authenticated"].as_bool(), auth["username"].as_str(), auth["uid"].as_i64()), (Some(true), Some("perf2"), Some(7)));
    assert_eq!((auth["renames"].as_bool(), auth["devices"].as_bool()), (Some(true), Some(true)));
    assert_eq!(first.dump("core")["userDataDir"], profile.path().display().to_string());

    // Memberships → the community connects.
    let servers = first.dump("servers");
    assert_eq!(names(&servers["servers"], "name"), [GAMING], "perf2 belongs to Daily Gaming only");
    let gaming = &servers["servers"][0];
    assert_eq!((gaming["id"].as_str(), gaming["host"].as_str(), gaming["port"].as_i64()), (Some("1"), Some("127.0.0.11"), Some(8082)));
    assert_eq!((gaming["connected"].as_bool(), gaming["pending"].as_bool()), (Some(true), Some(false)));
    assert_eq!(gaming["memberCount"], 298);
    // The public directory lists the two public communities, never the unlisted one.
    let mut discoverable = names(&servers["discoverable"], "name");
    discoverable.sort();
    assert_eq!(discoverable, [GAMING, "Design Lab"]);
    assert_eq!(first.dump("connections")["central"], "connected");
    assert_eq!(first.dump("connections")["connected"], serde_json::json!(["1"]));

    // The channel tree, as tools/dev seeded it and as a Member may see it: no Staff.
    let channels = first.dump(&gaming_dumps("channels"));
    let seen: Vec<(String, String)> = channels
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap().to_string(), c["kind"].as_str().unwrap().to_string()))
        .collect();
    let expected = [
        ("Information", "category"), ("welcome", "text"), ("announcements", "text"),
        ("Text Channels", "category"), ("general", "text"), ("clips", "text"), ("off-topic", "text"),
        ("slow-lane", "text"), ("vault", "text"),
        ("Voice Channels", "category"), ("Lounge", "voice"), ("Squad", "voice"), ("AFK", "voice"),
    ];
    assert_eq!(seen, expected.map(|(n, k)| (n.to_string(), k.to_string())));
    assert_eq!(by_name(channels, "slow-lane")["slowmodeSeconds"], 30);
    assert_eq!(by_name(channels, "vault")["encrypted"], true);
    assert_eq!(by_name(channels, "general")["encrypted"], false);
    assert_ne!(by_name(channels, "off-topic")["topic"], "");
    assert_eq!(by_name(channels, "general")["category"], "Text Channels");
    assert_eq!(by_name(channels, "Lounge")["category"], "Voice Channels");
    assert_eq!(by_name(channels, "Information")["category"], Value::Null);

    // Roles, most senior first.
    let roles = first.dump(&gaming_dumps("roles"));
    assert_eq!(names(roles, "name"), ["Admin", "Moderator", "Member", "everyone"]);
    assert_eq!(by_name(roles, "everyone")["isDefault"], true);
    assert_eq!(by_name(roles, "everyone")["memberCount"], 298);
    assert_eq!(by_name(roles, "Admin")["memberCount"], 1);
    assert_eq!(by_name(roles, "Moderator")["memberCount"], 4);
    assert_eq!(by_name(roles, "Member")["memberCount"], 204);
    let member_role = by_name(roles, "Member")["id"].as_i64().unwrap();

    // The first roster window: the counts, alice owns it, our own row is held.
    let roster = first.dump(&gaming_dumps("roster"));
    assert_eq!(roster["meta"]["totalMembers"], 298);
    assert_eq!(roster["meta"]["refetching"], false);
    assert_eq!(roster["owner"], 1);
    let online = roster["online"].as_array().unwrap();
    assert_eq!(roster["meta"]["onlineCount"].as_u64().unwrap() as usize, online.len(), "every online member is in the first page");
    let me = online.iter().find(|m| m["uid"] == 7).expect("we are online in our own roster");
    assert_eq!((me["username"].as_str(), me["isOwner"].as_bool()), (Some("perf2"), Some(false)));
    assert_eq!(me["roleIds"], serde_json::json!([member_role]));
    // 298 members fit the first page (500): the offline part is complete too.
    assert_eq!(roster["meta"]["hasMore"], false);
    assert_eq!(roster["held"], 298);
    assert_eq!(online.len() + roster["offlineCount"].as_u64().unwrap() as usize, 298);
    // Roster order: folded display names ascending.
    let offline_first = names(&roster["offlineFirst"], "display");
    let mut sorted = offline_first.clone();
    sorted.sort_by_key(|n| n.to_lowercase());
    assert_eq!(offline_first, sorted);

    // Effective permissions of a member.
    let general = first.dump(&format!("permissions {GAMING} general"));
    assert_eq!(general["isOwner"], false);
    assert_eq!(general["channel"], serde_json::json!({"view": true, "send": true, "attach": true, "readHistory": true, "manageMessages": false, "manageChannels": false}));
    let announcements = first.dump(&format!("permissions {GAMING} announcements"));
    assert_eq!((announcements["channel"]["view"].as_bool(), announcements["channel"]["send"].as_bool()), (Some(true), Some(false)), "read-only for @everyone");
    assert_eq!(first.dump(&format!("permissions {GAMING} mod-chat"))["visible"], false, "Staff is hidden from members");
    assert_eq!(general["server"]["kickMembers"], false);
    assert_eq!(general["server"]["administrator"], false);
    assert_eq!(general["server"]["sendMessages"], true);

    // Navigation: opening the community lands on its first text channel.
    let nav = first.dump("nav");
    assert_eq!((nav["view"].as_str(), nav["server"].as_str(), nav["channel"].as_str()), (Some("server"), Some("1"), Some("general")));
    let navs: Vec<Value> = first.stdout.lines().filter_map(parse_dump).filter(|(n, _)| n == "nav").map(|(_, v)| v).collect();
    assert_eq!(navs[0]["view"], "home");
    let welcome_id = by_name(channels, "welcome")["id"].as_str().unwrap();
    assert_eq!(navs[1]["channel"], welcome_id, "open-server picks the first text channel");

    // The users directory: central named us, the community named its members.
    let users: HashMap<i64, String> = first.dump("users")["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| (pair[0].as_i64().unwrap(), pair[1].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(users.get(&7).map(String::as_str), Some("perf2"));
    assert_eq!(users.get(&1).map(String::as_str), Some("alice"));
    assert_eq!(users.get(&2).map(String::as_str), Some("bob"));
    assert!(users.keys().all(|uid| *uid > 0), "placeholders never enter the directory");
    assert!(users.len() >= 298);

    // The pump: a sign-in is a handful of events and never more wakes than events.
    let pump = first.dump("pump");
    assert!(pump["events"].as_u64().unwrap() >= 5);
    assert!(pump["wakes"].as_u64().unwrap() <= pump["events"].as_u64().unwrap());
    assert_eq!(pump["wakes"], pump["drains"]);
    assert_eq!(first.dump("toasts"), &serde_json::json!([]));
    println!("first start: pump {pump}, {} names, roster online {}", users.len(), online.len());

    // ── Second start: no login step. The saved sign-in signs in by itself. ──
    let second = run_ok(profile.path(), &["wait", "logged-in", "wait", "connected", GAMING, "dump", "auth", "dump", "servers"]);
    let auth = second.dump("auth");
    assert_eq!((auth["authenticated"].as_bool(), auth["username"].as_str(), auth["savedUsername"].as_str()), (Some(true), Some("perf2"), Some("perf2")));
    assert_eq!(second.dump("servers")["servers"][0]["connected"], true);

    // ── Third start: Log out forgets the saved sign-in… ──
    let third = run_ok(
        profile.path(),
        &["wait", "logged-in", "open-server", GAMING, "logout", "wait", "logged-out", "dump", "auth", "dump", "servers", "dump", "nav", "dump", "connections"],
    );
    let auth = third.dump("auth");
    assert_eq!((auth["authenticated"].as_bool(), auth["username"].clone(), auth["uid"].as_i64()), (Some(false), Value::Null, Some(0)));
    assert_eq!(auth["loginNotice"], Value::Null, "our own Log out shows no notice");
    assert_eq!(third.dump("servers")["servers"], serde_json::json!([]));
    assert_eq!(third.dump("nav"), &serde_json::json!({"view": "home", "server": null, "channel": null}));
    assert_eq!(third.dump("connections")["connected"], serde_json::json!([]));
    let config: Value = serde_json::from_slice(&std::fs::read(profile.path().join("config.json")).unwrap()).unwrap();
    assert!(config.get("saved_login").is_none_or(Value::is_null), "the saved sign-in is gone from config.json: {config}");

    // ── …so the fourth start lands on the sign-in page. ──
    let fourth = run_ok(profile.path(), &["wait", "booted", "dump", "auth"]);
    let auth = fourth.dump("auth");
    assert_eq!((auth["authenticated"].as_bool(), auth["savedUsername"].clone(), auth["loggingIn"].as_bool()), (Some(false), Value::Null, Some(false)));
}

/// A second client signing in is a live roster delta for the first one, and its
/// sign-out another.
#[test]
fn a_second_client_comes_online_and_leaves() {
    if !stack() {
        return;
    }
    let (watcher_profile, other_profile) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let roster_dump = format!("roster {GAMING}");

    // Who is offline right now? (Other work on this machine may hold some accounts.)
    let peek = run_ok(watcher_profile.path(), &["login", "perf3", "wait", "connected", GAMING, "wait", "roster", GAMING, "dump", "roster", GAMING]);
    let online_now = names(&peek.dump(&roster_dump)["online"], "username");
    let Some(other) = ["perf4", "perf1"].into_iter().find(|name| !online_now.iter().any(|n| n == name)) else {
        eprintln!("skipped: perf4 and perf1 are both online already ({online_now:?})");
        return;
    };
    let go = watcher_profile.path().join("other-may-leave");

    // The watcher starts from its saved sign-in and waits for `other` to come and go.
    let mut watcher = Live::start(
        watcher_profile.path(),
        &[
            "wait", "connected", GAMING, "wait", "roster", GAMING, "dump", "roster", GAMING, "dump", "pump", "print", "watching",
            "wait", "online", GAMING, other, "dump", "roster", GAMING, "print", "saw-online",
            "wait", "offline", GAMING, other, "dump", "roster", GAMING, "dump", "pump", "print", "saw-offline",
            "logout", "wait", "logged-out",
        ],
    );
    watcher.until("MARK watching", Duration::from_secs(40));
    let before = watcher.last_dump(&roster_dump);
    assert!(!names(&before["online"], "username").iter().any(|n| n == other));
    let (revision, online_count) = (before["meta"]["revision"].as_u64().unwrap(), before["meta"]["onlineCount"].as_u64().unwrap());

    let mut second = Live::start(
        other_profile.path(),
        &["login", other, "wait", "connected", GAMING, "print", "online", "wait-file", go.to_str().unwrap(), "logout", "wait", "logged-out"],
    );
    second.until("MARK online", Duration::from_secs(40));
    watcher.until("MARK saw-online", Duration::from_secs(40));
    let during = watcher.last_dump(&roster_dump);
    let row = during["online"].as_array().unwrap().iter().find(|m| m["username"] == other).expect("the delta put them in the Online section").clone();
    assert_eq!(row["isOwner"], false);
    assert!(during["meta"]["revision"].as_u64().unwrap() > revision, "deltas advance the revision");
    assert!(during["meta"]["onlineCount"].as_u64().unwrap() > online_count.saturating_sub(1));
    assert_eq!(during["meta"]["generation"], before["meta"]["generation"], "no refetch: the deltas continued the revision");
    assert_eq!(during["held"], 298);

    std::fs::write(&go, b"").unwrap();
    second.finish();
    watcher.until("MARK saw-offline", Duration::from_secs(40));
    let after = watcher.last_dump(&roster_dump);
    assert!(!names(&after["online"], "username").iter().any(|n| n == other));
    assert!(after["meta"]["revision"].as_u64().unwrap() > during["meta"]["revision"].as_u64().unwrap());
    assert_eq!(after["meta"]["generation"], before["meta"]["generation"]);
    println!("{other} came online and left: roster revision {revision} → {} → {}; pump {}", during["meta"]["revision"], after["meta"]["revision"], watcher.last_dump("pump"));
    watcher.finish();
}

/// Rustaceans restricts its members: what a member's stores say against the owner's.
#[test]
fn a_member_and_the_owner_in_a_restricted_community() {
    if !stack() {
        return;
    }
    const RUST: &str = "Rustaceans";
    let script = [
        "login", "", "wait", "logged-in", "wait", "connected", RUST, "wait", "roster", RUST,
        "dump", "auth", "dump", "channels", RUST, "dump", "roles", RUST, "dump", "roster", RUST,
        "dump", "permissions", RUST, "core-team", "dump", "permissions", RUST, "announcements",
        "dump", "permissions", RUST, "showcase", "dump", "permissions", RUST, "general",
        "logout", "wait", "logged-out",
    ];
    let as_account = |account: &'static str| {
        let profile = tempfile::tempdir().unwrap();
        let mut steps = script;
        steps[1] = account;
        run_ok(profile.path(), &steps)
    };
    let permissions = |run: &Run, channel: &str| run.dump(&format!("permissions {RUST} {channel}")).clone();

    // dave: a member without a role that opens `core-team`.
    let member = as_account("dave");
    assert_eq!(member.dump("auth")["uid"], 4);
    let channels = names(member.dump(&format!("channels {RUST}")), "name");
    assert!(!channels.iter().any(|c| c == "core-team"), "hidden from members: {channels:?}");
    assert!(channels.iter().any(|c| c == "general") && channels.iter().any(|c| c == "announcements"));
    assert_eq!(permissions(&member, "core-team")["visible"], false);
    assert_eq!(permissions(&member, "core-team")["channel"]["view"], false);
    let announcements = permissions(&member, "announcements");
    assert_eq!((announcements["channel"]["view"].as_bool(), announcements["channel"]["send"].as_bool()), (Some(true), Some(false)));
    let showcase = permissions(&member, "showcase");
    assert_eq!((showcase["channel"]["send"].as_bool(), showcase["channel"]["attach"].as_bool()), (Some(true), Some(false)));
    let general = permissions(&member, "general");
    assert_eq!((general["channel"]["send"].as_bool(), general["channel"]["attach"].as_bool()), (Some(true), Some(true)));
    assert_eq!(general["isOwner"], false);
    // Server-wide: @everyone here has no Create Invite and no Change Nickname.
    assert_eq!(general["server"]["createInvite"], false);
    assert_eq!(general["server"]["changeNickname"], false);
    assert_eq!(general["server"]["sendMessages"], true);
    assert_eq!(general["server"]["manageServer"], false);
    let roster = member.dump(&format!("roster {RUST}"));
    assert_eq!(roster["meta"]["totalMembers"], 29);
    assert_eq!(roster["owner"], 2, "bob owns Rustaceans");

    // bob: the owner. Everything, everywhere, and the hidden channel is listed.
    let owner = as_account("bob");
    let channels = names(owner.dump(&format!("channels {RUST}")), "name");
    assert!(channels.iter().any(|c| c == "core-team"));
    for channel in ["core-team", "announcements", "showcase", "general"] {
        let p = permissions(&owner, channel);
        assert_eq!(p["isOwner"], true);
        assert!(p["channel"].as_object().unwrap().values().all(|v| v == true), "{channel}: {p}");
        assert!(p["server"].as_object().unwrap().values().all(|v| v == true), "{channel}: {p}");
    }
    // Both saw the same role list.
    assert_eq!(names(member.dump(&format!("roles {RUST}")), "name"), ["Core", "Rustacean", "everyone"]);
    assert_eq!(member.dump(&format!("roles {RUST}")), owner.dump(&format!("roles {RUST}")));
}

/// One client through both halves of the model in one process (the headless usage's
/// example): perf3 signs in, unlocks its E2EE keys (the stack's test passphrase; voice
/// needs them), reads `#off-topic` and posts there, joins Lounge deafened (no audio leaves
/// or plays), sets Do Not Disturb, watches 100 members of Daily Gaming, then `dump all`;
/// afterwards it leaves voice, goes back online, deletes its post and signs out.
#[test]
fn one_client_through_both_halves_and_dump_all() {
    if !stack() {
        return;
    }
    const PASSPHRASE: &str = "decibell-m3-test-passphrase";
    let profile = tempfile::tempdir().unwrap();
    let id = format!(
        "combined-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
    );
    let text = format!("{id} hello from both halves");
    let prefix = format!("prefix:{id}");
    let run = run_ok(
        profile.path(),
        &[
            "login", "perf3", "wait", "connected", GAMING, "wait", "roster", GAMING, "open-server", GAMING,
            "e2ee-ready", PASSPHRASE, "wait", "e2ee", "ready",
            "open-channel", "off-topic", "history", "off-topic", "send", "off-topic", &text, "wait-sent", "off-topic",
            "deafen", "voice-join", "Lounge", "wait", "voice", "Lounge", "perf3", "wait", "mls", "ready",
            "status", "dnd", "watch-roster", GAMING, "100", "idle", "1",
            "dump", "all",
            "voice-leave", "status", "online", "delete", "off-topic", &prefix, "wait-deleted", "off-topic",
            "history", "off-topic", "dump", "chat", "off-topic", "logout", "wait", "logged-out",
        ],
    );
    // Every store of both halves answered in one go.
    for what in [
        "auth", "users", "servers", "connections", "nav", "settings", "layout", "toasts", "pump", "core",
        &format!("channels {GAMING}"), &format!("roles {GAMING}"), &format!("roster {GAMING}"),
        &format!("permissions {GAMING} off-topic"), "search", "voice", "presence", "own", "friends", "e2ee", "call",
        "transfers", "devices", "hotkeys", "cues",
    ] {
        // A dump that couldn't be made is `{"error": "…"}` alone.
        let d = run.dump(what);
        assert!(!d.as_object().is_some_and(|o| o.len() == 1 && o["error"].is_string()), "dump {what}: {d}");
    }
    let me = run.dump("auth")["uid"].as_i64().unwrap();
    assert_eq!(run.dump("auth")["username"], "perf3");
    assert_eq!(run.dump("nav")["channel"], "off-topic");
    // The post, echoed (a stored id) — the dump of `all` came before the cleanup.
    let messages = run.stdout.lines().filter_map(parse_dump).find(|(w, _)| w == "chat off-topic").unwrap().1;
    let posted = messages["messages"].as_array().unwrap().iter().find(|m| m["content"] == text.as_str()).cloned();
    assert!(posted.as_ref().is_some_and(|m| m["id"].as_i64().unwrap() > 0), "echoed: {messages}");
    // Voice: in Lounge, deafened, MLS ready, us on the stage.
    let voice = run.dump("voice");
    assert!(voice["channel"].is_string(), "{voice}");
    assert_eq!(voice["deafened"], true);
    assert_eq!(voice["e2ee"]["phase"], "ready", "{voice}");
    assert!(voice["participants"].as_array().unwrap().iter().any(|p| p["uid"] == me), "{voice}");
    assert_eq!(run.dump("own")["status"], "dnd");
    assert_eq!(run.dump("e2ee")["status"], "ready");
    let watched = run.dump("presence")["watched"].as_u64().unwrap();
    assert!((90..=100).contains(&watched), "watched {watched}");
    assert!(run.dump("pump")["events"].as_u64().unwrap() > 0);
    // Cleaned up: the post is gone from a fresh page (the last `chat off-topic` dump).
    let after = run.dump("chat off-topic");
    assert!(!after["messages"].as_array().unwrap().iter().any(|m| m["content"] == text.as_str()), "deleted: {after}");
}
