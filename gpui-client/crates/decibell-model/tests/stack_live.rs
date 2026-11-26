//! The M3 stores through the real core against the local test stack (docs/model.md §9 and
//! "M3"): statuses and messages by subscription, invisible, friend requests, devices and
//! signing one out, E2EE setup and a peer's keys, two clients in a voice channel, a DM
//! call ringing on two devices.
//!
//! ```sh
//! eval "$(../tools/dev/local-stack.sh client-env)"     # DECIBELL_CENTRAL=127.0.0.1:18080
//! DECIBELL_STACK=1 cargo test -p decibell-model --test stack_live -- --nocapture --test-threads 1
//! ```
//!
//! Accounts: `perf3` and `perf4` (another agent may use `perf1` / `perf2`; a device of the
//! same account online elsewhere is tolerated), `dave` and `erin` for the friend request
//! (they have no relation in the seed, and none is left behind). **`perf3` and `perf4` get
//! E2EE keys** (passphrase [`PASSPHRASE`]): the first run sets them up, later runs unlock.
//! Every client runs silent: muted or deafened before it joins voice or a call.

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::Value;

const HEADLESS: &str = env!("CARGO_BIN_EXE_decibell-headless");
const GAMING: &str = "Daily Gaming";
/// The E2EE passphrase of the test accounts' keys.
pub const PASSPHRASE: &str = "decibell-m3-test-passphrase";

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
    command.arg("--user-data-dir").arg(profile).arg("--timeout").arg("40").args(steps);
    command.env("DECIBELL_LOG", "warn").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

fn parse_dump(line: &str) -> Option<(String, Value)> {
    let rest = line.strip_prefix("DUMP ")?;
    let json_at = rest.find(['{', '['])?;
    Some((rest[..json_at].trim().to_string(), serde_json::from_str(&rest[json_at..]).ok()?))
}

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

impl Run {
    fn dumps(&self, what: &str) -> Vec<Value> {
        self.stdout.lines().filter_map(parse_dump).filter(|(n, _)| n == what).map(|(_, v)| v).collect()
    }

    fn dump(&self, what: &str) -> Value {
        self.dumps(what).pop().unwrap_or_else(|| panic!("no `dump {what}` in:\n{}\n{}", self.stdout, self.stderr))
    }
}

fn run(profile: &Path, steps: &[&str]) -> Run {
    let output = command(profile, steps).output().expect("starting decibell-headless");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Run {
        ok: output.status.success() && stdout.lines().any(|l| l == "DONE"),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        stdout,
    }
}

fn run_ok(profile: &Path, steps: &[&str]) -> Run {
    let r = run(profile, steps);
    assert!(r.ok, "the script failed:\n--- stdout\n{}\n--- stderr\n{}", r.stdout, r.stderr);
    r
}

struct Live {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
    name: &'static str,
}

impl Live {
    fn start(name: &'static str, profile: &Path, steps: &[&str]) -> Live {
        let mut child = command(profile, steps).stderr(Stdio::inherit()).spawn().expect("starting decibell-headless");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::Builder::new()
            .name("stack-live-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .unwrap();
        Live { child, lines, seen: Vec::new(), name }
    }

    fn until(&mut self, line: &str, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            match self.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(got) => {
                    let found = got == line;
                    assert!(!got.starts_with("FAILED"), "{}: {got}\nbefore it: {:#?}", self.name, self.seen);
                    self.seen.push(got);
                    if found {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("{}: no {line:?} within {limit:?}; seen: {:#?}", self.name, self.seen),
                Err(RecvTimeoutError::Disconnected) => panic!("{}: exited before {line:?}; seen: {:#?}", self.name, self.seen),
            }
        }
    }

    fn dump(&self, what: &str) -> Value {
        self.seen
            .iter()
            .rev()
            .filter_map(|l| parse_dump(l))
            .find(|(n, _)| n == what)
            .unwrap_or_else(|| panic!("{}: no `dump {what}` yet; seen: {:#?}", self.name, self.seen))
            .1
    }

    fn finish(mut self) -> Vec<String> {
        self.until("DONE", Duration::from_secs(60));
        assert!(self.child.wait().expect("waiting for the client").success());
        std::mem::take(&mut self.seen)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn sign_in(account: &'static str) -> [&'static str; 6] {
    ["login", account, "wait", "logged-in", "wait", "connected"]
}

/// perf4 back to Online with no message (statuses persist on the account).
fn restore_perf4_status() {
    let p = tmp();
    let _ = run(p.path(), &["login", "perf4", "wait", "logged-in", "status", "online", "status-message", "", "0", "idle", "1", "logout", "wait", "logged-out"]);
}

// ── Statuses ────────────────────────────────────────────────────────────────

/// A status change reaches a client that watches the user and not one that doesn't;
/// invisible looks offline; a status message with a clear-after goes by itself.
#[test]
fn statuses_reach_watchers_only() {
    if !stack() {
        return;
    }
    restore_perf4_status();
    let (p4, watcher, bystander) = (tmp(), tmp(), tmp());
    let go = |n: u32| p4.path().join(format!("go{n}"));
    let (go1, go2, go3, go4) = (go(1), go(2), go(3), go(4));

    let mut perf4 = Live::start(
        "perf4",
        p4.path(),
        &[
            "login", "perf4", "wait", "logged-in", "print", "online",
            "wait-file", go1.to_str().unwrap(), "status", "dnd",
            "wait-file", go2.to_str().unwrap(), "status-message", "brb m3", "4", "idle", "1", "dump", "own",
            "idle", "5", "dump", "own",
            "wait-file", go3.to_str().unwrap(), "status", "invisible",
            "wait-file", go4.to_str().unwrap(), "status", "online", "idle", "1",
            "logout", "wait", "logged-out",
        ],
    );
    perf4.until("MARK online", Duration::from_secs(40));
    let mut watching = Live::start(
        "perf3 (watching)",
        watcher.path(),
        &[
            "login", "perf3", "wait", "logged-in", "watch", "perf4", "wait", "presence", "perf4", "online", "print", "sees-online",
            "wait", "presence", "perf4", "dnd", "print", "sees-dnd",
            "wait", "message", "perf4", "brb m3", "print", "sees-message",
            "wait", "message", "perf4", "-", "print", "message-cleared",
            "wait", "presence", "perf4", "offline", "print", "sees-offline",
            "wait", "presence", "perf4", "online", "dump", "presence", "print", "sees-back",
            "logout", "wait", "logged-out",
        ],
    );
    watching.until("MARK sees-online", Duration::from_secs(40));
    let mut not_watching = Live::start(
        "perf3 (not watching)",
        bystander.path(),
        &["login", "perf3", "wait", "logged-in", "print", "in", "wait-file", go4.to_str().unwrap(), "idle", "1", "dump", "presence", "logout", "wait", "logged-out"],
    );
    not_watching.until("MARK in", Duration::from_secs(40));

    std::fs::write(&go1, b"").unwrap();
    watching.until("MARK sees-dnd", Duration::from_secs(30));
    std::fs::write(&go2, b"").unwrap();
    watching.until("MARK sees-message", Duration::from_secs(30));
    // Central clears expired messages on its 30 s sweep: others see it up to 30 s late.
    watching.until("MARK message-cleared", Duration::from_secs(50));
    std::fs::write(&go3, b"").unwrap();
    watching.until("MARK sees-offline", Duration::from_secs(30));
    std::fs::write(&go4, b"").unwrap();
    watching.until("MARK sees-back", Duration::from_secs(30));

    let seen = perf4.finish();
    let owns: Vec<Value> = seen.iter().filter_map(|l| parse_dump(l)).filter(|(n, _)| n == "own").map(|(_, v)| v).collect();
    assert_eq!(owns[0]["display"], "dnd");
    assert_eq!(owns[0]["message"]["text"], "brb m3");
    assert_eq!(owns[0]["clears"], true);
    assert_eq!(owns[1]["message"], Value::Null, "our own copy clears at the deadline: {owns:?}");
    watching.finish();
    not_watching.until("DONE", Duration::from_secs(40));
    let bystander_presence = not_watching.dump("presence");
    let known = bystander_presence["known"].as_array().unwrap();
    assert!(!known.iter().any(|k| k[0] == 9), "a client that didn't watch perf4 never heard of it: {known:?}");
    restore_perf4_status();
}

// ── Friends ─────────────────────────────────────────────────────────────────

/// dave asks erin, erin accepts, dave removes her: both lists follow, and nothing is left.
#[test]
fn friend_request_accept_remove() {
    if !stack() {
        return;
    }
    let (dave_p, erin_p) = (tmp(), tmp());
    let accept = dave_p.path().join("accept");
    let remove = dave_p.path().join("remove");
    let mut erin = Live::start(
        "erin",
        erin_p.path(),
        &[
            "login", "erin", "wait", "logged-in", "friends", "idle", "1", "print", "ready",
            "wait", "friend", "dave", "pending_incoming", "print", "asked",
            "wait-file", accept.to_str().unwrap(), "friend-accept", "dave",
            "wait", "friend", "dave", "online|offline", "print", "friends",
            "wait", "friend", "dave", "none", "dump", "friends", "logout", "wait", "logged-out",
        ],
    );
    erin.until("MARK ready", Duration::from_secs(40));
    let mut dave = Live::start(
        "dave",
        dave_p.path(),
        &[
            "login", "dave", "wait", "logged-in", "friends", "idle", "1", "friend-add", "erin",
            "wait", "friend", "erin", "pending_outgoing", "print", "asked",
            "wait", "friend", "erin", "online|offline", "print", "accepted",
            "wait-file", remove.to_str().unwrap(), "friend-remove", "erin", "wait", "friend", "erin", "none",
            "dump", "friends", "logout", "wait", "logged-out",
        ],
    );
    dave.until("MARK asked", Duration::from_secs(40));
    erin.until("MARK asked", Duration::from_secs(40));
    std::fs::write(&accept, b"").unwrap();
    erin.until("MARK friends", Duration::from_secs(40));
    dave.until("MARK accepted", Duration::from_secs(40));
    std::fs::write(&remove, b"").unwrap();
    let dave_seen = dave.finish();
    erin.finish();
    let last = dave_seen.iter().rev().find_map(|l| parse_dump(l)).unwrap().1;
    assert!(!last["friends"].as_array().unwrap().iter().any(|f| f["username"] == "erin"));
}

// ── Devices ─────────────────────────────────────────────────────────────────

/// Two devices of perf4: the list shows both; one signs the other out, which is signed
/// out with the notice, and a new password sign-in was announced to the first.
#[test]
fn devices_list_and_sign_out() {
    if !stack() {
        return;
    }
    let (a_p, b_p) = (tmp(), tmp());
    let id_file = a_p.path().join("b-device-id");
    let mut a = Live::start(
        "perf4 A",
        a_p.path(),
        &[
            "login", "perf4", "wait", "logged-in", "print", "a-in",
            "wait-file", id_file.to_str().unwrap(), "devices", "wait", "devices", "idle", "1", "dump", "devices", "dump", "toasts",
            "device-sign-out", &format!("@{}", id_file.display()), "idle", "2", "dump", "devices",
            "logout", "wait", "logged-out",
        ],
    );
    a.until("MARK a-in", Duration::from_secs(40));
    let mut b = Live::start(
        "perf4 B",
        b_p.path(),
        &["login", "perf4", "wait", "logged-in", "devices", "wait", "devices", "dump", "devices", "print", "b-in", "wait", "logged-out", "dump", "auth"],
    );
    b.until("MARK b-in", Duration::from_secs(40));
    let b_list = b.dump("devices");
    let b_id = b_list["list"].as_array().unwrap().iter().find(|d| d["current"] == true).unwrap()["id"].as_i64().unwrap();
    std::fs::write(&id_file, b_id.to_string()).unwrap();
    let b_seen = b.finish();
    let auth = b_seen.iter().rev().find_map(|l| parse_dump(l).filter(|(n, _)| n == "auth")).unwrap().1;
    assert_eq!(auth["authenticated"], false);
    assert_eq!(auth["loginNotice"], "You were signed out on this device. Sign in again to keep using it here.");
    let a_seen = a.finish();
    let lists: Vec<Value> = a_seen.iter().filter_map(|l| parse_dump(l)).filter(|(n, _)| n == "devices").map(|(_, v)| v).collect();
    let ids = |v: &Value| v["list"].as_array().unwrap().iter().map(|d| d["id"].as_i64().unwrap()).collect::<Vec<_>>();
    assert!(ids(&lists[0]).contains(&b_id), "both devices listed: {lists:?}");
    assert!(lists[0]["list"].as_array().unwrap().iter().any(|d| d["current"] == true && d["id"] != b_id));
    assert!(!ids(&lists[1]).contains(&b_id), "signed out: gone from the list");
    let toasts = a_seen.iter().find_map(|l| parse_dump(l).filter(|(n, _)| n == "toasts")).unwrap().1;
    assert!(toasts.as_array().unwrap().iter().any(|t| t["title"] == "New sign-in to your account"), "{toasts}");
}

// ── E2EE ────────────────────────────────────────────────────────────────────

/// perf3 and perf4 set encryption up (or unlock it on a fresh device), then perf3 fetches
/// perf4's keys and a safety number.
#[test]
fn e2ee_ready_and_a_peers_keys() {
    if !stack() {
        return;
    }
    for account in ["perf3", "perf4"] {
        let p = tmp();
        let r = run_ok(p.path(), &["login", account, "wait", "logged-in", "e2ee-ready", PASSPHRASE, "dump", "e2ee", "logout", "wait", "logged-out"]);
        let e = r.dump("e2ee");
        assert_eq!(e["status"], "ready", "{account}: {e}");
        assert!(e["fingerprint"].as_str().unwrap().len() > 20);
    }
    let p = tmp();
    let r = run_ok(p.path(), &["login", "perf3", "wait", "logged-in", "e2ee-ready", PASSPHRASE, "peer-info", "perf4", "logout", "wait", "logged-out"]);
    let info = r.dump("peer-info");
    assert_eq!(info["hasKeys"], true, "{info}");
    assert!(info["safetyNumber"].as_str().unwrap().len() >= 20, "{info}");
}

// ── Voice ───────────────────────────────────────────────────────────────────

/// perf3 (deafened) and perf4 (muted) join Daily Gaming's Lounge: both rosters show both,
/// the MLS group is ready with two members, perf4's deafen and undeafen reach perf3,
/// and leaving shows. Nobody's audio is sent (muted, deafened).
#[test]
fn two_clients_in_a_voice_channel() {
    if !stack() {
        return;
    }
    let (p3, p4) = (tmp(), tmp());
    let go = p3.path().join("go");
    let s3 = sign_in("perf3");
    let mut perf3 = Live::start(
        "perf3",
        p3.path(),
        &[
            s3[0], s3[1], s3[2], s3[3], s3[4], s3[5], GAMING, "open-server", GAMING, "e2ee-ready", PASSPHRASE, "deafen",
            "voice-join", "Lounge", "wait", "voice", "Lounge", "perf3", "print", "joined",
            "wait", "voice", "Lounge", "perf4", "wait", "mls", "ready", "idle", "2", "dump", "voice", "print", "both",
            "wait", "voice-state", "perf4", "deafened", "print", "sees-deafened",
            "wait", "voice-state", "perf4", "undeafened", "print", "sees-undeafened",
            "wait", "voice-gone", "Lounge", "perf4", "dump", "voice", "dump", "cues", "voice-leave", "idle", "1",
            "logout", "wait", "logged-out",
        ],
    );
    perf3.until("MARK joined", Duration::from_secs(60));
    let s4 = sign_in("perf4");
    let mut perf4 = Live::start(
        "perf4",
        p4.path(),
        &[
            s4[0], s4[1], s4[2], s4[3], s4[4], s4[5], GAMING, "open-server", GAMING, "e2ee-ready", PASSPHRASE, "mute",
            "voice-join", "Lounge", "wait", "voice", "Lounge", "perf3", "wait", "mls", "ready", "idle", "2", "dump", "voice", "print", "both",
            "wait-file", go.to_str().unwrap(), "deafen", "idle", "2", "undeafen", "idle", "2", "dump", "voice",
            "voice-leave", "idle", "1", "logout", "wait", "logged-out",
        ],
    );
    perf4.until("MARK both", Duration::from_secs(60));
    perf3.until("MARK both", Duration::from_secs(60));
    std::fs::write(&go, b"").unwrap();
    perf3.until("MARK sees-deafened", Duration::from_secs(30));
    perf3.until("MARK sees-undeafened", Duration::from_secs(30));
    let seen4 = perf4.finish();
    let seen3 = perf3.finish();
    let voice = |seen: &[String], n: usize| {
        seen.iter().filter_map(|l| parse_dump(l)).filter(|(w, _)| w == "voice").nth(n).unwrap().1
    };
    for (who, v) in [("perf3", voice(&seen3, 0)), ("perf4", voice(&seen4, 0))] {
        let lounge = v["channels"].as_array().unwrap().iter().find(|c| c["channel"] == v["channel"]).unwrap().clone();
        let mut roster: Vec<i64> = lounge["roster"].as_array().unwrap().iter().map(|u| u.as_i64().unwrap()).collect();
        roster.sort();
        assert_eq!(roster, [8, 9], "{who} sees both: {v}");
        assert_eq!(v["e2ee"]["phase"], "ready", "{who}: {v}");
        assert_eq!(v["e2ee"]["members"], 2, "{who}: {v}");
        assert_eq!(v["participants"].as_array().unwrap().len(), 2);
    }
    assert_eq!(voice(&seen3, 0)["deafened"], true);
    let after = voice(&seen4, 1);
    assert_eq!((after["muted"].as_bool(), after["deafened"].as_bool()), (Some(true), Some(false)), "undeafen restores the mute: {after}");
    let cues = seen3.iter().rev().find_map(|l| parse_dump(l).filter(|(n, _)| n == "cues")).unwrap().1;
    let cues: Vec<String> = cues.as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    assert!(cues.contains(&"Play(UserJoin)".to_string()) && cues.contains(&"Play(UserLeave)".to_string()), "{cues:?}");
    println!("voice: perf3 {} / perf4 {}", voice(&seen3, 0), voice(&seen4, 0));
}

// ── Calls ───────────────────────────────────────────────────────────────────

/// perf3 calls perf4, signed in on two devices: both ring; one answers, the other shows
/// "answered on another device"; the media is tried P2P over loopback; perf3 hangs up.
/// Every device unlocks its E2EE keys first: a call to someone with keys must be signed
/// (`call_connect` fails closed otherwise).
#[test]
fn a_call_rings_every_device() {
    if !stack() {
        return;
    }
    let (caller_p, d1_p, d2_p) = (tmp(), tmp(), tmp());
    let go = caller_p.path().join("go");
    let mut d1 = Live::start(
        "perf4 D1",
        d1_p.path(),
        &[
            "login", "perf4", "wait", "logged-in", "e2ee-ready", PASSPHRASE, "deafen", "print", "ready", "wait", "call", "incoming", "print", "ringing",
            "idle", "1", "answer", "wait", "call", "connecting|active", "print", "answered", "idle", "8", "dump", "call", "dump", "voice",
            "wait", "call", "idle", "dump", "call", "logout", "wait", "logged-out",
        ],
    );
    d1.until("MARK ready", Duration::from_secs(40));
    let mut d2 = Live::start(
        "perf4 D2",
        d2_p.path(),
        &[
            "login", "perf4", "wait", "logged-in", "e2ee-ready", PASSPHRASE, "deafen", "print", "ready", "wait", "call", "incoming", "print", "ringing",
            "wait", "incoming-ended", "dump", "call", "dump", "cues", "logout", "wait", "logged-out",
        ],
    );
    d2.until("MARK ready", Duration::from_secs(40));
    let caller = Live::start(
        "perf3",
        caller_p.path(),
        &[
            "login", "perf3", "wait", "logged-in", "e2ee-ready", PASSPHRASE, "deafen", "idle", "1", "wait-file", go.to_str().unwrap(), "call", "perf4",
            "wait", "call", "connecting|active", "idle", "9", "dump", "call", "dump", "voice", "hangup", "wait", "call", "idle",
            "dump", "call", "dump", "cues", "logout", "wait", "logged-out",
        ],
    );
    std::fs::write(&go, b"").unwrap();
    d1.until("MARK ringing", Duration::from_secs(40));
    d2.until("MARK ringing", Duration::from_secs(40));
    d1.until("MARK answered", Duration::from_secs(40));
    let d2_seen = d2.finish();
    let caller_seen = caller.finish();
    let d1_seen = d1.finish();
    let dumps = |seen: &[String], what: &str| -> Vec<Value> {
        seen.iter().filter_map(|l| parse_dump(l)).filter(|(n, _)| n == what).map(|(_, v)| v).collect()
    };
    let d2_call = &dumps(&d2_seen, "call")[0];
    assert_eq!(d2_call["status"], "idle");
    assert_eq!(d2_call["incomingEnded"]["reason"], "Answered on another device", "{d2_call}");
    assert_eq!(d2_call["incomingEnded"]["answered"], true);
    let d2_cues = dumps(&d2_seen, "cues")[0].clone();
    assert!(d2_cues.as_array().unwrap().iter().any(|c| c == "StartRing"), "the second device rang: {d2_cues}");
    let caller_mid = &dumps(&caller_seen, "call")[0];
    let d1_mid = &dumps(&d1_seen, "call")[0];
    println!("caller during: {caller_mid}\ncallee during: {d1_mid}");
    let connected = caller_mid["status"] == "active" && d1_mid["status"] == "active";
    if connected {
        assert_eq!(dumps(&caller_seen, "voice")[0]["callPeer"], 9);
        assert_eq!(dumps(&d1_seen, "voice")[0]["callPeer"], 8);
    } else {
        eprintln!("the P2P media did not come up over loopback here (signalling verified): {caller_mid}");
    }
    assert_eq!(dumps(&caller_seen, "call").last().unwrap()["status"], "idle");
    assert_eq!(dumps(&d1_seen, "call").last().unwrap()["status"], "idle", "the hang-up reached the device that answered");
}
