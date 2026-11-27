//! The voice feature's self-tests, against the local test stack (`tools/dev/README.md`) as
//! `perf3`, with a `decibell-headless` peer as `perf4` (both have E2EE keys: MLS voice needs
//! them; passphrase `decibell-m3-test-passphrase`).
//!
//! Each `voice-*` / `call-*` test starts the app again in a child process (`decibell --check <name>
//! --login perf3`, its own temporary profile, `DECIBELL_TEST_PROBES=1`), as the shell's do, and the
//! check inside it starts the headless peer when it needs one. They refuse unless `DECIBELL_CENTRAL`
//! is loopback, skip (saying so) when the stack is down, the binary has no `--login` (`dev-local`)
//! or `decibell-headless` isn't built next to it.
//!
//! **Nothing is heard and the real microphone stays closed.** Each parent makes a PipeWire null
//! sink (`decibell_harness::test_audio`), names it to its child in `DECIBELL_CUE_SINK`, and the
//! check saves it as the output device and its monitor as the input device before anything joins
//! ([`ready`]): the session's streams, and the interface sounds the core mixes into its output,
//! sit on the sink ([`join`] asserts where the streams are). The sink is unloaded by the parent
//! after the child has gone, so no stream can be moved to the real output. Without a sink (no
//! `pactl`) the app's player hands the core no sound at all (`decibell-app/src/audio.rs`), the
//! sound checks skip and the session uses the desktop's devices, muted and deafened, as before.
//! The app is muted and deafened before it joins (the engine keeps the state for the session),
//! the peer joins muted and deafened and plays no sounds (the headless client has no player),
//! the streamer sends video only. Every process the checks start is killed when the check ends,
//! pass or fail.

use std::io::BufRead as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{Frames, SelfTest, TestCx, test_audio};
use decibell_core::api::audio;
use decibell_model::live::calls::CallStatus;
use decibell_model::live::presence::OwnStatus;
use decibell_model::live::{Cue, Sound};
use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
use decibell_model::live::voice::session::VoiceE2eePhase;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_shell::probes;
use gpui::{App, Bounds, Pixels, point};

const ACCOUNT: &str = "perf3";
const PEER: &str = "perf4";
const PASSPHRASE: &str = "decibell-m3-test-passphrase";
const COMMUNITY: &str = "Daily Gaming";
/// The stack's voice channels of Daily Gaming (ids as seeded: `tools/dev/seed.py`).
const LOUNGE: &str = "voice-lounge";
const SQUAD: &str = "voice-lounge-2";

// ── the parents ─────────────────────────────────────────────────────────────────────────

const TESTS: &[(&str, &str)] = &[
    ("voice-join", "join Lounge from the sidebar: our tile; a headless peer joins and leaves; frames only on those changes"),
    ("voice-speaking-idle", "in Lounge with a quiet peer: 0 frames drawn over 10 s (unfocused, then focused)"),
    ("voice-mute", "the dock's mute / deafen reach the model and the peer"),
    ("voice-watch", "a headless peer streams a .dbv recording: watched through the whole path at its rate, 0 dropped; mini-player; stop frees the decoder"),
    ("voice-golive-dialog", "the Go Live dialog opens, presets switch, software-only caps grey presets out (no capture)"),
    ("call-ring", "perf4 calls: the card rings and the ring loops on the test sink until declined; again under Do Not Disturb: the card, no ring; answer, the call stage connects (P2P over loopback) and takes the microphone from a running mic test, hang up"),
    ("voice-cues", "in Lounge on a null sink: a sound and the looping ring play in the session's own output stream (no second stream, no cue thread) and show on the session's input meter; the mic test is refused; nothing is left open after leaving"),
    ("voice-switch", "switching voice channels shows us in the new one at once"),
    ("voice-stall", "watching a stream on the decoder the probe found; fullscreen hides the cursor with the overlays; the streamer stops sending: Reconnecting… after 3 s, gone with the next picture"),
];

fn about(name: &str) -> &'static str {
    TESTS.iter().find(|(n, _)| *n == name).map(|(_, a)| *a).unwrap_or("")
}

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "voice-join", about: about("voice-join"), run: |cx| Box::pin(parent(cx, "voice-join")) },
        SelfTest { name: "voice-speaking-idle", about: about("voice-speaking-idle"), run: |cx| Box::pin(parent(cx, "voice-speaking-idle")) },
        SelfTest { name: "voice-mute", about: about("voice-mute"), run: |cx| Box::pin(parent(cx, "voice-mute")) },
        SelfTest { name: "voice-watch", about: about("voice-watch"), run: |cx| Box::pin(parent(cx, "voice-watch")) },
        SelfTest { name: "voice-golive-dialog", about: about("voice-golive-dialog"), run: |cx| Box::pin(parent(cx, "voice-golive-dialog")) },
        SelfTest { name: "call-ring", about: about("call-ring"), run: |cx| Box::pin(parent(cx, "call-ring")) },
        SelfTest { name: "voice-cues", about: about("voice-cues"), run: |cx| Box::pin(parent(cx, "voice-cues")) },
        SelfTest { name: "voice-switch", about: about("voice-switch"), run: |cx| Box::pin(parent(cx, "voice-switch")) },
        SelfTest { name: "voice-stall", about: about("voice-stall"), run: |cx| Box::pin(parent(cx, "voice-stall")) },
    ]
}

/// The stack as `accounts.json` describes it.
struct Stack {
    central: String,
    password: String,
}

fn dev_dir() -> PathBuf {
    std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"))
}

fn json_str(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = text.find(&needle)? + needle.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

fn stack() -> Result<Option<Stack>> {
    let path = dev_dir().join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| json_str(&text, "central")).context("no central address")?;
    let password = json_str(&text, "password").context("accounts.json has no password")?;
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h);
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: the voice tests run against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
}

/// A temporary profile, removed when dropped.
struct Profile(PathBuf);

impl Profile {
    fn new(name: &str) -> Result<Profile> {
        let dir = std::env::temp_dir().join(format!("decibell-{name}-{}-{}", std::process::id(), rand_tag()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Profile(dir))
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rand_tag() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as u64).unwrap_or(0)
}

/// `decibell-headless` next to this binary.
fn headless_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("decibell-headless");
    candidate.exists().then_some(candidate)
}

async fn parent(cx: TestCx, name: &'static str) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    if headless_exe().is_none() {
        cx.log("SKIP: decibell-headless is not built next to this binary (cargo build -p decibell-model --features local-central --bin decibell-headless, same profile)");
        return Ok(());
    }
    let profile = Profile::new(name)?;
    // The session's sound goes to a null sink this process owns: unloaded only after the child
    // has gone (a stream whose sink disappears would be moved to the real output).
    let sink = match test_audio::NullSink::load("") {
        Ok(sink) => {
            cx.log(format_args!("null sink {} (module {})", sink.name(), sink.module()));
            Some(sink)
        }
        Err(e) => {
            cx.log(format_args!("no null sink ({e:#}): sounds are not played, the sound checks skip"));
            None
        }
    };
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command.env_remove(CUE_SINK_ENV);
    if let Some(sink) = &sink {
        command.env(CUE_SINK_ENV, sink.name());
    }
    command
        .arg("--check")
        .arg(name)
        .arg("--login")
        .arg(ACCOUNT)
        .env("DECIBELL_USER_DATA_DIR", &profile.0)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env_remove("DECIBELL_PERF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().context("starting the app")?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (status, out, err) = cx
        .app()
        .background_executor()
        .spawn(async move {
            let err = std::thread::Builder::new()
                .name("voice-test-stderr".into())
                .spawn(move || std::io::BufReader::new(stderr).lines().map_while(Result::ok).collect::<Vec<_>>())
                .ok();
            let out: Vec<String> = std::io::BufReader::new(stdout).lines().map_while(Result::ok).collect();
            let status = child.wait();
            (status, out, err.and_then(|h| h.join().ok()).unwrap_or_default())
        })
        .await;
    let status = status?;
    // The child has ended: its streams are closed.
    let unloaded = sink.map(test_audio::NullSink::unload);
    for line in out.iter().filter(|l| !l.starts_with("selftest:")) {
        cx.log(format_args!("│ {line}"));
    }
    if err.iter().any(|l| l.contains("--login refused")) && err.iter().any(|l| l.contains("dev-local")) {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    }
    if !status.success() {
        for line in err.iter().rev().take(16).rev() {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{name} failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    if let Some(unloaded) = unloaded {
        unloaded?;
        cx.log("null sink unloaded; the default devices are as they were");
    }
    Ok(())
}

/// The sink a check may play on (`decibell-app/src/audio.rs`): the parent's null sink.
const CUE_SINK_ENV: &str = "DECIBELL_CUE_SINK";

fn test_sink() -> Option<String> {
    std::env::var(CUE_SINK_ENV).ok().filter(|s| !s.is_empty() && s != "off")
}

/// Every stream of this process is on the test sink / its monitor (nothing to check without one).
fn on_test_sink(what: &str) -> Result<test_audio::Streams> {
    match test_sink() {
        Some(sink) => test_audio::ensure_only_on(&sink).with_context(|| format!("while {what}")),
        None => Ok(test_audio::Streams::default()),
    }
}

// ── the checks, inside the signed-in app ────────────────────────────────────────────────

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "voice-join", about: "(in the app) join, a peer comes and goes", run: |cx| Box::pin(check_join(cx)) },
        SelfTest { name: "voice-speaking-idle", about: "(in the app) idle in voice", run: |cx| Box::pin(check_idle(cx)) },
        SelfTest { name: "voice-mute", about: "(in the app) the dock's mute / deafen", run: |cx| Box::pin(check_mute(cx)) },
        SelfTest { name: "voice-watch", about: "(in the app) watch a real stream", run: |cx| Box::pin(check_watch(cx)) },
        SelfTest { name: "voice-golive-dialog", about: "(in the app) the Go Live dialog", run: |cx| Box::pin(check_golive(cx)) },
        SelfTest { name: "call-ring", about: "(in the app) an incoming call", run: |cx| Box::pin(check_call(cx)) },
        SelfTest { name: "voice-cues", about: "(in the app) sounds in a voice session", run: |cx| Box::pin(check_cues(cx)) },
        SelfTest { name: "voice-switch", about: "(in the app) switching channels", run: |cx| Box::pin(check_switch(cx)) },
        SelfTest { name: "voice-stall", about: "(in the app) the probed decoder, the fullscreen cursor, a stalled stream", run: |cx| Box::pin(check_stall(cx)) },
    ]
}

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

fn daily_gaming() -> ServerId {
    ServerId::from_central(1)
}

/// Poll `ready` every 50 ms for at most `limit`.
async fn wait_for(cx: &TestCx, limit: Duration, what: &str, ready: impl Fn(&mut App) -> bool) -> Result<Duration> {
    let started = Instant::now();
    loop {
        if cx.update(|_, cx| ready(cx))? {
            return Ok(started.elapsed());
        }
        ensure!(started.elapsed() < limit, "timed out after {} s waiting for {what}", limit.as_secs());
        cx.sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_probe(cx: &TestCx, key: &str) -> Result<Bounds<Pixels>> {
    let owned = key.to_string();
    wait_for(cx, Duration::from_secs(15), &format!("{key} on screen"), move |_| probes::bounds(&owned).is_some()).await?;
    probes::bounds(key).context("probe vanished")
}

/// Rest the pointer where nothing reacts to it: the middle of the voice header's empty stretch,
/// else the middle of the channel header (no hover, no tooltip there).
fn park_pointer(cx: &TestCx) -> Result<()> {
    let at = probes::bounds("voice-header")
        .filter(|_| probes::flag_set("voice-pane"))
        .map(|b| point(b.left() + b.size.width * 0.6, b.center().y))
        .unwrap_or_else(|| point(gpui::px(700.), gpui::px(128.)));
    cx.mouse_move(at)
}

async fn click_probe(cx: &TestCx, key: &str) -> Result<()> {
    let bounds = wait_probe(cx, key).await?;
    cx.click(bounds.center())?;
    park_pointer(cx)
}

/// Waits until nothing has been drawn for 1 s and returns the counters.
async fn settle_drawn(cx: &TestCx) -> Result<Frames> {
    let started = Instant::now();
    let mut last = cx.frames()?;
    let mut quiet_since = Instant::now();
    loop {
        cx.sleep(Duration::from_millis(50)).await;
        let now = cx.frames()?;
        if now.drawn != last.drawn {
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= Duration::from_secs(1) {
            return Ok(now);
        }
        last = now;
        ensure!(started.elapsed() < Duration::from_secs(20), "the window was still drawing after 20 s");
    }
}

/// One change: the frames it drew, then a second without one.
async fn frames_of(cx: &TestCx, what: &str, act: impl AsyncFnOnce(&TestCx) -> Result<()>) -> Result<Frames> {
    let before = settle_drawn(cx).await?;
    act(cx).await?;
    let after = settle_drawn(cx).await?;
    let frames = after.since(before);
    cx.sleep(Duration::from_secs(1)).await;
    let idle = cx.frames()?.since(after);
    cx.log(format_args!("{what}: {} drawn / {} presented; then {} drawn in 1 s", frames.drawn, frames.presented, idle.drawn));
    Ok(frames)
}

/// Signed in, E2EE unlocked, Daily Gaming connected and open, and muted + deafened (before any
/// voice session: nothing is captured or played).
async fn ready(cx: &TestCx) -> Result<()> {
    wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    // Unlock the identity keys (the passphrase dialog is another feature's; the check types into
    // the model as the dialog would).
    let started = Instant::now();
    loop {
        let status = cx.update(|_, cx| model(cx).live.e2ee.read(cx).status)?;
        match status {
            E2eeStatus::Ready => break,
            E2eeStatus::Locked | E2eeStatus::NotSetUp => {
                let busy = cx.update(|_, cx| model(cx).live.e2ee.read(cx).passphrase.as_ref().is_some_and(|f| f.busy))?;
                if !busy {
                    let mode = if status == E2eeStatus::Locked { PassphraseMode::Unlock } else { PassphraseMode::Setup };
                    cx.update(|_, cx| {
                        let m = model(cx);
                        m.live.e2ee.update(cx, |e, cx| e.open(mode, cx));
                        actions::e2ee::submit_passphrase(PASSPHRASE.into(), PASSPHRASE.into(), cx);
                    })?;
                }
            }
            E2eeStatus::Unavailable => {}
        }
        ensure!(started.elapsed() < Duration::from_secs(30), "the E2EE keys never became ready ({status:?})");
        cx.sleep(Duration::from_millis(100)).await;
    }
    // The passphrase dialog (the settings crate's overlay) is open for a locked identity and takes
    // every click until it has closed and faded out (300 ms).
    wait_for(cx, Duration::from_secs(10), "the passphrase dialog to close", |cx| model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
    cx.sleep(Duration::from_millis(400)).await;
    // The session's devices: the parent's null sink and its monitor, never the desktop's.
    if let Some(sink) = test_sink() {
        cx.update(move |_, cx| {
            actions::voice::set_input_device(Some(format!("{sink}.monitor")), cx);
            actions::voice::set_output_device(Some(sink), cx);
        })?;
    }
    cx.update(|_, cx| {
        actions::voice::set_mute(true, cx);
        actions::voice::set_deafen(true, cx);
    })?;
    // The passphrase dialog (the settings feature's, open while `e2ee.passphrase` is) covers the
    // window until it has closed and faded: a click on the server tile before that lands on its
    // backdrop. So: wait for the form to go, then click until the navigation follows.
    wait_for(cx, Duration::from_secs(10), "the passphrase dialog to close", |cx| model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
    let opened = |cx: &mut App| {
        let m = model(cx);
        m.nav.read(cx).active_server == Some(daily_gaming()) && m.community(daily_gaming(), cx).is_some_and(|c| !c.channels.read(cx).list().is_empty())
    };
    let started = Instant::now();
    loop {
        click_probe(cx, "server-tile:1").await?;
        if wait_for(cx, Duration::from_secs(1), "Daily Gaming's channels", opened).await.is_ok() {
            return Ok(());
        }
        ensure!(started.elapsed() < Duration::from_secs(15), "timed out after 15 s waiting for Daily Gaming's channels");
    }
}

/// An account's uid from the stack's `accounts.json` (the stack's uids are fixed).
fn account_uid(name: &str) -> Result<Uid> {
    let text = std::fs::read_to_string(dev_dir().join("run/accounts.json"))?;
    let needle = format!("\"name\": \"{name}\"");
    let at = text.find(&needle).with_context(|| format!("{name} is not in accounts.json"))?;
    let rest = &text[at..];
    let uid_at = rest.find("\"uid\"").context("no uid")? + 5;
    let digits: String = rest[uid_at..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    Ok(digits.parse()?)
}

fn me(cx: &TestCx) -> Result<Uid> {
    cx.update(|_, cx| model(cx).auth.read(cx).uid)
}

fn in_channel(cx: &mut App, channel: &str, uid: Uid) -> bool {
    let key = (daily_gaming(), decibell_model::ChannelId::from(channel));
    model(cx).live.voice.rosters.read(cx).roster(&key).contains(&uid)
}

/// Click the voice channel in the sidebar and wait until we're connected with MLS ready.
async fn join(cx: &TestCx, channel: &str) -> Result<()> {
    click_probe(cx, &format!("voice:{channel}")).await?;
    let ch = channel.to_string();
    let t = wait_for(cx, Duration::from_secs(20), "the voice session with MLS ready", move |cx| {
        let s = model(cx).live.voice.session.read(cx);
        s.is_connected_to(daily_gaming(), &ch) && s.e2ee.as_ref().is_some_and(|e| e.phase == VoiceE2eePhase::Ready)
    })
    .await?;
    cx.log(format_args!("in {channel}, MLS ready after {} ms", t.as_millis()));
    if test_sink().is_some() {
        session_on_test_sink(cx, "in the voice session").await?;
    }
    Ok(())
}

/// The session's playback and capture streams are open and linked, all on the test sink and
/// its monitor (they open a moment after the session starts).
async fn session_on_test_sink(cx: &TestCx, what: &str) -> Result<test_audio::Streams> {
    let started = Instant::now();
    loop {
        let streams = on_test_sink(what)?;
        if streams.unlinked == 0 && !streams.sinks.is_empty() && !streams.sources.is_empty() {
            return Ok(streams);
        }
        ensure!(started.elapsed() < Duration::from_secs(5), "{what}: the session's streams never appeared on the test sink ({streams:?})");
        cx.sleep(Duration::from_millis(50)).await;
    }
}

async fn leave(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::voice::leave_voice_channel(cx))?;
    wait_for(cx, Duration::from_secs(10), "leaving voice", |cx| !model(cx).live.voice.session.read(cx).media_session_active()).await?;
    Ok(())
}

/// Counts what may legitimately redraw a quiet voice view: the latency readout changing (the
/// header and the user panel show it; a ping lands every ~2 s), the channel's MLS state (epoch /
/// members in the badge's title), and any change of the voice rosters, the session's
/// participants, the streams or a watched person's presence (what the stage and the sidebar
/// draw). Started before a window, read after it.
/// Notifications per model store, by name.
type StoreCounts = std::rc::Rc<std::cell::RefCell<std::collections::BTreeMap<&'static str, u32>>>;

struct Changes {
    latency: std::rc::Rc<std::cell::Cell<u32>>,
    e2ee: std::rc::Rc<std::cell::Cell<u32>>,
    other: std::rc::Rc<std::cell::Cell<u32>>,
    _subs: Vec<gpui::Subscription>,
}

impl Changes {
    fn start(cx: &TestCx) -> Result<Changes> {
        cx.update(|_, cx| {
            let m = model(cx);
            let latency = std::rc::Rc::new(std::cell::Cell::new(0));
            let e2ee = std::rc::Rc::new(std::cell::Cell::new(0));
            let other = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut last_latency = m.live.voice.stats.read(cx).latency_ms;
            let mut last_e2ee = m.live.voice.session.read(cx).e2ee.clone();
            let bump = |c: &std::rc::Rc<std::cell::Cell<u32>>| {
                let c = c.clone();
                move || c.set(c.get() + 1)
            };
            let (b1, b2, b3, b4) = (bump(&other), bump(&other), bump(&other), bump(&other));
            let subs = vec![
                cx.observe(&m.live.voice.stats, {
                    let latency = latency.clone();
                    move |stats, cx| {
                        let now = stats.read(cx).latency_ms;
                        if now != last_latency {
                            last_latency = now;
                            latency.set(latency.get() + 1);
                        }
                    }
                }),
                cx.observe(&m.live.voice.session, {
                    let e2ee = e2ee.clone();
                    move |session, cx| {
                        let now = session.read(cx).e2ee.clone();
                        if now != last_e2ee {
                            last_e2ee = now;
                            e2ee.set(e2ee.get() + 1);
                        }
                    }
                }),
                cx.observe(&m.live.voice.participants, move |_, _| b1()),
                cx.observe(&m.live.voice.rosters, move |_, _| b2()),
                cx.observe(&m.live.voice.streams, move |_, _| b3()),
                cx.observe(&m.live.presence.statuses, move |_, _| b4()),
            ];
            Changes { latency, e2ee, other, _subs: subs }
        })
    }

    fn count(&self) -> u32 {
        self.latency.get() + self.e2ee.get() + self.other.get()
    }

    /// Every store of the model that notified meanwhile (a frame drawn with nothing on screen
    /// changed has a cause among them: a store read while drawing notifies, GPUI draws).
    fn watch_all(cx: &TestCx) -> Result<(StoreCounts, Vec<gpui::Subscription>)> {
        let counts: StoreCounts = Default::default();
        let subs = cx.update(|_, cx| {
            let m = model(cx);
            let v = &m.live.voice;
            let mut subs = Vec::new();
            macro_rules! watch {
                ($($name:literal => $entity:expr),* $(,)?) => {
                    $(
                        let c = counts.clone();
                        subs.push(cx.observe(&$entity, move |_, _| *c.borrow_mut().entry($name).or_default() += 1));
                    )*
                };
            }
            watch!(
                "auth" => m.auth, "users" => m.users, "nav" => m.nav, "overlays" => m.overlays, "settings" => m.settings,
                "layout" => m.layout, "servers" => m.servers, "connections" => m.connections, "communities" => m.communities,
                "toasts" => m.toasts, "update" => m.update, "answers" => m.answers,
                "voice.session" => v.session, "voice.participants" => v.participants, "voice.rosters" => v.rosters,
                "voice.speaking" => v.speaking, "voice.input_level" => v.input_level, "voice.stats" => v.stats,
                "voice.streams" => v.streams, "voice.thumbnails" => v.thumbnails, "voice.watching" => v.watching,
                "voice.stream_views" => v.stream_views, "voice.own_stream" => v.own_stream, "voice.devices" => v.devices,
                "voice.codecs" => v.codecs, "calls" => m.live.calls, "presence.statuses" => m.live.presence.statuses,
                "presence.messages" => m.live.presence.messages, "presence.own" => m.live.presence.own,
                "presence.interest" => m.live.presence.interest, "presence.auto_away" => m.live.presence.auto_away,
                "presence.avatars" => m.live.presence.avatars, "friends" => m.live.friends, "e2ee" => m.live.e2ee,
                "hotkeys" => m.live.hotkeys, "devices" => m.live.devices, "transfers" => m.live.transfers,
                "cues" => m.live.cues, "notices" => m.live.notices,
            );
            subs
        })?;
        Ok((counts, subs))
    }

    fn describe(&self) -> String {
        format!(
            "{} latency changes, {} MLS state changes, {} roster / participant / stream / presence changes",
            self.latency.get(),
            self.e2ee.get(),
            self.other.get()
        )
    }
}

// ── the headless peer ───────────────────────────────────────────────────────────────────

/// A `decibell-headless` process as `perf4`, killed when dropped.
struct Peer {
    child: Child,
    lines: mpsc::Receiver<String>,
    cues: PathBuf,
    _profile: Profile,
}

impl Peer {
    /// Signs in, unlocks, and runs `steps` (cue files are `<cues>/<name>`).
    fn start(steps: &[String]) -> Result<Peer> {
        let exe = headless_exe().context("decibell-headless not built")?;
        let profile = Profile::new("voice-peer")?;
        let cues = profile.0.join("cues");
        std::fs::create_dir_all(&cues)?;
        let mut args: Vec<String> = vec!["--user-data-dir".into(), profile.0.display().to_string(), "--timeout".into(), "60".into()];
        args.extend(
            ["login", PEER, "wait", "connected", COMMUNITY, "open-server", COMMUNITY, "e2ee-ready", PASSPHRASE, "mute", "deafen"]
                .map(String::from),
        );
        args.extend(steps.iter().map(|s| s.replace("@cue:", &format!("{}/", cues.display()))));
        let mut child = Command::new(exe)
            .args(&args)
            .env("DECIBELL_CENTRAL", std::env::var("DECIBELL_CENTRAL").unwrap_or_default())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting decibell-headless")?;
        let stdout = child.stdout.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name("voice-test-peer".into())
            .spawn(move || {
                for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Peer { child, lines, cues, _profile: profile })
    }

    /// Wait for a line starting with `prefix` (a `MARK`, `STREAMING`, `DONE`); FAILED fails.
    async fn wait_line(&self, cx: &TestCx, prefix: &str, limit: Duration) -> Result<String> {
        let started = Instant::now();
        loop {
            while let Ok(line) = self.lines.try_recv() {
                if line.starts_with(prefix) {
                    return Ok(line);
                }
                if line.starts_with("FAILED") {
                    bail!("the peer failed: {line}");
                }
            }
            ensure!(started.elapsed() < limit, "the peer never printed {prefix:?}");
            cx.sleep(Duration::from_millis(50)).await;
        }
    }

    fn cue(&self, name: &str) -> Result<()> {
        std::fs::write(self.cues.join(name), b"")?;
        Ok(())
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn steps(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ── voice-join ──────────────────────────────────────────────────────────────────────────

async fn check_join(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let me = me(&cx)?;
    frames_of(&cx, "joining Lounge from the sidebar", async |cx| join(cx, LOUNGE).await).await?;
    ensure!(cx.update(|_, cx| model(cx).nav.read(cx).active_view == ActiveView::Voice)?, "the voice view didn't open");
    wait_probe(&cx, &format!("participant-tile:{me}")).await?;
    cx.log("our tile is on the stage");
    let peer = Peer::start(&steps(&[
        "print", "ready", "wait-file", "@cue:join", "voice-join", "Lounge", "wait", "mls", "ready", "print", "joined",
        "wait-file", "@cue:leave", "voice-leave", "print", "left", "wait-file", "@cue:quit", "logout",
    ]))?;
    let peer_uid = account_uid(PEER)?;
    peer.wait_line(&cx, "MARK ready", Duration::from_secs(60)).await?;
    let joined = frames_of(&cx, "the peer joins", async |cx| {
        peer.cue("join")?;
        peer.wait_line(cx, "MARK joined", Duration::from_secs(60)).await?;
        wait_probe(cx, &format!("participant-tile:{peer_uid}")).await?;
        Ok(())
    })
    .await?;
    ensure!(joined.drawn >= 1, "the peer's tile drew nothing");
    let left = frames_of(&cx, "the peer leaves", async |cx| {
        peer.cue("leave")?;
        peer.wait_line(cx, "MARK left", Duration::from_secs(30)).await?;
        wait_for(cx, Duration::from_secs(10), "the peer's tile to go", move |cx| !in_channel(cx, LOUNGE, peer_uid)).await?;
        Ok(())
    })
    .await?;
    ensure!(left.drawn >= 1, "the peer's leaving drew nothing");
    // At rest: nothing drawn beyond what changed. Another window taking or giving the desktop's
    // focus redraws ours (the window's activation is in the frame): such a try is taken again.
    let mut last = String::new();
    let mut result = Err(decibell_harness::inconclusive("no try"));
    for attempt in 1..=3 {
        let before = settle_drawn(&cx).await?;
        let changes = Changes::start(&cx)?;
        let (stores, _all) = Changes::watch_all(&cx)?;
        let active = cx.update(|window, _| window.is_window_active())?;
        log::info!("[voice-test] idle window starts");
        let disturbed = cx.watched_sleep(Duration::from_secs(5)).await?;
        log::info!("[voice-test] idle window ends");
        let idle = cx.frames()?.since(before);
        let notified: Vec<String> = stores.borrow().iter().map(|(k, n)| format!("{k} {n}")).collect();
        last = format!(
            "try {attempt}: then {} drawn / {} presented in 5 s; {}; stores that notified: [{}]; window active {active}{}",
            idle.drawn,
            idle.presented,
            changes.describe(),
            notified.join(", "),
            disturbed.as_ref().map_or(String::new(), |d| format!(" (disturbed: {d})"))
        );
        cx.log(&last);
        if disturbed.is_some() {
            result = Err(decibell_harness::inconclusive(format_args!("at rest after the peer left: no undisturbed try in three ({last})")));
            continue;
        }
        result = if idle.drawn as u32 <= changes.count() { Ok(()) } else { Err(anyhow::anyhow!("frames drawn while nothing on screen changed: {last}")) };
        break;
    }
    peer.cue("quit")?;
    leave(&cx).await?;
    result
}

// ── voice-speaking-idle ─────────────────────────────────────────────────────────────────

async fn check_idle(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    join(&cx, LOUNGE).await?;
    let peer = Peer::start(&steps(&["voice-join", "Lounge", "wait", "mls", "ready", "print", "joined", "wait-file", "@cue:quit", "voice-leave", "logout"]))?;
    peer.wait_line(&cx, "MARK joined", Duration::from_secs(60)).await?;
    wait_probe(&cx, &format!("participant-tile:{}", account_uid(PEER)?)).await?;
    // Nobody speaks (both muted): the stage draws nothing. Measured as the window is (focused
    // or not); the server bar's active tile glow may re-present while focused (it draws nothing).
    let mut last = String::new();
    let (mut passed, mut measured_failure) = (false, false);
    for attempt in 1..=4 {
        let before = settle_drawn(&cx).await?;
        let events_before = cx.update(|_, cx| model(cx).pump_stats().events)?;
        let active = cx.update(|window, _| window.is_window_active())?;
        let changes = Changes::start(&cx)?;
        let disturbed = cx.watched_sleep(Duration::from_secs(10)).await?;
        let frames = cx.frames()?.since(before);
        let events = cx.update(|_, cx| model(cx).pump_stats().events)? - events_before;
        last = format!(
            "try {attempt}: {} drawn, {} presented in 10 s ({}); {events} core events (voice pings / stats every ~2 s); {}{}",
            frames.drawn,
            frames.presented,
            if active { "focused" } else { "unfocused" },
            changes.describe(),
            disturbed.as_ref().map_or(String::new(), |d| format!(" (disturbed: {d})"))
        );
        cx.log(&last);
        if frames.drawn as u32 <= changes.count() {
            passed = true;
            break;
        }
        measured_failure |= disturbed.is_none();
    }
    if !passed && !measured_failure {
        peer.cue("quit")?;
        leave(&cx).await?;
        return Err(decibell_harness::inconclusive(format_args!("every try was disturbed by the desktop's focus ({last})")));
    }
    ensure!(passed, "the voice view drew while nothing on screen changed: {last}");
    peer.cue("quit")?;
    leave(&cx).await?;
    Ok(())
}

// ── voice-mute ──────────────────────────────────────────────────────────────────────────

async fn check_mute(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    join(&cx, LOUNGE).await?;
    let me = me(&cx)?;
    let peer = Peer::start(&steps(&[
        "voice-join", "Lounge", "wait", "mls", "ready", "print", "joined",
        "wait", "voice-state", ACCOUNT, "unmuted", "print", "saw-unmuted",
        "wait", "voice-state", ACCOUNT, "muted", "print", "saw-muted",
        "wait-file", "@cue:quit", "voice-leave", "logout",
    ]))?;
    peer.wait_line(&cx, "MARK joined", Duration::from_secs(60)).await?;
    // We joined muted and deafened. Undeafen first (the peer is muted: nothing plays), then
    // unmute (Discord's rule: unmuting also undeafens) and mute again.
    let state = |cx: &mut App| {
        let s = model(cx).live.voice.session.read(cx);
        (s.muted, s.deafened)
    };
    ensure!(cx.update(|_, cx| state(cx))? == (true, true), "not muted and deafened after joining");
    click_probe(&cx, "dock-mute").await?;
    wait_for(&cx, Duration::from_secs(5), "the dock's unmute to land", move |cx| state(cx) == (false, false)).await?;
    cx.log("dock: unmuted (and undeafened) in the model");
    peer.wait_line(&cx, "MARK saw-unmuted", Duration::from_secs(10)).await?;
    cx.log("the peer saw us unmuted");
    click_probe(&cx, "dock-mute").await?;
    wait_for(&cx, Duration::from_secs(5), "the dock's mute to land", move |cx| state(cx).0).await?;
    peer.wait_line(&cx, "MARK saw-muted", Duration::from_secs(10)).await?;
    cx.log("dock: muted; the peer saw it");
    click_probe(&cx, "dock-deafen").await?;
    wait_for(&cx, Duration::from_secs(5), "the dock's deafen to land", move |cx| state(cx) == (true, true)).await?;
    cx.log("dock: deafened");
    let _ = me;
    peer.cue("quit")?;
    leave(&cx).await?;
    Ok(())
}

// ── voice-watch ─────────────────────────────────────────────────────────────────────────

fn stream_file() -> Option<PathBuf> {
    let name = std::env::var("DECIBELL_WATCH_DBV").unwrap_or_else(|_| "1080p60_h264.dbv".into());
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().map(|d| d.join("vendor/streams").join(&name)).find(|p| p.exists())
}

async fn check_watch(cx: TestCx) -> Result<()> {
    let Some(file) = stream_file() else {
        cx.log("SKIP: vendor/streams/*.dbv not found above the binary");
        return Ok(());
    };
    ready(&cx).await?;
    join(&cx, LOUNGE).await?;
    let peer_uid = account_uid(PEER)?;
    let path = file.display().to_string();
    let peer = Peer::start(&steps(&[
        "voice-join", "Lounge", "wait", "mls", "ready", "stream-file", &path, "print", "streaming",
        "wait-file", "@cue:stop", "stream-stop", "voice-leave", "logout",
    ]))?;
    peer.wait_line(&cx, "STREAMING", Duration::from_secs(60)).await?;
    wait_probe(&cx, &format!("stream-tile:{peer_uid}")).await?;
    cx.log(format_args!("{} is live in Lounge", file.file_name().and_then(|n| n.to_str()).unwrap_or("?")));
    // Watch: a click on the tile focuses it (its first hover action).
    click_probe(&cx, &format!("stream-tile:{peer_uid}")).await?;
    let key = peer_uid.to_string();
    let t = wait_for(&cx, Duration::from_secs(15), "the first picture", {
        let key = key.clone();
        move |cx| crate::video_events::StreamStates::global(cx).read(cx).get(peer_uid).picture.is_some() && !key.is_empty()
    })
    .await?;
    cx.log(format_args!("first picture {} ms after the click", t.as_millis()));
    cx.sleep(Duration::from_secs(2)).await;
    // The rates over 10 s, from the hub's own counters (what the stats overlay reads).
    let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
    let stream = hub.stream(&key);
    let a = stream.stats();
    let frames_a = cx.frames()?;
    let window = Duration::from_secs(10);
    cx.sleep(window).await;
    let b = stream.stats();
    let frames = cx.frames()?.since(frames_a);
    let secs = window.as_secs_f64();
    let received = (b.received - a.received) as f64 / secs;
    let decoded = (b.decoded - a.decoded) as f64 / secs;
    let presented = (b.presented - a.presented) as f64 / secs;
    let dropped = b.dropped - a.dropped;
    cx.log(format_args!(
        "{} {}x{} via {}: received {received:.1} / decoded {decoded:.1} / presented {presented:.1} per s, {dropped} dropped, {} replaced, queue {}, {} keyframe requests so far; window: {} drawn / {} presented in {secs} s",
        b.codec.map(|c| c.label()).unwrap_or("?"),
        b.width,
        b.height,
        b.path.map(|p| p.label()).unwrap_or("?"),
        // Published and not yet drawn at the moment of the read: it can be one fewer at the end.
        b.replaced as i64 - a.replaced as i64,
        b.queue,
        b.keyframe_requests,
        frames.drawn,
        frames.presented,
    ));
    let fps = 60.;
    ensure!(received > fps * 0.95, "received {received:.1} frames/s of a {fps} fps stream");
    ensure!(presented > fps * 0.9, "presented {presented:.1} pictures/s of a {fps} fps stream");
    ensure!(dropped == 0, "{dropped} frames dropped");
    ensure!(frames.drawn < 30, "the watched stream drew {} frames (a picture should cost a present, not a draw)", frames.drawn);
    // The stats overlay shows the codec and the size.
    click_probe(&cx, "focus-stats").await.ok();
    // Leave the voice view: the mini-player keeps the same pictures coming.
    click_probe(&cx, "channel:general").await?;
    wait_for(&cx, Duration::from_secs(5), "the mini-player", |_| probes::flag_set("mini-player")).await?;
    let a = stream.stats();
    cx.sleep(Duration::from_secs(3)).await;
    let b = stream.stats();
    let mini = (b.presented - a.presented) as f64 / 3.;
    cx.log(format_args!("mini-player: {mini:.1} pictures/s presented"));
    ensure!(mini > fps * 0.9, "the mini-player presented {mini:.1} pictures/s");
    // Stop watching from the mini-player: the decoder goes.
    click_probe(&cx, "mini-player").await.ok();
    cx.update(|_, cx| actions::voice::stop_watching_stream(peer_uid, cx))?;
    let t = wait_for(&cx, Duration::from_secs(5), "the decoder to stop", {
        let hub = hub.clone();
        move |_| hub.decoder_threads() == 0
    })
    .await?;
    cx.sleep(Duration::from_millis(500)).await;
    cx.log(format_args!("stopped watching: decoder threads {} after {} ms, {} live textures", hub.decoder_threads(), t.as_millis(), hub.live_textures()));
    ensure!(hub.live_textures() == 0, "{} textures still alive", hub.live_textures());
    peer.cue("stop")?;
    if let Ok(line) = peer.wait_line(&cx, "STREAM-STOPPED", Duration::from_secs(15)).await {
        cx.log(format_args!("the streamer: {line}"));
    }
    leave(&cx).await?;
    Ok(())
}

// ── voice-stall ─────────────────────────────────────────────────────────────────────────

/// One watched stream, three things the stream path owes the views:
///
/// 1. the decoder is the one the capability probe found (the app ran it after its first frame,
///    in a fresh profile: no cache), and that is what the model advertises and Settings shows;
/// 2. in fullscreen the cursor is hidden when the overlays go (2.5 s after the pointer rested),
///    and not outside fullscreen;
/// 3. the streamer stops sending: "Reconnecting…" over the last picture after 3 s, and the
///    picture back when it sends again.
async fn check_stall(cx: TestCx) -> Result<()> {
    let Some(file) = stream_file() else {
        cx.log("SKIP: vendor/streams/*.dbv not found above the binary");
        return Ok(());
    };
    ready(&cx).await?;
    join(&cx, LOUNGE).await?;
    let peer_uid = account_uid(PEER)?;
    let path = file.display().to_string();
    let peer = Peer::start(&steps(&[
        "voice-join", "Lounge", "wait", "mls", "ready", "stream-file", &path, "print", "streaming",
        "wait-file", "@cue:pause", "stream-pause", "wait-file", "@cue:resume", "stream-resume",
        "wait-file", "@cue:stop", "stream-stop", "voice-leave", "logout",
    ]))?;
    peer.wait_line(&cx, "STREAMING", Duration::from_secs(60)).await?;
    wait_probe(&cx, &format!("stream-tile:{peer_uid}")).await?;
    click_probe(&cx, &format!("stream-tile:{peer_uid}")).await?;
    let state = move |cx: &mut App| crate::video_events::StreamStates::global(cx).read(cx).get(peer_uid);
    wait_for(&cx, Duration::from_secs(15), "the first picture", move |cx| state(cx).picture.is_some()).await?;
    let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
    let stream = hub.stream(&peer_uid.to_string());

    // 1. The probe ran (off the UI thread, after the first frame) and is what is used and shown.
    wait_for(&cx, Duration::from_secs(20), "the decode probe's answer in the model", |cx| {
        model(cx).live.voice.codecs.read(cx).decode_probed
    })
    .await?;
    let (decoders, skipped, advertised) = cx.update(|_, cx| {
        let codecs = model(cx).live.voice.codecs.read(cx);
        (codecs.decoders.clone(), codecs.skipped_decoders.clone(), codecs.decode.clone())
    })?;
    for d in &decoders {
        cx.log(format_args!("probed: {} decodes with {} ({})", d.codec.human_name(), d.label, if d.hardware { "hardware" } else { "software" }));
    }
    for s in &skipped {
        cx.log(format_args!("probed: {} with {} is not used: {}", s.codec.human_name(), s.label, s.reason));
    }
    ensure!(decoders.len() == 3 && decoders.iter().all(|d| d.probed), "the model holds {} probed decoders of 3", decoders.len());
    ensure!(advertised.len() == 3, "{} codecs advertised", advertised.len());
    let stats = stream.stats();
    let (codec, backend) = (stats.codec.context("no codec")?, stats.backend.context("no decoder open")?);
    let used = backend.display_name(codec);
    let model_codec = match codec {
        decibell_video::Codec::H264 => decibell_model::live::voice::VideoCodec::H264Hw,
        decibell_video::Codec::Hevc => decibell_model::live::voice::VideoCodec::H265,
        decibell_video::Codec::Av1 => decibell_model::live::voice::VideoCodec::Av1,
        // Codecs only attachment playback decodes: never a stream's.
        other => anyhow::bail!("the stream's codec is {}", other.label()),
    };
    let shown = decoders.iter().find(|d| d.codec == model_codec).map(|d| d.label.clone()).context("the stream's codec is not in the probe's answer")?;
    cx.log(format_args!("the stream ({}) is decoded with {used}; Settings → Codecs says {shown}", codec.label()));
    ensure!(shown.as_ref() == used, "the stream is decoded with {used}, the probe said {shown}");
    let cache = decibell_model::core::user_data_dir().context("no user data directory")?.join("video-decoders.txt");
    ensure!(cache.is_file(), "{} was not written", cache.display());
    cx.log(format_args!("the probe's answer is kept in {}", cache.file_name().and_then(|n| n.to_str()).unwrap_or("?")));

    // 2. The cursor goes with the overlays in fullscreen, and only there.
    let video = wait_probe(&cx, "focus-video").await?;
    let rests = || crate::focus::CURSOR_RESTS.load(std::sync::atomic::Ordering::Relaxed);
    cx.mouse_move(video.center())?;
    cx.sleep(crate::focus::OVERLAY_HIDE + Duration::from_millis(600)).await;
    ensure!(rests() == 0, "the cursor was hidden outside fullscreen");
    cx.update(|_, cx| model(cx).live.voice.watching.update(cx, |w, cx| w.set_fullscreen(true, cx)))?;
    cx.sleep(Duration::from_millis(500)).await;
    let video = probes::bounds("focus-video").context("the fullscreen video")?;
    // The real pointer may be over the test window and move, or leave it, at any time (that
    // re-arms or cancels the timer, as it should): a rest that was disturbed is tried again.
    let mut moved = Instant::now();
    for attempt in 1..=4 {
        cx.mouse_move(video.center() + point(gpui::px(attempt as f32), gpui::px(0.)))?;
        moved = Instant::now();
        if wait_for(&cx, crate::focus::OVERLAY_HIDE + Duration::from_secs(1), "the cursor to be hidden", move |_| rests() >= 1).await.is_ok() {
            break;
        }
        ensure!(attempt < 4, "the cursor was not hidden {} s after the pointer rested on a fullscreen stream (4 attempts)", crate::focus::OVERLAY_HIDE.as_secs_f32());
    }
    let visible = cx.update(|_, cx| cx.is_cursor_visible())?;
    cx.log(format_args!(
        "fullscreen: the cursor was hidden {:.2} s after the pointer rested (the platform says the cursor is {})",
        moved.elapsed().as_secs_f64(),
        if visible { "visible: the real pointer is not over this window" } else { "hidden" }
    ));
    ensure!(moved.elapsed() >= crate::focus::OVERLAY_HIDE - Duration::from_millis(100), "hidden too early");
    // Movement re-arms the one timer: nothing more is hidden until the pointer rests again.
    let before = rests();
    cx.mouse_move(video.center() + point(gpui::px(5.), gpui::px(0.)))?;
    cx.sleep(Duration::from_millis(1200)).await;
    cx.mouse_move(video.center())?;
    cx.sleep(Duration::from_millis(1200)).await;
    ensure!(rests() == before, "the cursor was hidden while the pointer kept moving");
    wait_for(&cx, crate::focus::OVERLAY_HIDE + Duration::from_secs(2), "the cursor to be hidden again", move |_| rests() == before + 1).await?;
    cx.update(|_, cx| model(cx).live.voice.watching.update(cx, |w, cx| w.set_fullscreen(false, cx)))?;
    cx.sleep(Duration::from_millis(800)).await;
    park_pointer(&cx)?;

    // 3. The streamer stops sending.
    ensure!(!cx.update(move |_, cx| state(cx).stalled)?, "stalled while playing");
    let frames_before = stream.stats().received;
    peer.cue("pause")?;
    peer.wait_line(&cx, "STREAM-PAUSED", Duration::from_secs(10)).await?;
    let paused = Instant::now();
    wait_for(&cx, decibell_video::STALLED_AFTER + Duration::from_secs(4), "Reconnecting…", move |cx| state(cx).stalled).await?;
    let after = paused.elapsed();
    cx.sleep(Duration::from_millis(300)).await;
    ensure!(probes::bounds("stream-reconnecting").is_some(), "the Reconnecting… indicator was not drawn");
    cx.log(format_args!(
        "the streamer paused after {frames_before} frames: Reconnecting… {:.2} s later; {} decoder threads, the last picture still up ({:?})",
        after.as_secs_f64(),
        hub.decoder_threads(),
        stream.picture_size()
    ));
    ensure!(after >= decibell_video::STALLED_AFTER - Duration::from_millis(300), "stalled after only {after:?}");
    ensure!(hub.decoder_threads() == 0, "a decoder thread is still running for a stalled stream");
    ensure!(stream.picture_size().is_some(), "the last picture is gone");
    let resumed = Instant::now();
    peer.cue("resume")?;
    wait_for(&cx, Duration::from_secs(10), "the picture to come back", move |cx| !state(cx).stalled).await?;
    cx.log(format_args!("the streamer told to send again: the picture is back within {} ms", resumed.elapsed().as_millis()));
    peer.wait_line(&cx, "STREAM-RESUMED", Duration::from_secs(10)).await?;
    let a = stream.stats();
    cx.sleep(Duration::from_secs(2)).await;
    let b = stream.stats();
    ensure!(b.presented - a.presented > 100, "only {} pictures in the 2 s after the stall", b.presented - a.presented);
    ensure!(!cx.update(move |_, cx| state(cx).stalled)?, "stalled again");

    cx.update(|_, cx| actions::voice::stop_watching_stream(peer_uid, cx))?;
    peer.cue("stop")?;
    if let Ok(line) = peer.wait_line(&cx, "STREAM-STOPPED", Duration::from_secs(15)).await {
        cx.log(format_args!("the streamer: {line}"));
    }
    leave(&cx).await?;
    Ok(())
}

// ── voice-golive-dialog ─────────────────────────────────────────────────────────────────

async fn check_golive(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    join(&cx, LOUNGE).await?;
    // The probe's answer is faked: software only (SVT-AV1, 1080p30 a second).
    cx.update(|_, cx| {
        let encoders = crate::codecs::Encoders::global(cx);
        encoders.update(cx, |e, cx| e.pin(software_caps(), cx));
    })?;
    let settings = |cx: &mut App| decibell_model::live::voice::go_live::StreamSettings::of(model(cx).settings.read(cx).get());
    let start = cx.update(|_, cx| settings(cx))?;
    frames_of(&cx, "the Go Live dialog opens", async |cx| {
        click_probe(cx, "dock-share").await?;
        wait_for(cx, Duration::from_secs(5), "the dialog", |_| probes::flag_set("go-live-open")).await?;
        Ok(())
    })
    .await?;
    // Slides and Data saver encode in software: they switch.
    for preset in ["slides", "saver"] {
        click_probe(&cx, &format!("preset:{preset}")).await?;
        let p = preset.to_string();
        wait_for(&cx, Duration::from_secs(3), &format!("the {preset} preset"), move |cx| settings(cx).preset().is_some_and(|x| x.id == p)).await?;
        cx.log(format_args!("{preset}: switched"));
    }
    // Smooth (1080p60) is past the software ceiling: greyed, a click changes nothing.
    click_probe(&cx, "preset:smooth").await?;
    cx.sleep(Duration::from_millis(300)).await;
    let now = cx.update(|_, cx| settings(cx))?;
    ensure!(now.preset().map(|p| p.id) == Some("saver"), "Smooth switched although software can't encode it");
    cx.log("smooth: greyed out (software-only caps), the click did nothing");
    click_probe(&cx, "go-live-close").await?;
    wait_for(&cx, Duration::from_secs(5), "the dialog to close", |_| !probes::flag_set("go-live-open")).await?;
    // Put the settings back as they were.
    cx.update(move |_, cx| actions::voice::update_stream_settings(|s| *s = start.clone(), cx))?;
    leave(&cx).await?;
    Ok(())
}

fn software_caps() -> Vec<crate::codecs::EncoderCap> {
    vec![crate::codecs::EncoderCap {
        codec: decibell_model::live::voice::VideoCodec::Av1,
        hardware: false,
        backend: "software".into(),
        label: "Software (SVT-AV1)".into(),
        max_width: 3840,
        max_height: 2160,
        max_fps: 60,
        max_pixel_rate: 1920 * 1080 * 30,
    }]
}

// ── call-ring ───────────────────────────────────────────────────────────────────────────

/// The sounds the model asked for since `start` (what the app's player was handed).
struct CueLog {
    seen: std::rc::Rc<std::cell::RefCell<Vec<Cue>>>,
    _sub: gpui::Subscription,
}

impl CueLog {
    fn start(cx: &TestCx) -> Result<CueLog> {
        cx.update(|_, cx| {
            let seen: std::rc::Rc<std::cell::RefCell<Vec<Cue>>> = Default::default();
            let log = seen.clone();
            let sub = cx.subscribe(&model(cx).live.cues, move |_, cue: &Cue, _| log.borrow_mut().push(*cue));
            CueLog { seen, _sub: sub }
        })
    }

    fn take(&self) -> Vec<Cue> {
        std::mem::take(&mut self.seen.borrow_mut())
    }
}

/// What plays on the test sink, metered outside a voice session: the core's microphone test on
/// the sink's monitor (no processing), its level kept for the check to read.
struct SinkMeter {
    db: std::sync::Arc<std::sync::atomic::AtomicI32>,
}

impl SinkMeter {
    fn start(sink: &str) -> Result<SinkMeter> {
        use std::sync::atomic::{AtomicI32, Ordering};
        let db = std::sync::Arc::new(AtomicI32::new(-96_000));
        let level = db.clone();
        audio::start_mic_test(
            Some(format!("{sink}.monitor")),
            audio::MicProcessing::default(),
            std::sync::Arc::new(move |db: f32| level.store((db * 1000.) as i32, Ordering::Relaxed)),
        )
        .map_err(|e| anyhow::anyhow!("the mic test on the sink's monitor was refused: {e}"))?;
        Ok(SinkMeter { db })
    }

    fn db(&self) -> f32 {
        self.db.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.
    }

    /// Watch for `window`: how many separate bursts above −45 dB, and the loudest level.
    async fn listen(&self, cx: &TestCx, window: Duration) -> (u32, f32) {
        bursts(cx, window, || self.db()).await
    }
}

impl Drop for SinkMeter {
    fn drop(&mut self) {
        audio::stop_mic_test();
    }
}

/// Separate bursts above −45 dB (the tone after at least 0.7 s without it: the ring's tone
/// dips once inside its 0.8 s) and the peak over `window`.
async fn bursts(cx: &TestCx, window: Duration, db: impl Fn() -> f32) -> (u32, f32) {
    let (mut count, mut peak, mut heard_at) = (0, -96f32, None::<Instant>);
    let started = Instant::now();
    while started.elapsed() < window {
        let now = db();
        peak = peak.max(now);
        if now > -45. {
            if heard_at.is_none_or(|at| at.elapsed() > Duration::from_millis(700)) {
                count += 1;
            }
            heard_at = Some(Instant::now());
        }
        cx.sleep(Duration::from_millis(20)).await;
    }
    (count, peak)
}

async fn check_call(cx: TestCx) -> Result<()> {
    // Without the DM pane (another crate's) the call stage needs a host to show in.
    cx.update(|_, cx| crate::calls::host_stage_in_dm_stand_in(cx))?;
    ready(&cx).await?;
    let status = |cx: &mut App| model(cx).live.calls.read(cx).status;
    let sink = test_sink();
    let cues = CueLog::start(&cx)?;
    // The ring is heard on the sink through the core's mic test on its monitor.
    let meter = match &sink {
        Some(sink) => {
            let meter = SinkMeter::start(sink)?;
            wait_for(&cx, Duration::from_secs(3), "the meter's capture stream", |_| {
                test_audio::streams_of(std::process::id()).is_ok_and(|s| !s.sources.is_empty())
            })
            .await?;
            on_test_sink("metering the sink")?;
            Some(meter)
        }
        None => None,
    };
    let peer = Peer::start(&steps(&[
        "call", ACCOUNT, "print", "calling", "wait", "call", "idle", "print", "declined",
        "wait-file", "@cue:again", "call", ACCOUNT, "wait", "call", "active", "print", "connected",
        "wait", "call", "idle", "print", "ended", "logout",
    ]))?;
    peer.wait_line(&cx, "MARK calling", Duration::from_secs(60)).await?;
    wait_for(&cx, Duration::from_secs(15), "the incoming card", |_| probes::flag_set("incoming-call")).await?;
    cx.log("ringing: the card is up");
    ensure!(cues.take().contains(&Cue::StartRing), "an incoming call didn't ask for the ring");
    if let Some(meter) = &meter {
        // 0.8 s of tone in each 2 s: more than one loop.
        let (heard, peak) = meter.listen(&cx, Duration::from_millis(4600)).await;
        on_test_sink("the ring plays")?;
        ensure!(heard >= 2, "the ring was heard {heard} time(s) in 4.6 s (peak {peak:.1} dB): it doesn't loop");
        cx.log(format_args!("the ring looped on the sink: {heard} bursts in 4.6 s, peak {peak:.1} dB"));
    }
    click_probe(&cx, "call-decline").await?;
    peer.wait_line(&cx, "MARK declined", Duration::from_secs(15)).await?;
    wait_for(&cx, Duration::from_secs(5), "idle after declining", move |cx| status(cx) == CallStatus::Idle).await?;
    cx.log("declined: the caller saw it end");
    ensure!(cues.take().contains(&Cue::StopRing), "declining didn't stop the ring");
    if let Some(meter) = &meter {
        cx.sleep(Duration::from_millis(400)).await;
        let (heard, peak) = meter.listen(&cx, Duration::from_millis(2500)).await;
        ensure!(heard == 0 && peak <= -90., "the ring went on after declining ({heard} bursts, peak {peak:.1} dB)");
        wait_for(&cx, Duration::from_secs(3), "the sounds' stream to close", |_| !test_audio::has_thread("decibell-cue")).await?;
        cx.log("declined: the ring stopped, its stream closed");
    }

    // Do Not Disturb: the call still shows, silently (no ring, no flash).
    cx.update(|_, cx| actions::presence::choose_status(OwnStatus::Dnd, cx))?;
    cx.sleep(Duration::from_millis(500)).await;
    let silent = async {
        peer.cue("again")?;
        wait_for(&cx, Duration::from_secs(15), "the second ring", move |cx| status(cx) == CallStatus::Incoming).await?;
        wait_for(&cx, Duration::from_secs(5), "the incoming card under Do Not Disturb", |_| probes::flag_set("incoming-call")).await?;
        let quiet = match &meter {
            Some(meter) => Some(meter.listen(&cx, Duration::from_millis(2500)).await),
            None => {
                cx.sleep(Duration::from_millis(500)).await;
                None
            }
        };
        let asked = cues.take();
        ensure!(!asked.contains(&Cue::StartRing) && !asked.contains(&Cue::Flash), "Do Not Disturb still rang or flashed: {asked:?}");
        if let Some((heard, peak)) = quiet {
            ensure!(heard == 0 && peak <= -90., "something sounded under Do Not Disturb ({heard} bursts, peak {peak:.1} dB)");
            ensure!(!test_audio::has_thread("decibell-cue"), "a stream for sounds opened under Do Not Disturb");
        }
        cx.log("Do Not Disturb: the card is up, nothing rang");
        anyhow::Ok(())
    }
    .await;
    // perf3 is shared: back to Online whatever happened.
    cx.update(|_, cx| actions::presence::choose_status(OwnStatus::Online, cx))?;
    cx.sleep(Duration::from_millis(300)).await;
    silent?;

    click_probe(&cx, "call-accept").await?;
    let t = wait_for(&cx, Duration::from_secs(20), "the call to connect", move |cx| status(cx) == CallStatus::Active).await?;
    let (path, verified) = cx.update(|_, cx| {
        let c = model(cx).live.calls.read(cx);
        (c.path, c.verified)
    })?;
    cx.log(format_args!("answered: connected after {} ms ({path:?}, verified {verified:?})", t.as_millis()));
    peer.wait_line(&cx, "MARK connected", Duration::from_secs(10)).await?;
    wait_probe(&cx, "call-hang-up").await?;
    cx.log("the call stage shows the call");
    if let Some(sink) = &sink {
        // The call holds the microphone: it ended the running test and refuses a new one.
        ensure!(!audio::mic_test_running(), "the mic test still runs in a call");
        wait_for(&cx, Duration::from_secs(2), "the mic test's thread to end", |_| !test_audio::has_thread("decibell-mic-test")).await?;
        let refused = audio::start_mic_test(Some(format!("{sink}.monitor")), audio::MicProcessing::default(), std::sync::Arc::new(|_| {}));
        ensure!(refused.is_err(), "a mic test started while a call holds the microphone");
        session_on_test_sink(&cx, "in the call").await?;
        cx.log("the call took the microphone from the mic test and refuses a new one; its streams are on the sink");
    }
    click_probe(&cx, "call-hang-up").await?;
    wait_for(&cx, Duration::from_secs(5), "idle after hanging up", move |cx| status(cx) == CallStatus::Idle).await?;
    peer.wait_line(&cx, "MARK ended", Duration::from_secs(10)).await?;
    cx.log("hung up: both sides idle");
    if sink.is_some() {
        closed_after_session(&cx).await?;
    }
    Ok(())
}

/// After a session: its streams close and no audio thread of ours stays (the end-of-call sound
/// plays out first).
async fn closed_after_session(cx: &TestCx) -> Result<()> {
    let t = wait_for(cx, Duration::from_secs(6), "the session's streams to close", |_| {
        !test_audio::has_thread("decibell-cue")
            && test_audio::streams_of(std::process::id()).is_ok_and(|s| s.sinks.is_empty() && s.sources.is_empty())
    })
    .await?;
    ensure!(!test_audio::has_thread("decibell-mic-test"), "a mic test thread is alive");
    cx.log(format_args!("{} ms after the session: no stream of ours is open, no cue or mic-test thread exists", t.as_millis()));
    Ok(())
}

// ── voice-cues ──────────────────────────────────────────────────────────────────────────

async fn check_cues(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let Some(sink) = test_sink() else {
        cx.log("SKIP: no null sink (pactl): sounds are never played on the real output");
        return Ok(());
    };
    // The session's input is the monitor of its own output: with echo cancellation on, what it
    // plays is what gets cancelled. Meter the raw path.
    cx.update(|_, cx| {
        actions::voice::set_aec_enabled(false, cx);
        actions::voice::set_noise_suppression_level(0, cx);
        actions::voice::set_agc_enabled(false, cx);
        model(cx).live.voice.input_level.update(cx, |l, _| l.retain_meter());
    })?;
    let level = |cx: &mut App| model(cx).live.voice.input_level.read(cx).db;
    join(&cx, LOUNGE).await?;
    // The connect sound was asked for before the session's stream existed: it may have opened
    // the sounds' own stream. From here every sound goes into the session's.
    wait_for(&cx, Duration::from_secs(4), "the connect sound's stream to close", |_| !test_audio::has_thread("decibell-cue")).await?;
    cx.sleep(Duration::from_millis(500)).await;
    let before = session_on_test_sink(&cx, "in Lounge").await?;
    cx.log(format_args!("in Lounge: {} playback / {} capture stream(s), all on the sink and its monitor", before.sinks.len(), before.sources.len()));
    let rest = cx.update(|_, cx| level(cx))?;
    ensure!(rest <= -90., "the session's meter reads {rest:.1} dB on a silent input");
    // The session holds the microphone: no test beside it.
    let refused = audio::start_mic_test(Some(format!("{sink}.monitor")), audio::MicProcessing::default(), std::sync::Arc::new(|_| {}));
    ensure!(refused.is_err() && !audio::mic_test_running(), "a mic test started while a voice session holds the microphone");
    ensure!(!test_audio::has_thread("decibell-mic-test"), "a mic test thread exists in voice");
    cx.log("the mic test is refused while the session holds the microphone");

    // A sound in voice: mixed into the session's output (heard on its own input, the monitor).
    let extra = std::cell::Cell::new(false);
    let watch = |cx: &TestCx| -> Result<f32> {
        if test_audio::has_thread("decibell-cue") {
            extra.set(true);
        }
        cx.update(|_, cx| level(cx))
    };
    cx.update(|_, cx| model(cx).live.cues.update(cx, |c, cx| c.play(Sound::UserJoin, cx)))?;
    let mut peak = -96f32;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1200) {
        peak = peak.max(watch(&cx)?);
        cx.sleep(Duration::from_millis(20)).await;
    }
    let during = on_test_sink("a sound plays in voice")?;
    ensure!(peak > -60., "user_join never showed on the session's meter (peak {peak:.1} dB)");
    ensure!(!extra.get() && during == before, "a sound in voice opened a stream of its own ({before:?} → {during:?})");
    wait_for(&cx, Duration::from_secs(3), "the meter to rest after the sound", move |cx| level(cx) <= -90.).await?;
    cx.log(format_args!("user_join in voice: peak {peak:.1} dB on the session's meter, same streams, no cue thread"));

    // The ring in voice loops in the same mix until stopped.
    cx.update(|_, cx| model(cx).live.cues.update(cx, |c, cx| c.start_ring(cx)))?;
    let (heard, ring_peak) = bursts(&cx, Duration::from_millis(4600), || watch(&cx).unwrap_or(-96.)).await;
    let during = on_test_sink("the ring plays in voice")?;
    cx.update(|_, cx| model(cx).live.cues.update(cx, |c, cx| c.stop_ring(cx)))?;
    ensure!(heard >= 2, "the ring was heard {heard} time(s) in 4.6 s in voice (peak {ring_peak:.1} dB)");
    ensure!(!extra.get() && during == before, "the ring in voice opened a stream of its own ({before:?} → {during:?})");
    cx.sleep(Duration::from_millis(400)).await;
    let (after, after_peak) = bursts(&cx, Duration::from_millis(2500), || cx.update(|_, cx| level(cx)).unwrap_or(-96.)).await;
    ensure!(after == 0 && after_peak <= -90., "the ring went on after it was stopped ({after} bursts, peak {after_peak:.1} dB)");
    cx.log(format_args!("the ring in voice: {heard} bursts in 4.6 s, stopped; same streams, no cue thread"));

    cx.update(|_, cx| model(cx).live.voice.input_level.update(cx, |l, cx| l.release_meter(cx)))?;
    leave(&cx).await?;
    closed_after_session(&cx).await
}

// ── voice-switch ────────────────────────────────────────────────────────────────────────

async fn check_switch(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let me = me(&cx)?;
    join(&cx, LOUNGE).await?;
    // The click shows us in Squad and out of Lounge at once (the optimistic switch, the next turn
    // of the event loop), before the server confirms it.
    let bounds = wait_probe(&cx, &format!("voice:{SQUAD}")).await?;
    let frames_before = cx.frames()?;
    let clicked = Instant::now();
    cx.click(bounds.center())?;
    let mut shown_after = None;
    let mut mls_ready_before = false;
    while clicked.elapsed() < Duration::from_secs(2) {
        let (squad, lounge, ready) = cx.update(move |_, cx| {
            let s = model(cx).live.voice.session.read(cx);
            let ready = s.is_connected_to(daily_gaming(), SQUAD) && s.e2ee.as_ref().is_some_and(|e| e.phase == VoiceE2eePhase::Ready);
            (in_channel(cx, SQUAD, me), in_channel(cx, LOUNGE, me), ready)
        })?;
        if squad && !lounge {
            shown_after = Some(clicked.elapsed());
            mls_ready_before = ready;
            break;
        }
        cx.sleep(Duration::from_millis(5)).await;
    }
    let shown_after = shown_after.context("the switch didn't show within 2 s")?;
    let frames = cx.frames()?.since(frames_before);
    cx.log(format_args!(
        "shown in Squad {} ms after the click ({} frames drawn by then; the server's confirmation {})",
        shown_after.as_millis(),
        frames.drawn,
        if mls_ready_before { "had already come" } else { "still pending" }
    ));
    ensure!(shown_after < Duration::from_millis(100), "the switch took {} ms to show", shown_after.as_millis());
    let t = wait_for(&cx, Duration::from_secs(20), "the server's confirmation (MLS ready in Squad)", move |cx| {
        let s = model(cx).live.voice.session.read(cx);
        s.is_connected_to(daily_gaming(), SQUAD) && s.e2ee.as_ref().is_some_and(|e| e.phase == VoiceE2eePhase::Ready)
    })
    .await?;
    cx.log(format_args!("Squad confirmed, MLS ready after {} ms", t.as_millis()));
    leave(&cx).await?;
    Ok(())
}
