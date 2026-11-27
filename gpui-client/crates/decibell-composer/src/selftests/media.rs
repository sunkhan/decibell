//! Live checks of what a sent message carries to another client, read back by a
//! `decibell-headless` second client (built beside the app):
//!
//! - `composer-attach-media`: an MP4 and an MP3 attached from disk as the file dialog hands them
//!   over: their chips, what the server stored (the echo) and what the second client received
//!   carry the size, length, ThumbHash and thumbnails the Electron client stored for the same
//!   two files (`decibell-attachments/tests/assets/electron.md`).
//! - `composer-vault`: the stack's encrypted `#vault` (Daily Gaming): a message with a picture,
//!   sealed; a second member's client opens both (the channel's escrowed key), sees the real
//!   file name, size and picture facts from the envelope; an edit stays sealed; the delete
//!   through the trash and its confirmation takes the row away on both (the channel's only
//!   message: the pane shows the empty channel again, the list holds no row).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

use decibell_chat::RowAction;
use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
use serde_json::Value;

use super::*;

/// The stack's test identities (`tools/dev/README.md`): perf3 / perf4 / perf5 have E2EE keys.
const PASSPHRASE: &str = "decibell-m3-test-passphrase";
pub(super) const VAULT: &str = "vault";
pub(super) const VAULT_ACCOUNT: &str = "perf3";
const VAULT_PEER: &str = "perf4";
const MEDIA_PEER: &str = "perf3";
const COMMUNITY: &str = "Daily Gaming";

fn headless_exe() -> Option<PathBuf> {
    let candidate = std::env::current_exe().ok()?.parent()?.join("decibell-headless");
    candidate.exists().then_some(candidate)
}

/// A second client: `decibell-headless` running one script; its stdout lines arrive on a channel.
struct Peer {
    label: &'static str,
    process: Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
    _profile: TempDir,
}

impl Peer {
    fn start(label: &'static str, steps: &[&str]) -> Result<Peer> {
        let exe = headless_exe().context("decibell-headless is not built next to this binary")?;
        let profile = TempDir::new(&format!("{label}-peer"))?;
        let central = std::env::var("DECIBELL_CENTRAL").context("DECIBELL_CENTRAL is not set")?;
        let mut process = Command::new(exe)
            .arg("--user-data-dir")
            .arg(&profile.0)
            .args(["--timeout", "60"])
            .args(steps)
            .env("DECIBELL_CENTRAL", central)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting decibell-headless")?;
        let stdout = process.stdout.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new().name(format!("{label}-peer")).spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
        Ok(Peer { label, process, lines, seen: Vec::new(), _profile: profile })
    }

    /// Until a line starting with `prefix` (a FAILED line fails); returns it.
    async fn line(&mut self, cx: &TestCx, prefix: &str, limit: Duration) -> Result<String> {
        let started = Instant::now();
        loop {
            while let Ok(line) = self.lines.try_recv() {
                self.seen.push(line.clone());
                if line.starts_with("FAILED") {
                    bail!("the {} client: {line}", self.label);
                }
                if line.starts_with(prefix) {
                    return Ok(line);
                }
            }
            ensure!(started.elapsed() < limit, "the {} client never printed {prefix:?} (it printed {:?})", self.label, self.seen);
            cx.sleep(Duration::from_millis(30)).await;
        }
    }

    async fn mark(&mut self, cx: &TestCx, name: &str) -> Result<()> {
        self.line(cx, &format!("MARK {name}"), Duration::from_secs(90)).await.map(|_| ())
    }

    /// The next `dump chat …` line, as JSON.
    async fn chat_dump(&mut self, cx: &TestCx) -> Result<Value> {
        let line = self.line(cx, "DUMP chat", Duration::from_secs(90)).await?;
        let json = line.find('{').map(|at| &line[at..]).context("a dump without JSON")?;
        Ok(serde_json::from_str(json)?)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// The row whose text is `content` in a `dump chat` answer.
fn dumped<'a>(dump: &'a Value, content: &str) -> Option<&'a Value> {
    dump["messages"].as_array()?.iter().find(|m| m["content"].as_str() == Some(content))
}

/// What the Electron client stored for a file (`decibell-attachments/tests/assets/electron.md`).
struct Expected {
    name: &'static str,
    kind: decibell_attachments::AttachmentKind,
    size: (u32, u32),
    duration_ms: u32,
    mask: u32,
    placeholder: &'static str,
}

const MP4: Expected = Expected {
    name: "rot90.mp4",
    kind: decibell_attachments::AttachmentKind::Video,
    size: (90, 160),
    duration_ms: 1003,
    mask: 7,
    placeholder: "BwgGBADvDCDkzUOUeWgAAAAAAA==",
};
const MP3: Expected = Expected {
    name: "cbr.mp3",
    kind: decibell_attachments::AttachmentKind::Audio,
    size: (0, 0),
    duration_ms: 2040,
    mask: 0,
    placeholder: "",
};

/// Two ThumbHashes that draw the same placeholder (equal, or within 3 levels on average: the
/// two clients' decoders and scalers round pixels apart).
fn same_placeholder(ours: &str, theirs: &str) -> bool {
    if ours == theirs {
        return true;
    }
    let (Some(a), Some(b)) = (decibell_attachments::thumbhash::decode_b64(ours), decibell_attachments::thumbhash::decode_b64(theirs)) else {
        return false;
    };
    (a.width, a.height) == (b.width, b.height)
        && a.rgba.iter().zip(&b.rgba).map(|(x, y)| x.abs_diff(*y) as u32).sum::<u32>() as f32 / a.rgba.len() as f32 <= 3.
}

fn check_file(what: &str, expected: &Expected, kind: decibell_attachments::AttachmentKind, size: (u32, u32), duration_ms: u32, placeholder: &str) -> Result<()> {
    ensure!(kind == expected.kind, "{what} {}: kind {kind:?}, Electron's {:?}", expected.name, expected.kind);
    ensure!(
        (size, duration_ms) == (expected.size, expected.duration_ms),
        "{what} {}: {size:?} {duration_ms} ms, Electron stored {:?} {} ms",
        expected.name,
        expected.size,
        expected.duration_ms
    );
    ensure!(same_placeholder(placeholder, expected.placeholder), "{what} {}: ThumbHash {placeholder:?}, Electron's {:?}", expected.name, expected.placeholder);
    Ok(())
}

pub(super) async fn check_attach_media(cx: TestCx) -> Result<()> {
    if headless_exe().is_none() {
        cx.log("SKIP: decibell-headless is not built next to this binary (cargo build --release -p decibell-model --features local-central --bin decibell-headless)");
        return Ok(());
    }
    let composer = open(&cx, OFF_TOPIC).await?;
    let dir = TempDir::new("composer-attach-media")?;
    let tag = tag();
    let (mp4, mp3) = (dir.0.join(format!("{tag}-{}", MP4.name)), dir.0.join(format!("{tag}-{}", MP3.name)));
    std::fs::write(&mp4, include_bytes!("../../../decibell-attachments/tests/assets/rot90.mp4"))?;
    std::fs::write(&mp3, include_bytes!("../../../decibell-media/tests/assets/cbr.mp3"))?;
    let text = format!("media probe {tag}");
    let mut peer = Peer::start(
        "media",
        &[
            "login", MEDIA_PEER, "wait", "connected", COMMUNITY, "open-server", COMMUNITY, "history", OFF_TOPIC, "print", "ready",
            "wait-message", OFF_TOPIC, &text, "dump", "chat", OFF_TOPIC, "logout", "wait", "logged-out",
        ],
    )?;
    peer.mark(&cx, "ready").await?;

    // What the file dialog hands over: the chips carry the probe's facts.
    cx.update(|_, cx| composer.update(cx, |c, cx| c.attach(vec![UploadSource::Path(mp4.clone()), UploadSource::Path(mp3.clone())], cx)))?;
    wait_for(&cx, Duration::from_secs(20), "two chips", |cx| model(cx).chat.composer.read(cx).for_channel(daily_gaming(), OFF_TOPIC).count() == 2).await?;
    let chips = cx.update(|_, cx| {
        model(cx)
            .chat
            .composer
            .read(cx)
            .for_channel(daily_gaming(), OFF_TOPIC)
            .map(|p| (p.filename.to_string(), p.kind, (p.width, p.height), p.duration_ms, p.placeholder.to_string(), p.preview.is_some()))
            .collect::<Vec<_>>()
    })?;
    // Each chip comes when its file's probe ends (the renderer's `queueUpload`s aren't awaited
    // one by one either): matched by name.
    ensure!(chips.len() == 2, "{} chips", chips.len());
    for expected in [&MP4, &MP3] {
        let (name, kind, size, duration, placeholder, preview) =
            chips.iter().find(|c| c.0.ends_with(expected.name)).with_context(|| format!("no chip for {}", expected.name))?;
        check_file("the chip of", expected, *kind, *size, *duration, placeholder)?;
        ensure!(*preview == (expected.kind == decibell_attachments::AttachmentKind::Video), "{name}: a preview picture only for the video");
    }
    cx.log(format_args!("chips: {chips:?}"));

    send_with_attachments(&cx, &composer, &text).await?;
    let id = echoed(&cx, OFF_TOPIC, &text).await?;
    let result = async {
        wait_for(&cx, Duration::from_secs(30), "both files on the stored row", |cx| row(OFF_TOPIC, id, cx).is_some_and(|m| m.attachments.len() == 2)).await?;
        let stored = cx.update(|_, cx| row(OFF_TOPIC, id, cx).map(|m| m.attachments.to_vec()))?.context("the stored row")?;
        for expected in [&MP4, &MP3] {
            let a = stored.iter().find(|a| a.filename.ends_with(expected.name)).with_context(|| format!("{} isn't on the row", expected.name))?;
            check_file("the server stored", expected, a.kind, (a.width, a.height), a.duration_ms, &a.placeholder)?;
            ensure!(a.thumbnail_sizes_mask == expected.mask, "{}: thumbnail mask {}, Electron's {}", a.filename, a.thumbnail_sizes_mask, expected.mask);
        }
        cx.log(format_args!(
            "the server stored {:?}",
            stored.iter().map(|a| (a.filename.to_string(), a.width, a.height, a.duration_ms, a.thumbnail_sizes_mask)).collect::<Vec<_>>()
        ));
        let dump = peer.chat_dump(&cx).await?;
        let received = dumped(&dump, &text).context("the second client has no such row")?;
        let files = received["files"].as_array().context("no files in the dump")?;
        ensure!(files.len() == 2, "the second client sees {} files", files.len());
        for expected in [&MP4, &MP3] {
            let f = files
                .iter()
                .find(|f| f["filename"].as_str().is_some_and(|n| n.ends_with(expected.name)))
                .with_context(|| format!("the second client has no {}", expected.name))?;
            let kind = match f["kind"].as_str() {
                Some("Video") => decibell_attachments::AttachmentKind::Video,
                Some("Audio") => decibell_attachments::AttachmentKind::Audio,
                other => bail!("the second client's kind {other:?}"),
            };
            let n = |k: &str| f[k].as_u64().unwrap_or(u64::MAX) as u32;
            check_file("the second client received", expected, kind, (n("width"), n("height")), n("durationMs"), f["placeholder"].as_str().unwrap_or(""))?;
            ensure!(n("thumbnailSizesMask") == expected.mask, "the second client's mask {}", n("thumbnailSizesMask"));
        }
        cx.log("the second client received the same: 90 × 160, 1003 ms, mask 7, Electron's ThumbHash; 2040 ms");
        Ok::<(), anyhow::Error>(())
    }
    .await;
    delete(&cx, OFF_TOPIC, &[id]).await?;
    let _ = peer.line(&cx, "DONE", Duration::from_secs(30)).await;
    result
}

/// The identity keys unlocked as the passphrase dialog would, and the dialog gone.
async fn unlock(cx: &TestCx) -> Result<()> {
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
    // The dialog takes every click while open and for its 300 ms fade.
    wait_for(cx, Duration::from_secs(10), "the passphrase dialog to close", |cx| model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
    cx.sleep(Duration::from_millis(400)).await;
    Ok(())
}

pub(super) async fn check_vault(cx: TestCx) -> Result<()> {
    if headless_exe().is_none() {
        cx.log("SKIP: decibell-headless is not built next to this binary (cargo build --release -p decibell-model --features local-central --bin decibell-headless)");
        return Ok(());
    }
    wait_for(&cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    unlock(&cx).await?;
    let composer = open(&cx, VAULT).await?;
    let sealed = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).is_some_and(|c| c.channels.read(cx).list().iter().any(|c| &*c.id == VAULT && c.encrypted)))?;
    ensure!(sealed, "#vault is not an encrypted channel");
    let before = cx.update(|_, cx| model(cx).chat.channels.read(cx).get(daily_gaming(), VAULT).map_or(0, |s| s.read(cx).rows().len()))?;
    let tag = tag();
    let (text, edited) = (format!("sealed {tag}"), format!("sealed and edited {tag}"));
    let mut peer = Peer::start(
        "vault",
        &[
            "login", VAULT_PEER, "wait", "connected", COMMUNITY, "open-server", COMMUNITY, "e2ee-ready", PASSPHRASE, "history", VAULT,
            "print", "ready", "wait-message", VAULT, &text, "dump", "chat", VAULT, "wait-edited", VAULT, &edited, "dump", "chat", VAULT,
            "wait-gone", VAULT, &edited, "dump", "chat", VAULT, "logout", "wait", "logged-out",
        ],
    )?;
    peer.mark(&cx, "ready").await?;

    // A message with a picture, sealed on the way out.
    let dir = TempDir::new("composer-vault")?;
    let png = dir.0.join(format!("{tag}-sealed.png"));
    write_png(&png)?;
    let png_size = std::fs::metadata(&png)?.len();
    cx.update(|_, cx| composer.update(cx, |c, cx| c.attach(vec![UploadSource::Path(png.clone())], cx)))?;
    wait_for(&cx, Duration::from_secs(10), "the chip", |cx| model(cx).chat.composer.read(cx).for_channel(daily_gaming(), VAULT).count() == 1).await?;
    send_with_attachments(&cx, &composer, &text).await?;
    let id = echoed(&cx, VAULT, &text).await?;
    let result = vault_flow(&cx, &composer, &mut peer, id, &text, &edited, &png, png_size, before).await;
    if result.is_err() && cx.update(|_, cx| row(VAULT, id, cx).is_some())? {
        let _ = delete(&cx, VAULT, &[id]).await;
    }
    let _ = peer.line(&cx, "DONE", Duration::from_secs(30)).await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn vault_flow(
    cx: &TestCx,
    composer: &Entity<Composer>,
    peer: &mut Peer,
    id: i64,
    text: &str,
    edited: &str,
    png: &std::path::Path,
    png_size: u64,
    before: usize,
) -> Result<()> {
    let name = png.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Ours: sealed, opened, the file's real facts from the envelope with its key.
    wait_for(cx, Duration::from_secs(30), "the picture on the stored row", |cx| row(VAULT, id, cx).is_some_and(|m| m.attachments.len() == 1)).await?;
    let (encrypted, a) = cx.update(|_, cx| row(VAULT, id, cx).map(|m| (m.encrypted, m.attachments[0].clone())))?.context("the row")?;
    ensure!(encrypted, "our row is not sealed");
    ensure!(a.encrypted && a.key_b64.is_some() && a.chunk_bytes == 64 * 1024, "our attachment: encrypted {}, key {}, chunks {}", a.encrypted, a.key_b64.is_some(), a.chunk_bytes);
    ensure!(
        *a.filename == *name && *a.mime == *"image/png" && a.size_bytes as u64 == png_size && (a.width, a.height) == (64, 48) && !a.placeholder.is_empty() && a.thumbnail_sizes_mask == 7,
        "our attachment's facts: {:?} {:?} {} B {}×{} mask {} (the PNG is {png_size} B, 64×48)",
        a.filename,
        a.mime,
        a.size_bytes,
        a.width,
        a.height,
        a.thumbnail_sizes_mask
    );
    // What the community holds: stand-ins (read from its database when this machine has the
    // stack's files and sqlite3).
    match server_row(a.id) {
        Some(row) => {
            ensure!(row.starts_with("encrypted|application/octet-stream|"), "the server's row is not a stand-in: {row}");
            ensure!(row.ends_with("|1"), "the server's row is not flagged encrypted: {row}");
            cx.log(format_args!("the community stores a stand-in: {row} (name|mime|size|width|height|duration|encrypted)"));
        }
        None => cx.log("SKIP: the community's database isn't readable here (sqlite3, the stack's files): stand-ins not checked"),
    }

    // The second member: the message opened with the channel's escrowed key, the same facts.
    let dump = peer.chat_dump(cx).await?;
    let m = dumped(&dump, text).context("the second client has no such row")?;
    ensure!(m["encrypted"] == Value::Bool(true) && m["decryptError"] == "None", "the second client's row: {m}");
    let f = &m["files"][0];
    ensure!(
        f["filename"] == name.as_str() && f["mime"] == "image/png" && f["size"].as_u64() == Some(png_size) && f["width"] == 64 && f["height"] == 48 && f["key"] == true && f["encrypted"] == true && f["placeholder"] == a.placeholder.as_ref(),
        "the second client's file: {f}"
    );
    cx.log(format_args!("the second client opened it: {} with {name} ({png_size} B, 64×48, its key)", m["content"]));

    // An edit stays sealed.
    cx.update(|_, cx| actions::chat::edit_message(daily_gaming(), VAULT.into(), id, edited, cx))?;
    let dump = peer.chat_dump(cx).await?;
    let m = dumped(&dump, edited).context("the second client never saw the edit")?;
    ensure!(m["encrypted"] == true && m["edited"] == true && m["files"][0]["key"] == true, "the edit on the second client: {m}");
    wait_for(cx, Duration::from_secs(10), "our row edited", |cx| row(VAULT, id, cx).is_some_and(|m| *m.content == *edited && m.encrypted)).await?;
    cx.log("the edit stayed sealed, the attachment kept its key");

    // Delete through the UI: the trash on the hovered row, then the confirmation (Enter).
    let list = list_of(cx, composer)?;
    let body = cx.update(|_, cx| list.read(cx).body_point(id, 0.5))?.context("the row's text on screen")?;
    cx.mouse_move(body)?;
    cx.sleep(Duration::from_millis(100)).await;
    let trash = cx.update(|_, cx| list.read(cx).action_point(id, RowAction::Delete, cx))?.context("the trash")?;
    cx.click(trash)?;
    wait_for(cx, Duration::from_secs(5), "the delete confirmation", move |cx| {
        decibell_chat::shown_pane(cx).is_some_and(|pane| pane.read(cx).confirming_delete() == Some(id))
    })
    .await?;
    // Enter confirms as the dialog opens (the kit's ConfirmModal), after its entrance.
    cx.sleep(Duration::from_millis(350)).await;
    cx.keystroke("enter")?;
    wait_for(cx, Duration::from_secs(20), "the server to confirm the delete", move |cx| {
        let slice = model(cx).chat.channels.read(cx).get(daily_gaming(), VAULT).cloned();
        row(VAULT, id, cx).is_none() && !slice.is_some_and(|s| s.read(cx).is_deleting(id))
    })
    .await?;
    let dump = peer.chat_dump(cx).await?;
    ensure!(dumped(&dump, edited).is_none(), "the second client still has the row");
    // The list follows its slice even when the pane no longer draws it.
    cx.sleep(Duration::from_millis(300)).await;
    let (in_list, rows) = cx.update(|_, cx| (list.read(cx).row(id).is_some(), list.read(cx).row_count()))?;
    ensure!(!in_list, "the list still holds the deleted row ({rows} rows)");
    if before == 0 {
        let body = cx.update(|_, cx| decibell_chat::shown_pane(cx).and_then(|pane| pane.read(cx).body(cx)))?;
        ensure!(body == Some(decibell_chat::PaneBody::Welcome), "the emptied channel shows {body:?}, not its welcome");
        cx.log("deleted through the trash and the confirmation: gone on both, the list holds no row, the channel shows empty again");
    } else {
        cx.log(format_args!("deleted through the trash and the confirmation: gone on both (the channel had {before} rows before)"));
    }
    Ok(())
}

/// The community's own row for an attachment: `filename|mime|size|width|height|duration|encrypted`.
fn server_row(attachment: i64) -> Option<String> {
    let dev = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"));
    let db = dev.join("run/communities/gaming/community.db");
    if !db.exists() {
        return None;
    }
    let out = Command::new("sqlite3")
        .arg("-readonly")
        .arg(&db)
        .arg(format!("select filename, mime, size_bytes, width, height, duration_ms, encrypted from attachments where id = {attachment}"))
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}
