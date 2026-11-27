//! `--check media-perf`: attachment playback held still for an outside measurement
//! (`tools/perf/scenarios.sh gpui media-play-h264 | media-play-vp9`, `tools/perf/README.md`).
//!
//! In the signed-in app's main window, on the local stack: the clip named by
//! `DECIBELL_MEDIA_CLIP` is uploaded to `#off-topic`, and the check then does what the lines
//! written to the FIFO `DECIBELL_MEDIA_PERF_CONTROL` say, answering each with one line on
//! stdout that starts with `media-perf:`:
//!
//! | Command | Does | Answer |
//! |---|---|---|
//! | (start) | signs in, opens the channel, posts the clip | `media-perf: ready {pid, message, attachment, filename, bytes}` |
//! | `play` | clicks the clip's card, waits for its first picture | `media-perf: playing {clip, codec, decoder, path, width, height, fps, first_picture_ms, …}` |
//! | `restart` | seeks to the start (a measurement window begins behind it) | `media-perf: restarted {…}` |
//! | `stats [label]` | nothing | `media-perf: stats {…}`: the player's and the hub's counters, the window's frames |
//! | `stop` | drops the player, waits for its threads to end | `media-perf: stopped {…}` |
//! | `quit` | stops, deletes the message | `media-perf: deleted {message}` and the check ends |
//!
//! Nothing runs between two commands: the control thread sleeps in `read(2)` and the check in
//! a channel, so what the script measures is the client playing, not the check. The sound goes
//! to the null sink (`DECIBELL_MEDIA_OUTPUT=null`, required). Without `play` the check is only
//! the clip's poster: the Electron half of the scenario plays the same upload.

use std::io::BufRead as _;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, ensure};
use decibell_attachments::upload::UploadSource;
use decibell_harness::{SelfTest, TestCx};
use decibell_media::Phase;
use decibell_model::{ChannelId, actions};
use futures::StreamExt as _;
use gpui::Entity;
use serde_json::{Value, json};

use crate::selftests::{daily_gaming, media_threads, model, open_channel, private_mb, wait_for};

pub(crate) const CHECK: SelfTest = SelfTest { name: "media-perf", about: "(in the app)", run: |cx| Box::pin(check(cx)) };

fn say(what: &str, fields: Value) {
    println!("media-perf: {what} {fields}");
}

/// The lines written to the control FIFO, one at a time. The thread blocks in `open` / `read`
/// (a FIFO reads EOF when its writer closes: it is opened again for the next one).
fn commands(path: std::path::PathBuf) -> Result<futures::channel::mpsc::UnboundedReceiver<String>> {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    std::thread::Builder::new()
        .name("decibell-test-control".into())
        .spawn(move || {
            loop {
                let Ok(file) = std::fs::File::open(&path) else { return };
                for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                    if tx.unbounded_send(line.trim().to_string()).is_err() {
                        return;
                    }
                }
            }
        })
        .context("the control thread")?;
    Ok(rx)
}

struct Run {
    cx: TestCx,
    channel: ChannelId,
    list: Entity<decibell_chat::MessageList>,
    message: i64,
    attachment: i64,
    filename: String,
    started: Instant,
}

impl Run {
    /// Everything the client counts about the playing video, at one instant.
    fn stats(&self, label: &str) -> Result<Value> {
        let key = format!("media:{}:{}", daily_gaming().wire(), self.attachment);
        let attachment = self.attachment;
        self.cx.update(|window, cx| {
            let hub = decibell_video::VideoHub::global(cx);
            let stream = hub.stream(&key).stats();
            let playback = crate::playback::Playback::global(cx);
            let report = playback.as_ref().and_then(|p| p.read(cx).video_report());
            let (position, duration) = playback.as_ref().map_or((0, 0), |p| p.read(cx).video_times());
            let active = model(cx).chat.video.read(cx).active.as_ref().map(|a| a.attachment_id);
            let (play, status) = report.unzip();
            let video = status.as_ref().and_then(|s| s.video);
            let phase = status.as_ref().map(|s| match &s.phase {
                Phase::Loading => "loading".to_string(),
                Phase::Paused => "paused".to_string(),
                Phase::Playing => "playing".to_string(),
                Phase::Ended => "ended".to_string(),
                Phase::Failed(why) => format!("failed: {why}"),
            });
            let play = play.unwrap_or_default();
            json!({
                "label": label,
                "t_ms": self.started.elapsed().as_millis() as u64,
                "playing_this": active == Some(attachment),
                "phase": phase,
                "position_ms": position / 1000,
                "duration_ms": duration / 1000,
                "clip": self.filename,
                "codec": video.map(|v| v.codec.label()),
                "fps": video.map(|v| v.frame_rate),
                "width": video.map(|v| v.width),
                "height": video.map(|v| v.height),
                // The player's word for the decoder (`Status.video.backend`), then the hub's.
                "decoder": video.and_then(|v| v.backend).map(|b| b.label()),
                "hub_decoder": stream.backend.map(|b| b.label()),
                "path": stream.path.map(|p| p.label()),
                "fallback_reason": stream.fallback_reason.as_deref(),
                // The player (decibell_media::PlayStats).
                "shown": play.shown,
                "late": play.late,
                "skipped": play.skipped,
                "max_late_us": play.max_late,
                "seeks": play.seeks,
                "streams_opened": play.streams_opened,
                // The hub (decibell_video::StreamStats).
                "received": stream.received,
                "decoded": stream.decoded,
                "published": stream.published,
                "presented": stream.presented,
                "replaced": stream.replaced,
                "dropped": stream.dropped,
                "errors": stream.errors,
                "decoder_opens": stream.decoder_opens,
                "fallbacks": stream.fallbacks,
                "decode_ms_p50": stream.decode_ms_p50,
                "decode_ms_p99": stream.decode_ms_p99,
                "present_interval_ms_p50": stream.present_interval_ms_p50,
                "present_interval_ms_p99": stream.present_interval_ms_p99,
                // The window (TestCx::frames).
                "frames_drawn": window.frames_drawn(),
                "frames_presented": window.frames_presented(),
                "window_active": window.is_window_active(),
                "private_mb": (private_mb() * 10.).round() / 10.,
                "threads": media_threads(),
            })
        })
    }

    async fn play(&self) -> Result<Value> {
        let (cx, attachment) = (&self.cx, self.attachment);
        // The card is the newest row: its surface's box, as the list drew it.
        cx.update(|_, cx| {
            if let Some(hosts) = cx.try_global::<crate::player::VideoHosts>() {
                hosts.bounds.borrow_mut().clear();
            }
            cx.notify(self.list.entity_id());
        })?;
        wait_for(cx, Duration::from_secs(15), "the clip's card", move |cx| {
            cx.try_global::<crate::player::VideoHosts>().is_some_and(|hosts| hosts.bounds.borrow().contains_key(&attachment))
        })
        .await?;
        let at = cx
            .update(|_, cx| cx.try_global::<crate::player::VideoHosts>().and_then(|hosts| hosts.bounds.borrow().get(&attachment).map(|b| b.center())))?
            .context("the card has no box")?;
        let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
        let stream = hub.stream(&format!("media:{}:{attachment}", daily_gaming().wire()));
        let (before, clicked) = (stream.stats().presented, Instant::now());
        cx.click(at)?;
        let s = stream.clone();
        wait_for(cx, Duration::from_secs(30), "the first picture", move |_| s.stats().presented > before).await?;
        let first = clicked.elapsed();
        // The pointer leaves the card, so its controls fade as they do for someone watching.
        cx.mouse_move(gpui::point(gpui::px(2.), gpui::px(2.)))?;
        let mut stats = self.stats("playing")?;
        stats["first_picture_ms"] = json!(first.as_millis() as u64);
        Ok(stats)
    }

    async fn stop(&self) -> Result<Value> {
        self.cx.update(|_, cx| {
            let m = model(cx);
            m.chat.video.update(cx, |v, cx| v.set_active(None, cx));
            m.chat.audio.update(cx, |a, cx| a.set_active(None, cx));
        })?;
        wait_for(&self.cx, Duration::from_secs(10), "the player's threads to end", |_| media_threads().is_empty()).await?;
        self.stats("stopped")
    }

    async fn delete(&self) -> Result<()> {
        let (channel, message, list) = (self.channel.clone(), self.message, self.list.clone());
        self.cx.update(|_, cx| actions::chat::delete_message(daily_gaming(), channel, message, cx))?;
        wait_for(&self.cx, Duration::from_secs(15), "the clip's message to go", move |cx| list.read(cx).row(message).is_none()).await?;
        say("deleted", json!({ "message": message }));
        Ok(())
    }
}

async fn check(cx: TestCx) -> Result<()> {
    ensure!(std::env::var("DECIBELL_MEDIA_OUTPUT").is_ok_and(|v| v == "null"), "media-perf runs with DECIBELL_MEDIA_OUTPUT=null (nothing audible)");
    let clip = std::env::var_os("DECIBELL_MEDIA_CLIP").context("media-perf needs DECIBELL_MEDIA_CLIP=<file> (tools/perf/media-clips.sh)")?;
    let clip = std::fs::canonicalize(&clip).with_context(|| format!("DECIBELL_MEDIA_CLIP={}", std::path::Path::new(&clip).display()))?;
    let control = std::env::var_os("DECIBELL_MEDIA_PERF_CONTROL").context("media-perf needs DECIBELL_MEDIA_PERF_CONTROL=<fifo> (tools/perf/scenarios.sh drives it)")?;
    let mut commands = commands(control.into())?;
    let started = Instant::now();
    let filename = clip.file_name().map(|n| n.to_string_lossy().into_owned()).context("the clip has no file name")?;
    let bytes = std::fs::metadata(&clip)?.len();

    let (channel, list) = open_channel(&cx, "off-topic").await?;
    let newest_before = cx.update(|_, cx| list.read(cx).visible_ids().last().copied().unwrap_or(0))?;
    // From disk, as a picked file.
    cx.update(|_, cx| actions::chat::attach_file(daily_gaming(), channel.clone(), UploadSource::Path(clip), cx))?;
    let ch = channel.clone();
    wait_for(&cx, Duration::from_secs(30), "the clip to be attached", move |cx| model(cx).chat.composer.read(cx).for_channel(daily_gaming(), &ch).count() > 0).await?;
    // The send waits for the upload itself.
    cx.update(|_, cx| actions::chat::send_message(daily_gaming(), channel.clone(), "", 0, cx))?;
    let (l, n) = (list.clone(), filename.clone());
    let found = move |cx: &gpui::App| {
        let l = l.read(cx);
        l.visible_ids().into_iter().filter(|id| *id > newest_before).find_map(|id| {
            let (m, _) = l.row(id)?;
            let a = m.attachments.iter().find(|a| *a.filename == *n)?;
            Some((id, a.id))
        })
    };
    let f = found.clone();
    wait_for(&cx, Duration::from_secs(120), "the clip's echo", move |cx| f(cx).is_some()).await?;
    let (message, attachment) = cx.update(|_, cx| found(cx))?.context("the clip's message")?;
    let run = Run { cx: cx.clone(), channel, list, message, attachment, filename: filename.clone(), started };
    say("ready", json!({ "pid": std::process::id(), "message": message, "attachment": attachment, "filename": filename, "bytes": bytes, "private_mb": (private_mb() * 10.).round() / 10. }));

    let result = async {
        while let Some(line) = commands.next().await {
            let (command, argument) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            match command {
                "play" => say("playing", run.play().await?),
                "restart" => {
                    let playback = cx.update(|_, cx| crate::playback::Playback::global(cx))?.context("no playback")?;
                    cx.update(|_, cx| playback.update(cx, |p, cx| p.seek_video_to(0, cx)))?;
                    // The seek has landed once the position is back near the start.
                    let p = playback.clone();
                    wait_for(&cx, Duration::from_secs(10), "the seek to the start", move |cx| p.read(cx).video_times().0 < 1_500_000).await?;
                    say("restarted", run.stats("restarted")?);
                }
                "stats" => say("stats", run.stats(argument)?),
                "stop" => say("stopped", run.stop().await?),
                "quit" => break,
                "" => {}
                other => say("unknown", json!({ "command": other })),
            }
        }
        anyhow::Ok(())
    }
    .await;
    // Whatever happened: nothing playing, nothing left on the stack.
    let _ = run.stop().await;
    run.delete().await?;
    result
}
