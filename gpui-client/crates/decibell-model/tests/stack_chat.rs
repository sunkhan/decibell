//! The message model driven through the real core against the local test stack (M2;
//! docs/model.md §15). Same rules as `tests/stack.rs`:
//!
//! ```sh
//! eval "$(../tools/dev/local-stack.sh client-env)"     # DECIBELL_CENTRAL=127.0.0.1:18080
//! DECIBELL_STACK=1 cargo test -p decibell-model --test stack_chat -- --nocapture --test-threads 1
//! ```
//!
//! Accounts `perf2`, `perf3` (twice: two devices) only. Posts go to `#off-topic` and
//! `#slow-lane`, never the 10k `#general`, and every test deletes what it posted; DMs too.
//! Each text carries a run id, so a rerun never confuses an earlier run's rows.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::Value;

const HEADLESS: &str = env!("CARGO_BIN_EXE_decibell-headless");
const GAMING: &str = "Daily Gaming";

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

fn run_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    format!("m2t{}x{}", std::process::id(), t % 1_000_000)
}

fn command(profile: &Path, timeout: u32, steps: &[&str]) -> Command {
    let mut command = Command::new(HEADLESS);
    command.arg("--user-data-dir").arg(profile).arg("--timeout").arg(timeout.to_string()).args(steps);
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
    /// Every `dump <what>` in order.
    fn dumps(&self, what: &str) -> Vec<Value> {
        self.stdout.lines().filter_map(parse_dump).filter(|(n, _)| n == what).map(|(_, v)| v).collect()
    }
    fn dump(&self, what: &str) -> Value {
        self.dumps(what).pop().unwrap_or_else(|| panic!("no `dump {what}` in:\n{}\n{}", self.stdout, self.stderr))
    }
}

fn run_ok(profile: &Path, timeout: u32, steps: &[&str]) -> Run {
    let output = command(profile, timeout, steps).output().expect("starting decibell-headless");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let run = Run {
        ok: output.status.success() && stdout.lines().any(|l| l == "DONE"),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        stdout,
    };
    assert!(run.ok, "the script failed:\n--- stdout\n{}\n--- stderr\n{}", run.stdout, run.stderr);
    run
}

struct Live {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl Live {
    fn start(profile: &Path, timeout: u32, steps: &[&str]) -> Live {
        let mut child = command(profile, timeout, steps).stderr(Stdio::inherit()).spawn().expect("starting decibell-headless");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::Builder::new()
            .name("stack-chat-reader".into())
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
        self.until("DONE", Duration::from_secs(90));
        assert!(self.child.wait().expect("waiting for the client").success());
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn ids(chat: &Value) -> Vec<i64> {
    chat["messages"].as_array().unwrap().iter().map(|m| m["id"].as_i64().unwrap()).collect()
}

fn contents(chat: &Value) -> Vec<String> {
    chat["messages"].as_array().unwrap().iter().map(|m| m["content"].as_str().unwrap().to_string()).collect()
}

fn toast_titles(run: &Run) -> Vec<String> {
    run.dump("toasts").as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect()
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// `#general` (the 10k fixture): the newest page, three older pages, then a jump window
/// around an id deep in what was paged, in a fresh client.
#[test]
fn paging_back_through_general_and_a_jump_window() {
    if !stack() {
        return;
    }
    let profile = tempfile::tempdir().unwrap();
    let run = run_ok(
        profile.path(),
        30,
        &[
            "login", "perf2", "wait", "connected", GAMING, "open-server", GAMING,
            "history", "general", "dump", "chat", "general",
            "history", "general", "before", "dump", "chat", "general",
            "history", "general", "before", "history", "general", "before", "dump", "chat", "general",
            "dump", "pump", "dump", "toasts",
        ],
    );
    let pages = run.dumps("chat general");
    let first = &pages[0];
    assert_eq!(first["rows"], 50, "the newest page: 50 rows");
    assert_eq!((first["fetched"].as_bool(), first["hasMoreBefore"].as_bool(), first["hasMoreAfter"].as_bool()), (Some(true), Some(true), Some(false)));
    assert_eq!((first["sorted"].as_bool(), first["unique"].as_bool()), (Some(true), Some(true)));
    // The fixture's 10,000 messages plus the live ones: the newest ids are past 10,000.
    assert!(first["newest"].as_i64().unwrap() > 10_000, "{}", first["newest"]);
    let second = &pages[1];
    assert_eq!(second["rows"], 100);
    assert!(second["oldest"].as_i64() < first["oldest"].as_i64());
    let fourth = &pages[2];
    assert_eq!(fourth["rows"], 200, "four pages of 50, none lost, none twice");
    assert_eq!((fourth["sorted"].as_bool(), fourth["unique"].as_bool(), fourth["hasMoreBefore"].as_bool()), (Some(true), Some(true), Some(true)));
    assert_eq!(fourth["newest"], first["newest"], "older pages never touch the newest rows");
    // Contiguous: every page started where the previous one ended (no row in between is
    // missing: each id of page 1 is still there).
    let all = ids(fourth);
    assert!(ids(first).iter().all(|id| all.contains(id)));
    assert_eq!(toast_titles(&run), Vec::<String>::new());
    // The model's revisions: one change per page (a page is one splice).
    assert!(fourth["revision"].as_u64().unwrap() <= 8, "{}", fourth["revision"]);

    // A fresh client: the newest page, then a window around the oldest id paged above.
    let target = fourth["oldest"].as_i64().unwrap().to_string();
    let profile = tempfile::tempdir().unwrap();
    let jump = run_ok(
        profile.path(),
        30,
        &["login", "perf2", "wait", "connected", GAMING, "open-server", GAMING, "history", "general", "history", "general", "around", &target, "dump", "chat", "general"],
    );
    let window = jump.dump("chat general");
    let ids = ids(&window);
    let at = ids.iter().position(|id| id.to_string() == target).expect("the target is in the window");
    assert!(at > 10 && ids.len() - at > 10, "centred: {at} of {}", ids.len());
    assert_eq!((window["hasMoreBefore"].as_bool(), window["hasMoreAfter"].as_bool()), (Some(true), Some(true)), "windowed both ways");
    assert_eq!((window["sorted"].as_bool(), window["unique"].as_bool()), (Some(true), Some(true)));
    println!("general: newest page {}..{}, four pages {}..{}, window of {} around {target}", first["oldest"], first["newest"], fourth["oldest"], fourth["newest"], ids.len());
}

/// Send, echo, edit and delete in `#off-topic` while a second client watches the live
/// events arrive.
#[test]
fn send_edit_delete_seen_live_by_another_client() {
    if !stack() {
        return;
    }
    let id = run_id();
    let (text, edited) = (format!("{id} hello"), format!("{id} hello, edited"));
    let (watcher_profile, sender_profile) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut watcher = Live::start(
        watcher_profile.path(),
        40,
        &[
            "login", "perf3", "wait", "connected", GAMING, "open-server", GAMING, "history", "off-topic", "print", "watching",
            "wait-message", "off-topic", &text, "dump", "chat", "off-topic", "print", "saw-message",
            "wait-edited", "off-topic", &edited, "dump", "chat", "off-topic", "print", "saw-edit",
            "wait-gone", "off-topic", &edited, "dump", "chat", "off-topic", "print", "saw-delete",
            "logout", "wait", "logged-out",
        ],
    );
    watcher.until("MARK watching", Duration::from_secs(60));
    let sender = run_ok(
        sender_profile.path(),
        40,
        &[
            "login", "perf2", "wait", "connected", GAMING, "open-server", GAMING, "history", "off-topic",
            "send", "off-topic", &text, "wait-sent", "off-topic", "wait-message", "off-topic", &text, "dump", "chat", "off-topic",
            "edit", "off-topic", "last", &edited, "wait-edited", "off-topic", &edited,
            "delete", "off-topic", "last", "wait-gone", "off-topic", &edited, "wait-deleted", "off-topic",
            "dump", "chat", "off-topic", "dump", "toasts",
            "logout", "wait", "logged-out",
        ],
    );
    let sent = &sender.dumps("chat off-topic")[0];
    let row = sent["messages"].as_array().unwrap().iter().find(|m| m["content"] == text.as_str()).expect("our echo");
    assert_eq!((row["pending"].as_bool(), row["own"].as_bool()), (Some(false), Some(true)), "the echo replaced the bubble");
    assert!(row["id"].as_i64().unwrap() > 0);
    assert_eq!(contents(&sender.dump("chat off-topic")).iter().filter(|c| c.starts_with(&id)).count(), 0, "deleted");
    assert_eq!(toast_titles(&sender), Vec::<String>::new());

    watcher.until("MARK saw-message", Duration::from_secs(60));
    let seen = watcher.last_dump("chat off-topic");
    let row = seen["messages"].as_array().unwrap().iter().find(|m| m["content"] == text.as_str()).unwrap().clone();
    assert_eq!((row["own"].as_bool(), row["pending"].as_bool()), (Some(false), Some(false)));
    watcher.until("MARK saw-edit", Duration::from_secs(60));
    let row = watcher.last_dump("chat off-topic")["messages"].as_array().unwrap().iter().find(|m| m["content"] == edited.as_str()).unwrap().clone();
    assert_eq!(row["edited"], true);
    watcher.until("MARK saw-delete", Duration::from_secs(60));
    watcher.finish();
}

/// Fifteen sends at once: the first ten go out, the rest are paced (2.7 a second), none is
/// refused or dropped.
#[test]
fn a_burst_of_fifteen_is_paced_not_dropped() {
    if !stack() {
        return;
    }
    let id = run_id();
    let profile = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let run = run_ok(
        profile.path(),
        40,
        &[
            "login", "perf2", "wait", "connected", GAMING, "open-server", GAMING, "history", "off-topic",
            "send-burst", "off-topic", "15", &id, "dump", "chat", "off-topic", "wait-sent", "off-topic", "dump", "chat", "off-topic", "dump", "toasts",
            "delete", "off-topic", &format!("prefix:{id}"), "wait-deleted", "off-topic",
            // A fresh newest page: what the server holds, not what we removed locally.
            "history", "off-topic", "dump", "chat", "off-topic",
            "logout", "wait", "logged-out",
        ],
    );
    let queued = &run.dumps("chat off-topic")[0];
    assert_eq!(queued["queued"], 5, "ten admitted at once, five waiting on the bucket");
    let after = &run.dumps("chat off-topic")[1];
    let ours: Vec<String> = contents(after).into_iter().filter(|c| c.starts_with(&id)).collect();
    assert_eq!(ours.len(), 15, "every send arrived: {ours:?}");
    for n in 1..=15 {
        assert!(ours.contains(&format!("{id} {n}")), "missing {n}");
    }
    // In order.
    let order: Vec<u32> = ours.iter().map(|c| c.rsplit(' ').next().unwrap().parse().unwrap()).collect();
    assert_eq!(order, (1..=15).collect::<Vec<_>>());
    assert!(!toast_titles(&run).iter().any(|t| t == "Message not sent"), "{:?}", toast_titles(&run));
    assert_eq!(contents(&run.dumps("chat off-topic")[2]).iter().filter(|c| c.starts_with(&id)).count(), 0, "cleaned up");
    println!("burst of 15 delivered and cleaned up in {:?}", started.elapsed());
}

/// `#slow-lane` (30 s slowmode): after our message the next send is held (its text kept),
/// and once the window passed it goes out.
#[test]
fn slowmode_holds_the_second_send_then_delivers_it() {
    if !stack() {
        return;
    }
    let id = run_id();
    let (one, two) = (format!("{id} first"), format!("{id} second"));
    let profile = tempfile::tempdir().unwrap();
    let run = run_ok(
        profile.path(),
        45,
        &[
            "login", "perf2", "wait", "connected", GAMING, "open-server", GAMING, "history", "slow-lane",
            "send", "slow-lane", &one, "wait-sent", "slow-lane", "wait-message", "slow-lane", &one,
            "wait-held", "slow-lane", "send", "slow-lane", &two, "print", "held",
            "wait-free", "slow-lane", "send", "slow-lane", &two, "wait-sent", "slow-lane", "wait-message", "slow-lane", &two,
            "dump", "chat", "slow-lane", "dump", "toasts",
            "delete", "slow-lane", &format!("prefix:{id}"), "wait-deleted", "slow-lane",
            "history", "slow-lane", "dump", "chat", "slow-lane",
            "logout", "wait", "logged-out",
        ],
    );
    let lines: Vec<&str> = run.stdout.lines().collect();
    let held_at = lines.iter().position(|l| *l == "MARK held").unwrap();
    assert!(lines[held_at - 1].starts_with("NOT-SENT slow-lane HeldBySlowmode"), "{}", lines[held_at - 1]);
    let chat = &run.dumps("chat slow-lane")[0];
    let ours: Vec<String> = contents(chat).into_iter().filter(|c| c.starts_with(&id)).collect();
    assert_eq!(ours, vec![one, two]);
    assert_eq!(contents(&run.dump("chat slow-lane")).iter().filter(|c| c.starts_with(&id)).count(), 0, "cleaned up");
    assert!(!toast_titles(&run).iter().any(|t| t == "Message not sent"), "the server never had to refuse it");
}

/// A DM from perf2 to perf3: perf3's first device counts it unread; perf3's second device
/// reads it, and the first sees DM_READ_SYNC clear its count.
#[test]
fn a_dm_with_unread_and_read_sync_across_devices() {
    if !stack() {
        return;
    }
    let id = run_id();
    let text = format!("{id} hi there");
    let (a, b, c) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // perf3, device 1: not looking at the conversation.
    let mut device1 = Live::start(
        b.path(),
        40,
        &[
            "login", "perf3", "wait", "logged-in", "print", "ready",
            "wait-dm", "perf2", &text, "dump", "dm", "perf2", "print", "unread",
            "wait-unread", "perf2", "0", "dump", "dm", "perf2", "print", "synced",
            "logout", "wait", "logged-out",
        ],
    );
    device1.until("MARK ready", Duration::from_secs(60));
    // The previews may not have come yet: let the conversation list settle first.
    std::thread::sleep(Duration::from_secs(2));
    let sender = run_ok(
        a.path(),
        40,
        &["login", "perf2", "wait", "logged-in", "dm-open", "perf3", "dm-send", "perf3", &text, "wait-dm", "perf3", &text, "dump", "dm", "perf3", "logout", "wait", "logged-out"],
    );
    let mine = sender.dump("dm perf3");
    let row = mine["messages"].as_array().unwrap().iter().find(|m| m["content"] == text.as_str()).expect("our echo").clone();
    assert_eq!((row["pending"].as_bool(), row["own"].as_bool()), (Some(false), Some(true)));
    device1.until("MARK unread", Duration::from_secs(60));
    let unread = device1.last_dump("dm perf2");
    assert!(unread["unread"].as_i64().unwrap() >= 1, "{unread}");
    assert_eq!(unread["lastMessage"]["content"], text.as_str());
    // perf3, device 2: opens and reads it.
    run_ok(c.path(), 40, &["login", "perf3", "wait", "logged-in", "dm-open", "perf2", "wait-dm", "perf2", &text, "dm-read", "perf2", "dump", "dm", "perf2", "logout", "wait", "logged-out"]);
    device1.until("MARK synced", Duration::from_secs(60));
    let synced = device1.last_dump("dm perf2");
    assert_eq!(synced["unread"], 0);
    assert!(synced["lastReadId"].as_i64().unwrap() >= row["id"].as_i64().unwrap());
    device1.finish();
    // Clean up the DM.
    let prefix = format!("prefix:{id}");
    run_ok(
        a.path(),
        40,
        &["login", "perf2", "wait", "logged-in", "dm-open", "perf3", "dm-delete", "perf3", &prefix, "wait-dm-gone", "perf3", &text, "wait-dm-deleted", "perf3", "logout", "wait", "logged-out"],
    );
}

/// A server-side search for a word the fixture uses.
#[test]
fn search_finds_a_fixture_word() {
    if !stack() {
        return;
    }
    let profile = tempfile::tempdir().unwrap();
    let run = run_ok(
        profile.path(),
        30,
        &["login", "perf2", "wait", "connected", GAMING, "open-server", GAMING, "search", "wayland", "dump", "search", "logout", "wait", "logged-out"],
    );
    let search = run.dump("search");
    assert_eq!(search["status"], "done");
    let results = search["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert!(search["total"].as_i64().unwrap() >= results.len() as i64);
    for r in results {
        assert!(r["content"].as_str().unwrap().to_lowercase().contains("wayland"), "{r}");
    }
    assert!(search["textSkipped"].as_array().unwrap().iter().any(|c| c.as_str().is_some()), "the encrypted vault is reported: {search}");
    let mut by_channel: HashMap<String, usize> = HashMap::new();
    for r in results {
        *by_channel.entry(r["channel"].as_str().unwrap().to_string()).or_default() += 1;
    }
    println!("search \"wayland\": total {} ({}), first page by channel {by_channel:?}", search["total"], if search["totalCapped"] == true { "capped" } else { "exact" });
}
