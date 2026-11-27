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
    ("voice-watch-hidden", "a watched stream (1080p60 H.264 from a headless peer) stops decoding while the window is minimised: CPU and decoded pictures with the window shown, minimised as before the pause (measured), minimised now (no decoder thread, nothing decoded, the watch goes on); restored, a keyframe is asked for and the picture comes back"),
    ("voice-member-drag", "as alice (Daily Gaming's owner, out of voice): perf4 in Lounge is dragged from the sidebar onto Squad; while the drag is in flight the channels it may land on pulse; dropped, perf4 is moved there; then the user menu's voice moderation: server mute and unmute reach the roster, Disconnect from voice takes perf4 out"),
    ("voice-golive-software", "DECIBELL_ENCODER=software (no hardware encoder): Go Live with Auto reaches SVT-AV1 through the real capture path (a fake ScreenCast portal on a private bus handing over a test-pattern PipeWire node, never the desktop); the greyed preset moves to an encodable one; a refused picker's error is gone on reopen; the peer sees an AV1 stream announced unlocked; our preview decodes it and mutes the stream gain, which comes back when the preview goes; the desktop ending the capture ends the stream; the dialog closes when the session ends"),
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
        SelfTest { name: "voice-golive-software", about: about("voice-golive-software"), run: |cx| Box::pin(parent_golive(cx, "voice-golive-software")) },
        SelfTest { name: "voice-watch-hidden", about: about("voice-watch-hidden"), run: |cx| Box::pin(parent(cx, "voice-watch-hidden")) },
        SelfTest { name: "voice-member-drag", about: about("voice-member-drag"), run: |cx| Box::pin(parent_env(cx, "voice-member-drag", "alice", Vec::new())) },
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
    parent_env(cx, name, ACCOUNT, Vec::new()).await
}

async fn parent_env(cx: TestCx, name: &'static str, account: &str, extra: Vec<(String, String)>) -> Result<()> {
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
        .arg(account)
        .env("DECIBELL_USER_DATA_DIR", &profile.0)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env_remove("DECIBELL_PERF")
        .envs(extra)
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
    // `DECIBELL_TEST_CHILD_LOG=<file>`: the child's whole log kept (its renders with
    // `DECIBELL_VOICE_TRACE=1`), for a failure to be read afterwards.
    if let Some(path) = std::env::var_os("DECIBELL_TEST_CHILD_LOG") {
        let _ = std::fs::write(&path, err.join("\n"));
    }
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
        SelfTest { name: "voice-golive-software", about: "(in the app) Go Live on the software encoder", run: |cx| Box::pin(check_golive_software(cx)) },
        SelfTest { name: "voice-watch-hidden", about: "(in the app) a watched stream while minimised", run: |cx| Box::pin(check_watch_hidden(cx)) },
        SelfTest { name: "voice-member-drag", about: "(in the app) drag a voice member to another channel", run: |cx| Box::pin(check_member_drag(cx)) },
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

/// The start of an at-rest window: drawing stopped, what changes from here on watched, and the
/// frame counters taken 100 ms later. A change notified just before the watch (the peer's MLS epoch
/// ending, a relayout) draws its frame within those 100 ms instead of being charged to the window
/// unexplained; one in them is counted, whether its frame lands before the counters or after.
async fn at_rest(cx: &TestCx) -> Result<(Changes, StoreCounts, Vec<gpui::Subscription>, Frames)> {
    settle_drawn(cx).await?;
    let changes = Changes::start(cx)?;
    let (stores, subscriptions) = Changes::watch_all(cx)?;
    cx.sleep(Duration::from_millis(100)).await;
    Ok((changes, stores, subscriptions, cx.frames()?))
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
            let (b1, b2, b3, b4, b5) = (bump(&other), bump(&other), bump(&other), bump(&other), bump(&other));
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
                // A toast coming or going is drawn too.
                cx.observe(&m.toasts, move |_, _| b5()),
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
    let cues = CueLog::start(&cx)?;
    frames_of(&cx, "joining Lounge from the sidebar", async |cx| join(cx, LOUNGE).await).await?;
    let heard = cues.take();
    cx.log(format_args!("joining played {heard:?}"));
    ensure!(heard.contains(&Cue::Play(Sound::Connect)), "no Connect sound for our own join: {heard:?}");
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
    let mut result = Err(decibell_harness::inconclusive("no try"));
    for attempt in 1..=3 {
        let (changes, stores, _all, before) = at_rest(&cx).await?;
        let active = cx.update(|window, _| window.is_window_active())?;
        log::info!("[voice-test] idle window starts");
        let disturbed = cx.watched_sleep(Duration::from_secs(5)).await?;
        log::info!("[voice-test] idle window ends");
        let idle = cx.frames()?.since(before);
        let notified: Vec<String> = stores.borrow().iter().map(|(k, n)| format!("{k} {n}")).collect();
        let last = format!(
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
    // The connection-stats popover opens from the user panel's latency readout and doesn't
    // outlive the session (`ConnectionStatsPopover` sits in the in-session block).
    click_probe(&cx, "user-panel-latency").await?;
    let stats_open = |cx: &mut App| {
        crate::state::VoiceUi::try_global(cx).is_some_and(|ui| matches!(ui.read(cx).popover, Some(crate::state::PanelPopover::Stats(_))))
    };
    wait_for(&cx, Duration::from_secs(5), "the connection-stats popover", stats_open).await?;
    cx.log("the latency readout opened the connection-stats popover");
    // Leave by the user panel's Disconnect: our Disconnect sound, and the popover goes too.
    click_probe(&cx, "user-panel-disconnect").await?;
    wait_for(&cx, Duration::from_secs(10), "leaving voice", |cx| !model(cx).live.voice.session.read(cx).media_session_active()).await?;
    let heard = cues.take();
    ensure!(heard.contains(&Cue::Play(Sound::Disconnect)), "no Disconnect sound for our own leave: {heard:?}");
    wait_for(&cx, Duration::from_secs(5), "the popover to close with the session", move |cx| !stats_open(cx)).await?;
    cx.log(format_args!("left by the user panel's Disconnect: played {heard:?}; the stats popover closed with the session"));
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
    let result = idle_tries(&cx, "as the desktop has it").await;
    // Then unfocused for sure: a small window of this process takes the focus (the desktop gives
    // a new window of the active app the focus; nothing else on it is touched).
    let other = cx.update(|_, cx| {
        let bounds = gpui::Bounds::new(point(gpui::px(0.), gpui::px(0.)), gpui::size(gpui::px(240.), gpui::px(120.)));
        cx.open_window(
            gpui::WindowOptions { window_bounds: Some(gpui::WindowBounds::Windowed(bounds)), ..Default::default() },
            |_, cx| gpui::AppContext::new(cx, |_| FocusTaker),
        )
    })??;
    let unfocused = {
        let started = Instant::now();
        loop {
            if !cx.update(|window, _| window.is_window_active())? {
                break true;
            }
            if started.elapsed() > Duration::from_secs(3) {
                break false;
            }
            cx.sleep(Duration::from_millis(50)).await;
        }
    };
    let second = if unfocused {
        Some(idle_tries(&cx, "unfocused (our own window took the focus)").await)
    } else {
        cx.log("the desktop didn't give our second window the focus: the unfocused case wasn't measured");
        None
    };
    cx.update(move |_, cx| {
        let _ = other.update(cx, |_, window, _| window.remove_window());
    })?;
    peer.cue("quit")?;
    leave(&cx).await?;
    result?;
    if let Some(second) = second {
        second?;
    }
    Ok(())
}

/// A blank window that only takes the focus from the app's main window.
struct FocusTaker;

impl gpui::Render for FocusTaker {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        gpui::div()
    }
}

/// Up to four 10 s windows at rest: the voice view passes when it drew no more frames than the
/// things on screen that changed (see [`Changes`]).
async fn idle_tries(cx: &TestCx, label: &str) -> Result<()> {
    let mut last = String::new();
    let (mut passed, mut measured_failure) = (false, false);
    let visibility = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let _visibility = {
        let visibility = visibility.clone();
        cx.update(move |window, _| window.observe_window_visibility(move |_, _, _| visibility.set(visibility.get() + 1)))?
    };
    for attempt in 1..=4 {
        // The change watch first, then 100 ms, then the window's first frame count (`at_rest`): a frame
        // already asked for when drawing seemed to stop lands before the measurement.
        let (changes, stores, _subscriptions, before) = at_rest(cx).await?;
        let events_before = cx.update(|_, cx| model(cx).pump_stats().events)?;
        let active = cx.update(|window, _| window.is_window_active())?;
        let flips = visibility.get();
        log::info!("[voice-test] idle window starts ({label})");
        let disturbed = cx.watched_sleep(Duration::from_secs(10)).await?;
        log::info!("[voice-test] idle window ends");
        let frames = cx.frames()?.since(before);
        let events = cx.update(|_, cx| model(cx).pump_stats().events)? - events_before;
        let notified: Vec<String> = stores.borrow().iter().map(|(k, n)| format!("{k} {n}")).collect();
        let shown = visibility.get() - flips;
        last = format!(
            "{label}, try {attempt}: {} drawn, {} presented in 10 s ({}); {events} core events (voice pings / stats every ~2 s); {}; {shown} visibility changes; stores that notified: [{}]{}",
            frames.drawn,
            frames.presented,
            if active { "focused" } else { "unfocused" },
            changes.describe(),
            notified.join(", "),
            disturbed.as_ref().map_or(String::new(), |d| format!(" (disturbed: {d})"))
        );
        cx.log(&last);
        if frames.drawn as u32 <= changes.count() + shown {
            passed = true;
            break;
        }
        measured_failure |= disturbed.is_none() && shown == 0;
    }
    if !passed && !measured_failure {
        return Err(decibell_harness::inconclusive(format_args!("every try was disturbed by the desktop ({last})")));
    }
    ensure!(passed, "the voice view drew while nothing on screen changed: {last}");
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
    // Escape steps back to the grid; a click on the tile focuses the stream again.
    let focused = move |cx: &mut App| model(cx).live.voice.watching.read(cx).focused;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape back to the grid", move |cx| focused(cx).is_none()).await?;
    ensure!(cx.update(move |_, cx| model(cx).live.voice.watching.read(cx).is_watching(peer_uid))?, "Escape stopped the watch");
    cx.log("Escape: back to the grid, still watching");
    click_probe(&cx, &format!("stream-tile:{peer_uid}")).await?;
    wait_for(&cx, Duration::from_secs(5), "the stream focused again", move |cx| focused(cx) == Some(peer_uid)).await?;
    // Leave the voice view: the mini-player keeps the same pictures coming.
    click_probe(&cx, "channel:general").await?;
    wait_for(&cx, Duration::from_secs(5), "the mini-player", |_| probes::flag_set("mini-player")).await?;
    let a = stream.stats();
    cx.sleep(Duration::from_secs(3)).await;
    let b = stream.stats();
    let mini = (b.presented - a.presented) as f64 / 3.;
    cx.log(format_args!("mini-player: {mini:.1} pictures/s presented"));
    ensure!(mini > fps * 0.9, "the mini-player presented {mini:.1} pictures/s");
    // A click on the mini-player returns to the stream, which grows back out of it.
    let grows = crate::focus::GROWS.load(std::sync::atomic::Ordering::Relaxed);
    // Press, a frame (the release is a window listener the press's frame installs), release.
    let mini = wait_probe(&cx, "mini-player").await?;
    cx.mouse_down(mini.center(), 1, gpui::Modifiers::default())?;
    cx.sleep(Duration::from_millis(100)).await;
    cx.mouse_up(mini.center())?;
    wait_for(&cx, Duration::from_secs(5), "the voice view with the stream focused", move |cx| {
        model(cx).nav.read(cx).active_view == ActiveView::Voice && focused(cx) == Some(peer_uid)
    })
    .await?;
    wait_for(&cx, Duration::from_secs(5), "the mini-player to go", |_| !probes::flag_set("mini-player")).await?;
    wait_for(&cx, Duration::from_secs(2), "the video to grow out of the mini-player", move |_| {
        crate::focus::GROWS.load(std::sync::atomic::Ordering::Relaxed) > grows
    })
    .await?;
    cx.log("the mini-player's click returned to the focused stream, which grew out of it");
    // Stop watching: the decoder goes.
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
    // Smooth chosen (the default), past this fake encoder's ceiling.
    cx.update(|_, cx| {
        actions::voice::update_stream_settings(|s| s.apply_preset(&decibell_model::live::voice::go_live::STREAM_PRESETS[0]), cx)
    })?;
    frames_of(&cx, "the Go Live dialog opens", async |cx| {
        click_probe(cx, "dock-share").await?;
        wait_for(cx, Duration::from_secs(5), "the dialog", |_| probes::flag_set("go-live-open")).await?;
        Ok(())
    })
    .await?;
    // A greyed preset can't stay chosen: the encodable one with the most pixels a second.
    wait_for(&cx, Duration::from_secs(3), "the greyed Smooth to give way to Data saver", move |cx| {
        settings(cx).preset().is_some_and(|p| p.id == "saver")
    })
    .await?;
    cx.log("Smooth was chosen and is greyed: Data saver chosen as the dialog opened");
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

// ── voice-member-drag ───────────────────────────────────────────────────────────────────

/// As alice, who owns Daily Gaming and isn't in voice (no E2EE needed: she never joins): drag
/// perf4's row in the sidebar's Lounge roster onto Squad.
async fn check_member_drag(cx: TestCx) -> Result<()> {
    wait_for(&cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    wait_for(&cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    let opened = |cx: &mut App| {
        let m = model(cx);
        m.nav.read(cx).active_server == Some(daily_gaming()) && m.community(daily_gaming(), cx).is_some_and(|c| !c.channels.read(cx).list().is_empty())
    };
    let started = Instant::now();
    loop {
        click_probe(&cx, "server-tile:1").await?;
        if wait_for(&cx, Duration::from_secs(1), "Daily Gaming's channels", opened).await.is_ok() {
            break;
        }
        ensure!(started.elapsed() < Duration::from_secs(15), "timed out after 15 s waiting for Daily Gaming's channels");
    }
    let peer_uid = account_uid(PEER)?;
    let peer = Peer::start(&steps(&[
        "voice-join", "Lounge", "wait", "mls", "ready", "print", "joined",
        "wait", "voice", "Squad", PEER, "print", "moved",
        "wait", "voice-gone", "Squad", PEER, "print", "disconnected",
        "wait-file", "@cue:quit", "logout",
    ]))?;
    peer.wait_line(&cx, "MARK joined", Duration::from_secs(60)).await?;
    wait_for(&cx, Duration::from_secs(10), "perf4 in Lounge's roster", move |cx| in_channel(cx, LOUNGE, peer_uid)).await?;
    // The floating user panel covers the sidebar's bottom: fold the text categories so the voice
    // channels sit above it (a drop lands only where the pointer really is over the row).
    for category in ["information", "text-channels"] {
        click_probe(&cx, &format!("category:{category}")).await?;
    }
    cx.sleep(Duration::from_millis(300)).await;
    let row = wait_probe(&cx, &format!("voice-member:{peer_uid}")).await?;
    let target = wait_probe(&cx, &format!("voice:{SQUAD}")).await?;
    if let Some(panel) = probes::bounds("user-panel-settings") {
        ensure!(target.bottom() < panel.top() - gpui::px(40.), "Squad's row ({target:?}) is under the user panel ({panel:?})");
    }
    cx.log(format_args!("perf4's row at {:?}, Squad's at {:?}", row.center(), target.center()));
    // Press, move past the drag threshold, then over Squad in steps (a drag's moves).
    cx.mouse_down(row.center(), 1, gpui::Modifiers::default())?;
    let (from, to) = (row.center(), target.center());
    for step in 1..=12 {
        let f = step as f32 / 12.;
        cx.mouse_drag(point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f))?;
        cx.sleep(Duration::from_millis(30)).await;
    }
    let pulsing = probes::flag_set("member-drag");
    cx.log(format_args!("dragging over Squad: the drop targets pulse ({pulsing})"));
    ensure!(pulsing, "no drag in flight after moving perf4's row onto Squad");
    cx.mouse_up(to)?;
    let moved = wait_for(&cx, Duration::from_secs(10), "perf4 in Squad", move |cx| in_channel(cx, SQUAD, peer_uid)).await;
    let toasts: Vec<String> =
        cx.update(|_, cx| model(cx).toasts.read(cx).toasts().iter().map(|t| format!("{} {:?}", t.title, t.body)).collect())?;
    if !toasts.is_empty() {
        cx.log(format_args!("toasts after the drop: {toasts:?}"));
    }
    let t = moved?;
    cx.log(format_args!("dropped on Squad: perf4 moved there {} ms later", t.as_millis()));
    peer.wait_line(&cx, "MARK moved", Duration::from_secs(10)).await?;
    wait_for(&cx, Duration::from_secs(5), "the drag to end", |_| !probes::flag_set("member-drag")).await?;
    cx.log("the peer sees itself in Squad; the pulse ended with the drag");

    // The user menu's voice moderation (the owner may moderate anyone): server mute and unmute
    // show on the roster, Disconnect takes perf4 out of voice.
    let squad_key = (daily_gaming(), decibell_model::ChannelId::from(SQUAD));
    // The menu closes when its window loses the focus; other windows share this desktop.
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    let menu = |entry: &'static str| {
        let cx = cx.clone();
        async move {
            wait_for(&cx, Duration::from_secs(5), "the last menu to close", |cx| model(cx).overlays.read(cx).context_menu.is_none()).await?;
            cx.update(move |_, cx| decibell_common::open_user_menu(peer_uid, point(gpui::px(700.), gpui::px(200.)), Some(daily_gaming()), cx))?;
            wait_for(&cx, Duration::from_secs(5), "the user menu", |cx| model(cx).overlays.read(cx).context_menu.is_some()).await?;
            // Its rows are where this menu drew them (not the last one's).
            settle_drawn(&cx).await?;
            click_probe(&cx, &format!("user-menu:{entry}")).await
        }
    };
    for (entry, muted) in [("menu-voice-mute", true), ("menu-voice-mute", false)] {
        menu(entry).await?;
        let key = squad_key.clone();
        wait_for(&cx, Duration::from_secs(10), if muted { "perf4 server muted" } else { "perf4 server unmuted" }, move |cx| {
            model(cx).live.voice.rosters.read(cx).channel(&key).is_some_and(|c| c.state(peer_uid).server_muted == muted)
        })
        .await?;
        cx.log(format_args!("the user menu's {}: the roster shows it", if muted { "Server mute" } else { "Server unmute" }));
    }
    menu("menu-voice-disconnect").await?;
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(None, cx))?;
    wait_for(&cx, Duration::from_secs(10), "perf4 out of voice", move |cx| !in_channel(cx, SQUAD, peer_uid)).await?;
    peer.wait_line(&cx, "MARK disconnected", Duration::from_secs(10)).await?;
    cx.log("the user menu's Disconnect from voice: perf4 left voice, and sees it");
    for category in ["information", "text-channels"] {
        click_probe(&cx, &format!("category:{category}")).await?;
    }
    peer.cue("quit")?;
    Ok(())
}

// ── voice-watch-hidden ──────────────────────────────────────────────────────────────────

/// `tools/perf/winctl.sh <verb> <our pid>` (KWin scripting), found above the binary.
fn winctl(verb: &str) -> Result<bool> {
    let exe = std::env::current_exe()?;
    let Some(script) = exe.ancestors().map(|dir| dir.join("tools/perf/winctl.sh")).find(|path| path.exists()) else {
        return Ok(false);
    };
    let status = Command::new(script).arg(verb).arg(std::process::id().to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    Ok(status.success())
}

/// CPU time of this process and of its threads whose name starts with `prefix`, in clock ticks.
fn cpu_ticks(prefix: &str) -> (u64, u64) {
    fn ticks(stat: &str) -> u64 {
        // Fields after the `(comm)`: state is the 1st, utime the 12th, stime the 13th.
        let rest = stat.rsplit_once(')').map_or("", |(_, r)| r);
        let fields: Vec<&str> = rest.split_whitespace().collect();
        fields.get(11).and_then(|u| u.parse::<u64>().ok()).unwrap_or(0) + fields.get(12).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0)
    }
    let process = std::fs::read_to_string("/proc/self/stat").map(|s| ticks(&s)).unwrap_or(0);
    let mut threads = 0;
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            let comm = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
            if comm.trim().starts_with(prefix) {
                threads += std::fs::read_to_string(task.path().join("stat")).map(|s| ticks(&s)).unwrap_or(0);
            }
        }
    }
    (process, threads)
}

/// Wait until the window's visibility (`Window::is_visible`) is `visible`.
async fn window_visible(cx: &TestCx, visible: bool, limit: Duration) -> Result<Duration> {
    let started = Instant::now();
    loop {
        if cx.update(|window, _| window.is_visible())? == visible {
            return Ok(started.elapsed());
        }
        ensure!(started.elapsed() < limit, "the window didn't become {} in {} s", if visible { "visible" } else { "hidden" }, limit.as_secs());
        cx.sleep(Duration::from_millis(50)).await;
    }
}

/// Over `window`: the process's and the decoder threads' CPU (% of one core), and the pictures
/// decoded per second.
async fn measure(cx: &TestCx, stream: &decibell_video::Stream, window: Duration) -> (f64, f64, f64) {
    let hz = 100.; // USER_HZ on Linux
    let (p0, t0) = cpu_ticks("decibell-video");
    let d0 = stream.stats().decoded;
    cx.sleep(window).await;
    let (p1, t1) = cpu_ticks("decibell-video");
    let d1 = stream.stats().decoded;
    let secs = window.as_secs_f64();
    ((p1 - p0) as f64 / hz / secs * 100., (t1.saturating_sub(t0)) as f64 / hz / secs * 100., (d1 - d0) as f64 / secs)
}

async fn check_watch_hidden(cx: TestCx) -> Result<()> {
    let Some(file) = stream_file() else {
        cx.log("SKIP: vendor/streams/*.dbv not found above the binary");
        return Ok(());
    };
    if !std::env::current_exe()?.ancestors().any(|dir| dir.join("tools/perf/winctl.sh").exists()) {
        cx.log("SKIP: tools/perf/winctl.sh isn't above the binary: a minimised window can't be restored from here");
        return Ok(());
    }
    ready(&cx).await?;
    join(&cx, LOUNGE).await?;
    let peer_uid = account_uid(PEER)?;
    let path = file.display().to_string();
    let peer = Peer::start(&steps(&[
        "voice-join", "Lounge", "wait", "mls", "ready", "stream-file", &path, "print", "streaming",
        "wait-file", "@cue:stop", "stream-stop", "voice-leave", "logout",
    ]))?;
    peer.wait_line(&cx, "STREAMING", Duration::from_secs(60)).await?;
    click_probe(&cx, &format!("stream-tile:{peer_uid}")).await?;
    let state = move |cx: &mut App| crate::video_events::StreamStates::global(cx).read(cx).get(peer_uid);
    wait_for(&cx, Duration::from_secs(15), "the first picture", move |cx| state(cx).picture.is_some()).await?;
    cx.sleep(Duration::from_secs(2)).await;
    let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
    let stream = hub.stream(&peer_uid.to_string());
    let window = Duration::from_secs(5);

    let (shown_cpu, shown_decoder, shown_rate) = measure(&cx, &stream, window).await;
    cx.log(format_args!("shown: process {shown_cpu:.1} % of a core, decoder threads {shown_decoder:.1} %, {shown_rate:.1} pictures/s decoded"));
    ensure!(shown_rate > 50., "only {shown_rate:.1} pictures/s decoded while shown");

    // Minimised (the platform's visibility: KWin suspends a minimised window).
    cx.update(|window, _| window.minimize_window())?;
    let t = window_visible(&cx, false, Duration::from_secs(5)).await?;
    cx.log(format_args!("minimised: hidden {} ms later", t.as_millis()));
    // As before the pause existed (the measurement's baseline): decoding goes on.
    cx.update(|_, _| hub.set_pause_when_hidden(false))?;
    cx.sleep(Duration::from_secs(1)).await;
    let (old_cpu, old_decoder, old_rate) = measure(&cx, &stream, window).await;
    cx.log(format_args!("minimised, without the pause (before): process {old_cpu:.1} %, decoder threads {old_decoder:.1} %, {old_rate:.1} pictures/s decoded"));
    // Now: paused.
    let requests = stream.stats().keyframe_requests;
    cx.update(|_, _| hub.set_pause_when_hidden(true))?;
    wait_for(&cx, Duration::from_secs(5), "the decoder thread to leave", {
        let hub = hub.clone();
        move |_| hub.decoder_threads() == 0
    })
    .await?;
    cx.sleep(Duration::from_secs(1)).await;
    let (new_cpu, new_decoder, new_rate) = measure(&cx, &stream, window).await;
    cx.log(format_args!(
        "minimised, paused (after): process {new_cpu:.1} %, decoder threads {new_decoder:.1} %, {new_rate:.1} pictures/s decoded, {} decoder threads",
        hub.decoder_threads()
    ));
    ensure!(new_rate == 0., "{new_rate:.1} pictures/s decoded while minimised");
    ensure!(hub.decoder_threads() == 0, "a decoder thread runs while minimised");
    ensure!(!cx.update(move |_, cx| state(cx).stalled)?, "a pause shows as a stall");
    let watching = cx.update(move |_, cx| model(cx).live.voice.watching.read(cx).is_watching(peer_uid))?;
    ensure!(watching, "the watch (and the stream's sound with it) ended while minimised");

    // Restored: a keyframe is asked for at once and decoding picks up from it.
    let decoded = stream.stats().decoded;
    ensure!(winctl("activate")?, "winctl.sh activate failed");
    let restored = Instant::now();
    window_visible(&cx, true, Duration::from_secs(5)).await?;
    wait_for(&cx, Duration::from_secs(5), "pictures again", {
        let stream = stream.clone();
        move |_| stream.stats().decoded > decoded + 5
    })
    .await?;
    let asked = stream.stats().keyframe_requests - requests;
    cx.log(format_args!("restored: {asked} keyframe request(s), decoding again {} ms after the restore", restored.elapsed().as_millis()));
    ensure!(asked >= 1, "no keyframe was asked for on the return");
    let (_, _, back_rate) = measure(&cx, &stream, Duration::from_secs(2)).await;
    ensure!(back_rate > 50., "only {back_rate:.1} pictures/s after the restore");
    cx.log(format_args!(
        "decoder CPU while minimised: {old_decoder:.1} % of a core before, {new_decoder:.1} % now (process {old_cpu:.1} % → {new_cpu:.1} %)"
    ));
    cx.update(|_, cx| actions::voice::stop_watching_stream(peer_uid, cx))?;
    peer.cue("stop")?;
    if let Ok(line) = peer.wait_line(&cx, "STREAM-STOPPED", Duration::from_secs(15)).await {
        cx.log(format_args!("the streamer: {line}"));
    }
    leave(&cx).await?;
    Ok(())
}

// ── voice-golive-software ───────────────────────────────────────────────────────────────

/// What a Go Live needs on this desktop without touching it: a private session bus with nothing
/// on it but a fake ScreenCast portal (`fake_screencast_portal.py`), which hands the core a
/// PipeWire node this rig made (a GStreamer test pattern, 1280×720 at 30 fps). Every process is
/// ours and killed by its pid when the rig is dropped; the node goes with its process.
struct GoLiveRig {
    dir: Profile,
    bus_pid: u32,
    address: String,
    source: Child,
    portal: Child,
    node: u32,
}

const PORTAL_SCRIPT: &str = include_str!("fake_screencast_portal.py");
/// The portal's control files (`refuse`, `close`), for the check inside the app.
const PORTAL_CONTROL_ENV: &str = "DECIBELL_TEST_PORTAL_CONTROL";

impl GoLiveRig {
    fn start() -> Result<GoLiveRig> {
        let dir = Profile::new("golive-rig")?;
        let config = dir.0.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path={}</listen>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>"#,
                dir.0.join("bus").display()
            ),
        )?;
        // No service directories: nothing can be activated on it (not the real portal).
        let out = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--fork", "--print-pid=1", "--print-address=1"])
            .output()
            .context("starting dbus-daemon")?;
        ensure!(out.status.success(), "dbus-daemon: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let address = lines.next().context("dbus-daemon printed no address")?.to_string();
        let bus_pid: u32 = lines.next().and_then(|l| l.trim().parse().ok()).context("dbus-daemon printed no pid")?;
        let kill_bus = || {
            let _ = Command::new("kill").arg(bus_pid.to_string()).status();
        };
        let node_name = format!("decibell-test-screen-{}", std::process::id());
        let source = Command::new("gst-launch-1.0")
            .args([
                "-q",
                "videotestsrc",
                "is-live=true",
                "pattern=ball",
                "!",
                "video/x-raw,format=BGRx,width=1280,height=720,framerate=30/1",
                "!",
                "pipewiresink",
                "mode=provide",
                &format!("client-name={node_name}"),
                &format!("stream-properties=props,node.name={node_name},media.class=Video/Source"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.0.join("source.log"))?)
            .spawn();
        let mut source = match source {
            Ok(source) => source,
            Err(e) => {
                kill_bus();
                bail!("starting the test source: {e}");
            }
        };
        let started = Instant::now();
        let node = loop {
            if let Some(id) = pipewire_node(&node_name) {
                break id;
            }
            if started.elapsed() > Duration::from_secs(5) {
                let _ = source.kill();
                let _ = source.wait();
                kill_bus();
                bail!("the test source's PipeWire node {node_name} never appeared");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let script = dir.0.join("fake_screencast_portal.py");
        std::fs::write(&script, PORTAL_SCRIPT)?;
        std::fs::create_dir_all(dir.0.join("portal-control"))?;
        let portal = Command::new("python3")
            .arg("-I")
            .arg(&script)
            .args(["--bus", &address, "--node", &node.to_string(), "--control"])
            .arg(dir.0.join("portal-control"))
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(dir.0.join("portal.log"))?)
            .stderr(std::fs::File::create(dir.0.join("portal.stderr"))?)
            .spawn();
        let portal = match portal {
            Ok(portal) => portal,
            Err(e) => {
                let _ = source.kill();
                let _ = source.wait();
                kill_bus();
                bail!("starting the fake portal: {e}");
            }
        };
        let rig = GoLiveRig { dir, bus_pid, address, source, portal, node };
        let started = Instant::now();
        while !rig.portal_log().contains("PORTAL ready") {
            ensure!(
                started.elapsed() < Duration::from_secs(5),
                "the fake portal never got ready: {}",
                std::fs::read_to_string(rig.dir.0.join("portal.stderr")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(rig)
    }

    fn portal_log(&self) -> String {
        std::fs::read_to_string(self.dir.0.join("portal.log")).unwrap_or_default()
    }

    /// The app's environment: the private bus, the software encoder only, the portal's controls.
    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("DBUS_SESSION_BUS_ADDRESS".into(), self.address.clone()),
            ("DECIBELL_ENCODER".into(), "software".into()),
            (PORTAL_CONTROL_ENV.into(), self.dir.0.join("portal-control").display().to_string()),
        ]
    }
}

impl Drop for GoLiveRig {
    fn drop(&mut self) {
        for child in [&mut self.portal, &mut self.source] {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("kill").arg(self.bus_pid.to_string()).status();
    }
}

/// The id of the PipeWire node named `name` (`pw-cli ls Node`).
fn pipewire_node(name: &str) -> Option<u32> {
    let out = Command::new("pw-cli").args(["ls", "Node"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut id = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("id ") {
            id = rest.split(',').next().and_then(|n| n.trim().parse().ok());
        } else if line == format!("node.name = \"{name}\"") {
            return id;
        }
    }
    None
}

async fn parent_golive(cx: TestCx, name: &'static str) -> Result<()> {
    let have = |program: &str, args: &[&str]| {
        Command::new(program).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    };
    for (program, args, what) in [
        ("python3", &["-I", "-c", "import dbus, gi"][..], "python3 with dbus-python and PyGObject"),
        ("gst-inspect-1.0", &["pipewiresink"][..], "GStreamer's pipewiresink"),
        ("pw-cli", &["--version"][..], "pw-cli"),
        ("dbus-daemon", &["--version"][..], "dbus-daemon"),
    ] {
        if !have(program, args) {
            cx.log(format_args!("SKIP: {what} is missing (the fake portal and the test source need it)"));
            return Ok(());
        }
    }
    if stack()?.is_none() {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    }
    let rig = GoLiveRig::start()?;
    cx.log(format_args!("rig: a private bus, the test pattern as PipeWire node {}, the fake portal", rig.node));
    let result = parent_env(cx.clone(), name, ACCOUNT, rig.env()).await;
    for line in rig.portal_log().lines() {
        cx.log(format_args!("│ {line}"));
    }
    drop(rig);
    cx.log("rig stopped (portal, test source, bus)");
    result
}

fn portal_control(file: &str) -> Result<PathBuf> {
    let dir = std::env::var(PORTAL_CONTROL_ENV).context("no portal control directory")?;
    Ok(PathBuf::from(dir).join(file))
}

async fn check_golive_software(cx: TestCx) -> Result<()> {
    use decibell_model::live::voice::VideoCodec;
    use decibell_model::live::voice::go_live::{STREAM_PRESETS, StreamSettings};
    use decibell_model::live::voice::streams::OwnStreamPhase;
    let settings = |cx: &mut App| StreamSettings::of(model(cx).settings.read(cx).get());
    let start = cx.update(|_, cx| settings(cx))?;
    ready(&cx).await?;
    join(&cx, LOUNGE).await?;
    let me = me(&cx)?;
    let peer = Peer::start(&steps(&[
        "voice-join", "Lounge", "wait", "mls", "ready", "print", "joined",
        "wait", "stream", ACCOUNT, "av1", "unlocked", "print", "saw-av1-unlocked",
        "wait", "stream-gone", ACCOUNT, "print", "saw-end",
        "wait", "stream", ACCOUNT, "av1", "unlocked", "print", "saw-again",
        "wait-file", "@cue:squad", "voice-join", "Squad", "wait", "mls", "ready", "print", "in-squad",
        "wait", "stream", ACCOUNT, "av1", "unlocked", "print", "saw-carried",
        "wait-file", "@cue:quit", "voice-leave", "logout",
    ]))?;
    peer.wait_line(&cx, "MARK joined", Duration::from_secs(60)).await?;

    // The core's probe with DECIBELL_ENCODER=software: only the software encoder.
    cx.update(|_, cx| crate::codecs::ensure_caps(cx))?;
    wait_for(&cx, Duration::from_secs(30), "the encoder caps", |cx| {
        let e = crate::codecs::Encoders::global(cx);
        let e = e.read(cx);
        e.loaded && !e.caps.is_empty()
    })
    .await?;
    let caps = cx.update(|_, cx| crate::codecs::Encoders::global(cx).read(cx).caps.clone())?;
    for c in &caps {
        cx.log(format_args!("encoder: {} on {} ({}), ≤ {}x{} at {} fps, {} px/s", c.codec.human_name(), c.label, if c.hardware { "hardware" } else { "software" }, c.max_width, c.max_height, c.max_fps, c.max_pixel_rate));
    }
    ensure!(cx.update(|_, cx| crate::codecs::Encoders::global(cx).read(cx).software_only())?, "DECIBELL_ENCODER=software left a hardware encoder");
    // Auto, no sound (the sound tap would hear the desktop's apps), and the default Smooth, which
    // the software encoder can't keep up with.
    cx.update(|_, cx| {
        actions::voice::update_stream_settings(
            |s| {
                s.share_audio = false;
                s.enforced_codec = VideoCodec::Unknown;
                s.apply_preset(&STREAM_PRESETS[0]);
            },
            cx,
        )
    })?;
    let codecs = cx.update(|_, cx| {
        let first = model(cx).live.voice.codecs.read(cx).encoders.first().map(|e| e.codec);
        settings(cx).start_codecs(first)
    })?;
    cx.log(format_args!("Auto asks for {} unpinned", codecs.0.human_name()));
    ensure!(codecs == (VideoCodec::Av1, VideoCodec::Unknown), "Auto on a software-only machine asks for {codecs:?}");
    let smooth_fits = cx.update(|_, cx| {
        crate::codecs::Encoders::global(cx).read(cx).can_encode(decibell_model::live::voice::go_live::StreamResolution::P1080, 60)
    })?;
    click_probe(&cx, "dock-share").await?;
    wait_for(&cx, Duration::from_secs(5), "the Go Live dialog", |_| probes::flag_set("go-live-open")).await?;
    if smooth_fits {
        // A CPU with room for 1080p60 in software (the ceiling grows with the core count): nothing
        // is greyed, and Smooth stays. The test pattern is 720p30: stream it as Data saver.
        cx.sleep(Duration::from_millis(500)).await;
        ensure!(cx.update(|_, cx| settings(cx).preset().map(|p| p.id))? == Some("smooth"), "Smooth moved although this CPU can encode it");
        cx.log("this CPU encodes Smooth in software: nothing greyed, Smooth kept; Data saver chosen for the 720p30 test pattern");
        click_probe(&cx, "preset:saver").await?;
        wait_for(&cx, Duration::from_secs(3), "Data saver", move |cx| settings(cx).preset().is_some_and(|p| p.id == "saver")).await?;
    } else {
        let t = wait_for(&cx, Duration::from_secs(5), "the greyed Smooth to give way", move |cx| settings(cx).preset().is_some_and(|p| p.id == "saver")).await?;
        cx.log(format_args!("Smooth is past the software encoder: Data saver chosen {} ms after the dialog opened", t.as_millis()));
    }
    let planned = cx.update(|_, cx| {
        let s = settings(cx);
        crate::codecs::Encoders::global(cx).read(cx).planned(&s).map(|c| c.label.to_string())
    })?;
    ensure!(planned.as_deref() == Some("Software (SVT-AV1)"), "the dialog plans {planned:?}");

    // A refused picker: its error shows; reopened, the dialog starts without it.
    std::fs::write(portal_control("refuse")?, b"")?;
    click_probe(&cx, "go-live-start").await?;
    wait_for(&cx, Duration::from_secs(15), "the refusal in the dialog", |cx| model(cx).live.voice.own_stream.read(cx).error.is_some()).await?;
    let error = cx.update(|_, cx| model(cx).live.voice.own_stream.read(cx).error.clone())?.unwrap_or_default();
    cx.log(format_args!("the picker refused: \"{error}\" in the dialog"));
    ensure!(!error.contains("not available") && !error.contains("No hardware encoder"), "the start failed on the encoder, not at the picker: {error}");
    std::fs::remove_file(portal_control("refuse")?)?;
    click_probe(&cx, "go-live-close").await?;
    wait_for(&cx, Duration::from_secs(5), "the dialog to close", |_| !probes::flag_set("go-live-open")).await?;
    cx.sleep(Duration::from_millis(400)).await;
    click_probe(&cx, "dock-share").await?;
    wait_for(&cx, Duration::from_secs(5), "the Go Live dialog", |_| probes::flag_set("go-live-open")).await?;
    ensure!(cx.update(|_, cx| model(cx).live.voice.own_stream.read(cx).error.is_none())?, "the last attempt's error is still in the reopened dialog");
    cx.log("reopened: no stale error");

    // Go Live: the core falls through to SVT-AV1 and the capture runs on the test pattern.
    click_probe(&cx, "go-live-start").await?;
    let t = wait_for(&cx, Duration::from_secs(30), "the stream to go live", |cx| {
        let o = model(cx).live.voice.own_stream.read(cx);
        o.is_live() || (o.phase == OwnStreamPhase::Idle && o.error.is_some())
    })
    .await?;
    let (live, error) = cx.update(|_, cx| {
        let o = model(cx).live.voice.own_stream.read(cx);
        (o.live().cloned(), o.error.clone())
    })?;
    let live = live.with_context(|| format!("Go Live failed: {error:?}"))?;
    cx.log(format_args!(
        "live {} ms after Go Live: {} on \"{}\", {}x{} at {} fps, announced {}",
        t.as_millis(),
        live.codec.human_name(),
        live.encoder,
        live.width,
        live.height,
        live.fps,
        if live.enforced == VideoCodec::Unknown { "unlocked" } else { "locked" }
    ));
    ensure!(live.codec == VideoCodec::Av1, "the stream encodes in {}", live.codec.human_name());
    ensure!(live.encoder.as_ref() == "Software (SVT-AV1)", "the stream's encoder is {:?}", live.encoder);
    ensure!(live.enforced == VideoCodec::Unknown, "an Auto stream was announced locked");
    ensure!((live.width, live.height) == (1280, 720), "encodes at {}x{}", live.width, live.height);
    wait_for(&cx, Duration::from_secs(5), "the dialog to close once live", |_| !probes::flag_set("go-live-open")).await?;
    peer.wait_line(&cx, "MARK saw-av1-unlocked", Duration::from_secs(15)).await?;
    cx.log("the peer sees our stream: AV1, no enforced codec");

    // Our preview: the app decodes its own SVT-AV1 stream; the stream gain is 0 meanwhile and
    // back to the slider's value when the preview goes.
    cx.update(|_, cx| actions::voice::focus_stream(me, cx))?;
    let state = move |cx: &mut App| crate::video_events::StreamStates::global(cx).read(cx).get(me);
    let t = wait_for(&cx, Duration::from_secs(15), "our preview's first picture", move |cx| state(cx).picture.is_some()).await?;
    let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
    let stream = hub.stream(&me.to_string());
    let a = stream.stats();
    cx.sleep(Duration::from_secs(3)).await;
    let b = stream.stats();
    let decoded = (b.decoded - a.decoded) as f64 / 3.;
    cx.log(format_args!(
        "preview: first picture {} ms after focusing; {} decoded with {} at {decoded:.1} pictures/s; stream gain sent {:?}",
        t.as_millis(),
        b.codec.map(|c| c.label()).unwrap_or("?"),
        b.backend.map(|d| d.display_name(b.codec.unwrap_or(decibell_video::Codec::Av1))).unwrap_or("?"),
        crate::focus::stream_gain_sent()
    ));
    ensure!(b.codec == Some(decibell_video::Codec::Av1), "the preview isn't AV1");
    ensure!(decoded > 20., "the preview decoded {decoded:.1} pictures/s of a 30 fps stream");
    ensure!(crate::focus::stream_gain_sent() == Some(0.0), "the preview didn't mute the stream gain");
    cx.update(|_, cx| actions::voice::stop_watching_stream(me, cx))?;
    wait_for(&cx, Duration::from_secs(5), "the stream gain back after the preview", |_| crate::focus::stream_gain_sent() == Some(1.0)).await?;
    cx.log("preview closed: the stream gain is back to 100 %");

    // The desktop ends the capture (its "stop sharing"): the stream ends, announced as ended.
    std::fs::write(portal_control("close")?, b"")?;
    let t = wait_for(&cx, Duration::from_secs(10), "the stream to end with its capture", |cx| {
        model(cx).live.voice.own_stream.read(cx).phase == OwnStreamPhase::Idle
    })
    .await?;
    cx.log(format_args!("the portal closed the session: the stream ended {} ms later", t.as_millis()));
    peer.wait_line(&cx, "MARK saw-end", Duration::from_secs(15)).await?;
    cx.log("the peer saw it end");

    // Take stream with me (Settings → Codecs): a same-community switch while live carries the
    // stream (capture and encoder keep running) into the new channel; without it the switch ends it.
    click_probe(&cx, "dock-share").await?;
    wait_for(&cx, Duration::from_secs(5), "the Go Live dialog", |_| probes::flag_set("go-live-open")).await?;
    click_probe(&cx, "go-live-start").await?;
    wait_for(&cx, Duration::from_secs(30), "the stream to go live again", |cx| model(cx).live.voice.own_stream.read(cx).is_live()).await?;
    // The dialog closes once live and takes every click until it has faded out.
    wait_for(&cx, Duration::from_secs(5), "the dialog to close once live", |_| !probes::flag_set("go-live-open")).await?;
    cx.sleep(Duration::from_millis(400)).await;
    peer.wait_line(&cx, "MARK saw-again", Duration::from_secs(15)).await?;
    peer.cue("squad")?;
    peer.wait_line(&cx, "MARK in-squad", Duration::from_secs(30)).await?;
    let take_along = |on: bool, cx: &mut App| {
        model(cx).settings.update(cx, |s, cx| s.change(|v| v.take_stream_on_channel_switch = on, cx))
    };
    cx.update(move |_, cx| take_along(true, cx))?;
    join(&cx, SQUAD).await?;
    let squad = decibell_model::ChannelId::from(SQUAD);
    let t = wait_for(&cx, Duration::from_secs(15), "the stream carried into Squad", move |cx| {
        model(cx).live.voice.own_stream.read(cx).live().is_some_and(|l| l.channel.as_ref() == Some(&squad))
    })
    .await?;
    peer.wait_line(&cx, "MARK saw-carried", Duration::from_secs(15)).await?;
    cx.log(format_args!("switched to Squad with Take stream with me: carried ({} ms), the peer there sees it", t.as_millis()));
    cx.update(move |_, cx| take_along(false, cx))?;
    join(&cx, LOUNGE).await?;
    wait_for(&cx, Duration::from_secs(10), "the stream to end on the switch", |cx| {
        model(cx).live.voice.own_stream.read(cx).phase == OwnStreamPhase::Idle
    })
    .await?;
    cx.log("switched back without it: the stream ended");

    // The dialog doesn't outlive the session.
    click_probe(&cx, "dock-share").await?;
    wait_for(&cx, Duration::from_secs(5), "the Go Live dialog", |_| probes::flag_set("go-live-open")).await?;
    leave(&cx).await?;
    wait_for(&cx, Duration::from_secs(5), "the dialog to close with the session", |_| !probes::flag_set("go-live-open")).await?;
    cx.log("left voice: the Go Live dialog closed with the session");
    peer.cue("quit")?;
    cx.update(move |_, cx| actions::voice::update_stream_settings(|s| *s = start.clone(), cx))?;
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
