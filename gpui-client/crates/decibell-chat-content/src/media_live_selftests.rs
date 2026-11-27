//! Attachment playback against the local stack, beyond `media-live`:
//!
//! - `media-sealed` (as `perf3`, whose identity keys exist): a clip uploaded to the encrypted
//!   `#vault`, played from the attachment service's range reader with the chunk decryption in
//!   the path. What the server holds is not the plaintext, a seek lands on its time and opens
//!   only the chunks it needs, and no file on disk (the profile, the temp directory, anything
//!   this process holds open) ever contains the clip.
//! - `media-memory` (as `perf2`): private memory before, while and after a clip is played from
//!   the server and dropped, three times over, by kind of mapping and with glibc's own account
//!   of its heap: a leak grows with every cycle, an allocator's kept pages do not.
//!
//! `DECIBELL_MEDIA_CLIP=<file>` replaces the 137 KB test clip in both (a 1080p60 file for the
//! numbers that need one). Both delete what they post.

use decibell_harness::SelfTest;

use crate::selftests::parent_as;

pub(crate) const SEALED: SelfTest = SelfTest {
    name: "media-sealed",
    about: "a clip in the encrypted #vault (local stack, perf3): the server holds ciphertext, it plays through the chunk decryption, a seek opens only the chunks it needs, nothing plaintext on disk; deleted again",
    run: |cx| Box::pin(parent_as(cx, "media-sealed", "perf3")),
};

pub(crate) const MEMORY: SelfTest = SelfTest {
    name: "media-memory",
    about: "private memory before / while / after a clip is played from the local stack and dropped, three cycles, by mapping and by glibc's heap account; under 1.5 MB of growth per cycle after the first",
    run: |cx| Box::pin(parent_as(cx, "media-memory", "perf2")),
};

#[cfg(not(target_os = "linux"))]
pub(crate) async fn check_sealed(cx: decibell_harness::TestCx) -> anyhow::Result<()> {
    cx.log("SKIP: media-sealed reads /proc and walks the temp directory (Linux only)");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) async fn check_memory(cx: decibell_harness::TestCx) -> anyhow::Result<()> {
    cx.log("SKIP: media-memory reads /proc/self/smaps and glibc's mallinfo2 (Linux only)");
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{check_memory, check_sealed};

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use anyhow::{Context as _, Result, bail, ensure};
    use decibell_attachments::media::{ReaderTotals, reader_totals};
    use decibell_attachments::upload::UploadSource;
    use decibell_harness::TestCx;
    use decibell_media::Phase;
    use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
    use decibell_model::{ChannelId, actions};
    use gpui::{Entity, Pixels, Point};

    use crate::selftests::{daily_gaming, media_threads, model, open_channel, private_mb, wait_for};

    /// The stack's test identities (`tools/dev/README.md`).
    const PASSPHRASE: &str = "decibell-m3-test-passphrase";

    // ── What both checks do ─────────────────────────────────────────────────────────────

    /// The clip: `DECIBELL_MEDIA_CLIP` (uploaded from disk), else the media crate's test file.
    struct Clip {
        bytes: Arc<Vec<u8>>,
        path: Option<PathBuf>,
    }

    impl Clip {
        fn load() -> Result<Clip> {
            match std::env::var_os("DECIBELL_MEDIA_CLIP") {
                Some(path) => {
                    let path = std::fs::canonicalize(&path).with_context(|| format!("DECIBELL_MEDIA_CLIP={}", Path::new(&path).display()))?;
                    Ok(Clip { bytes: Arc::new(std::fs::read(&path)?), path: Some(path) })
                }
                None => Ok(Clip { bytes: Arc::new(include_bytes!("../../decibell-media/tests/assets/sync60.mp4").to_vec()), path: None }),
            }
        }

        fn source(&self, name: &str) -> UploadSource {
            match &self.path {
                // From disk, as a picked file: the process never holds the whole clip for it.
                Some(path) => UploadSource::Path(path.clone()),
                None => UploadSource::Memory { name: name.to_string(), mime: "video/mp4".into(), data: bytes::Bytes::from(self.bytes.to_vec()) },
            }
        }

        fn name(&self, tag: &str) -> String {
            match &self.path {
                Some(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                None => format!("selftest-{tag}-{}.mp4", std::process::id()),
            }
        }
    }

    /// Attaches `source` in `channel`, sends it and waits for its echo: (message, attachment).
    async fn post(cx: &TestCx, channel: &ChannelId, list: &Entity<decibell_chat::MessageList>, source: UploadSource, name: &str) -> Result<(i64, i64)> {
        let newest_before = cx.update(|_, cx| list.read(cx).visible_ids().last().copied().unwrap_or(0))?;
        cx.update(|_, cx| actions::chat::attach_file(daily_gaming(), channel.clone(), source, cx))?;
        let ch = channel.clone();
        wait_for(cx, Duration::from_secs(30), "the clip to be attached", move |cx| model(cx).chat.composer.read(cx).for_channel(daily_gaming(), &ch).count() > 0).await?;
        // The send waits for the upload itself.
        cx.update(|_, cx| actions::chat::send_message(daily_gaming(), channel.clone(), "", 0, cx))?;
        let (l, n) = (list.clone(), name.to_string());
        let found = move |cx: &gpui::App| {
            let l = l.read(cx);
            l.visible_ids().into_iter().filter(|id| *id > newest_before).find_map(|id| {
                let (m, _) = l.row(id)?;
                let a = m.attachments.iter().find(|a| *a.filename == *n)?;
                Some((id, a.id))
            })
        };
        let f = found.clone();
        wait_for(cx, Duration::from_secs(60), "the clip's echo", move |cx| f(cx).is_some()).await?;
        cx.update(|_, cx| found(cx))?.context("the clip's message")
    }

    async fn delete(cx: &TestCx, channel: &ChannelId, list: &Entity<decibell_chat::MessageList>, message: i64) -> Result<()> {
        cx.update(|_, cx| actions::chat::delete_message(daily_gaming(), channel.clone(), message, cx))?;
        let l = list.clone();
        wait_for(cx, Duration::from_secs(15), "the clip's message to go", move |cx| l.read(cx).row(message).is_none()).await?;
        cx.log(format_args!("deleted message {message}"));
        Ok(())
    }

    /// The centre of the attachment's video surface, once the list drew it.
    async fn card(cx: &TestCx, list: &Entity<decibell_chat::MessageList>, attachment: i64) -> Result<Point<Pixels>> {
        cx.update(|_, cx| {
            if let Some(hosts) = cx.try_global::<crate::player::VideoHosts>() {
                hosts.bounds.borrow_mut().clear();
            }
            cx.notify(list.entity_id());
        })?;
        wait_for(cx, Duration::from_secs(10), "the clip's card", move |cx| {
            cx.try_global::<crate::player::VideoHosts>().is_some_and(|hosts| hosts.bounds.borrow().contains_key(&attachment))
        })
        .await?;
        cx.update(|_, cx| cx.try_global::<crate::player::VideoHosts>().and_then(|hosts| hosts.bounds.borrow().get(&attachment).map(|b| b.center())))?.context("no box")
    }

    async fn stop_players(cx: &TestCx) -> Result<()> {
        cx.update(|_, cx| {
            let m = model(cx);
            m.chat.video.update(cx, |v, cx| v.set_active(None, cx));
            m.chat.audio.update(cx, |a, cx| a.set_active(None, cx));
        })?;
        wait_for(cx, Duration::from_secs(10), "the players' threads to end", |_| media_threads().is_empty()).await?;
        Ok(())
    }

    fn kb(bytes: u64) -> String {
        format!("{:.1} KB", bytes as f64 / 1024.)
    }

    // ── media-sealed ────────────────────────────────────────────────────────────────────

    /// Unlocks the account's identity keys as the passphrase dialog would.
    async fn unlock(cx: &TestCx) -> Result<()> {
        wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
        wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
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
                            model(cx).live.e2ee.update(cx, |e, cx| e.open(mode, cx));
                            actions::e2ee::submit_passphrase(PASSPHRASE.into(), PASSPHRASE.into(), cx);
                        })?;
                    }
                }
                E2eeStatus::Unavailable => {}
            }
            ensure!(started.elapsed() < Duration::from_secs(30), "the E2EE keys never became ready ({status:?})");
            cx.sleep(Duration::from_millis(100)).await;
        }
        // The dialog takes every click until it has closed and faded out.
        wait_for(cx, Duration::from_secs(10), "the passphrase dialog to close", |cx| model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
        cx.sleep(Duration::from_millis(400)).await;
        Ok(())
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        let first = needle[0];
        let mut from = 0;
        while let Some(at) = haystack[from..].iter().position(|b| *b == first) {
            let at = from + at;
            if haystack[at..].starts_with(needle) {
                return true;
            }
            from = at + 1;
        }
        false
    }

    /// What marks a file as holding the clip: its first 64 bytes and 32 from two places inside.
    struct Marks {
        marks: Vec<Vec<u8>>,
        size: u64,
        sealed_size: u64,
        /// The clip's own file, when it was uploaded from disk.
        source: Option<PathBuf>,
    }

    impl Marks {
        fn of(clip: &Clip, sealed_size: u64) -> Marks {
            let b = &clip.bytes;
            let marks = vec![b[..64].to_vec(), b[b.len() / 2..b.len() / 2 + 32].to_vec(), b[b.len() * 3 / 4..b.len() * 3 / 4 + 32].to_vec()];
            Marks { marks, size: b.len() as u64, sealed_size, source: clip.path.clone() }
        }

        fn in_bytes(&self, bytes: &[u8]) -> bool {
            self.marks.iter().any(|m| contains(bytes, m))
        }

        /// Why `path` (of `size` bytes) looks like the clip, if it does.
        fn in_file(&self, path: &Path, size: u64) -> Option<String> {
            if self.source.as_deref() == std::fs::canonicalize(path).ok().as_deref() && self.source.is_some() {
                return None;
            }
            // The size alone gives a copy away, sealed or not (within 2 %: a container around it).
            for (what, expected) in [("the clip's size", self.size), ("the sealed clip's size", self.sealed_size)] {
                if size >= expected && size <= expected + expected / 50 {
                    return Some(format!("{} is {size} B, {what}", path.display()));
                }
            }
            if !(32..=512 << 20).contains(&size) {
                return None;
            }
            let bytes = std::fs::read(path).ok()?;
            self.in_bytes(&bytes).then(|| format!("{} ({size} B) contains the clip's bytes", path.display()))
        }
    }

    fn walk(dir: &Path, depth: usize, uid: u32, out: &mut Vec<(PathBuf, std::fs::Metadata)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else { continue };
            if meta.uid() != uid {
                continue;
            }
            if meta.is_dir() {
                if depth > 0 {
                    walk(&entry.path(), depth - 1, uid, out);
                }
            } else if meta.is_file() {
                out.push((entry.path(), meta));
            }
        }
    }

    /// No file of the profile, no file of ours made in the temp directory since `since`, and no
    /// file this process holds open contains the clip or has its size. Logs what the profile holds.
    fn nothing_on_disk(cx: &TestCx, when: &str, marks: &Marks, since: SystemTime, log_profile: bool) -> Result<()> {
        let started = Instant::now();
        let uid = std::fs::metadata("/proc/self")?.uid();
        let profile = PathBuf::from(std::env::var_os("DECIBELL_USER_DATA_DIR").context("no DECIBELL_USER_DATA_DIR")?);
        let mut found = Vec::new();

        let mut files = Vec::new();
        walk(&profile, 12, uid, &mut files);
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let total: u64 = files.iter().map(|(_, m)| m.len()).sum();
        if log_profile {
            cx.log(format_args!("the profile ({}) holds {} files, {}:", profile.display(), files.len(), kb(total)));
            for (path, meta) in &files {
                cx.log(format_args!("  {:>9} B  {}", meta.len(), path.strip_prefix(&profile).unwrap_or(path).display()));
            }
        }
        found.extend(files.iter().filter_map(|(path, meta)| marks.in_file(path, meta.len())));
        let in_profile = files.len();

        // The temp directory: what is ours and was written since the test began (the profile's
        // parent, with the test's private bus, is in it).
        let mut temp = Vec::new();
        walk(&std::env::temp_dir(), 6, uid, &mut temp);
        let recent: Vec<_> = temp.into_iter().filter(|(path, meta)| !path.starts_with(&profile) && meta.modified().is_ok_and(|m| m >= since)).collect();
        found.extend(recent.iter().filter_map(|(path, meta)| marks.in_file(path, meta.len())));

        // What this process holds open, unlinked files included.
        let mut open = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd")?.flatten() {
            let Ok(meta) = std::fs::metadata(entry.path()) else { continue };
            if !meta.is_file() {
                continue;
            }
            let target = std::fs::read_link(entry.path()).unwrap_or_default();
            if let Some(why) = marks.in_file(&entry.path(), meta.len()) {
                found.push(format!("open as {}: {why}", target.display()));
            }
            open.push(format!("{} ({} B)", target.display(), meta.len()));
        }
        open.sort();
        cx.log(format_args!(
            "{when}: no plaintext in {in_profile} profile files, {} files of ours written to {} since the start, {} regular files held open {:?} ({} ms)",
            recent.len(),
            std::env::temp_dir().display(),
            open.len(),
            open,
            started.elapsed().as_millis()
        ));
        ensure!(found.is_empty(), "{when}: the clip is on disk: {found:?}");
        Ok(())
    }

    fn log_totals(cx: &TestCx, what: &str, t: ReaderTotals, size: u64, chunks: u64) {
        cx.log(format_args!(
            "{what}: {} range request(s), {} fetched ({:.0} % of the file), {} of {chunks} chunks opened, {} read by the decoders",
            t.ranges,
            kb(t.fetched),
            t.fetched as f64 * 100. / size as f64,
            t.chunks,
            kb(t.read)
        ));
    }

    pub(crate) async fn check_sealed(cx: TestCx) -> Result<()> {
        ensure!(std::env::var("DECIBELL_MEDIA_OUTPUT").is_ok_and(|v| v == "null"), "media self-tests run with DECIBELL_MEDIA_OUTPUT=null");
        let began = SystemTime::now() - Duration::from_secs(1);
        let clip = Clip::load()?;
        unlock(&cx).await?;
        let (channel, list) = open_channel(&cx, "vault").await?;
        let sealed_channel = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).is_some_and(|c| c.channels.read(cx).list().iter().any(|c| c.id == channel && c.encrypted)))?;
        ensure!(sealed_channel, "#vault is not an encrypted channel");
        let name = clip.name("sealed");
        let (message, attachment) = post(&cx, &channel, &list, clip.source(&name), &name).await?;
        let result = sealed_playback(&cx, &clip, &channel, &list, message, attachment, began).await;
        // Whatever happened: nothing stays playing, nothing stays posted.
        let _ = stop_players(&cx).await;
        delete(&cx, &channel, &list, message).await?;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn sealed_playback(cx: &TestCx, clip: &Clip, channel: &ChannelId, list: &Entity<decibell_chat::MessageList>, message: i64, attachment: i64, began: SystemTime) -> Result<()> {
        let size = clip.bytes.len() as u64;
        // The model: a sealed message, a sealed attachment with its key and the real size.
        let (sealed_message, sealed, has_key, chunk_bytes, model_size) = cx
            .update(|_, cx| {
                let l = list.read(cx);
                let (m, _) = l.row(message)?;
                let a = m.attachments.iter().find(|a| a.id == attachment)?;
                Some((m.encrypted, a.encrypted, a.key_b64.is_some(), a.chunk_bytes as u64, a.size_bytes as u64))
            })?
            .context("the clip's row")?;
        ensure!(sealed_message && sealed && has_key, "the model does not flag it encrypted (message {sealed_message}, attachment {sealed}, key {has_key})");
        ensure!(chunk_bytes == 64 * 1024, "sealed in {chunk_bytes} B chunks, expected 64 KiB");
        ensure!(model_size == size, "the envelope says {model_size} B, the clip has {size}");
        let chunks = decibell_attachments::crypto::chunk_count(size, chunk_bytes);
        let sealed_size = decibell_attachments::crypto::ciphertext_size(size, chunk_bytes);
        let marks = Marks::of(clip, sealed_size);

        // What the server holds: fetched with no key applied.
        let service = decibell_model::chat::composer::CoreAttachmentService::attachments().context("no attachment service")?;
        ensure!(service.key(&daily_gaming().wire(), attachment).is_some(), "no key registered with the attachment service");
        let server = daily_gaming().wire();
        let stored = cx.app().background_executor().spawn(async move { service.stored_blocking(&server, attachment) }).await.map_err(|e| anyhow::anyhow!("fetching the stored bytes: {e}"))?;
        ensure!(stored.len() as u64 == sealed_size, "the server holds {} B, expected {sealed_size} ({size} + 16 per chunk)", stored.len());
        ensure!(!marks.in_bytes(&stored), "the server holds the clip's plaintext");
        ensure!(stored[..64] != clip.bytes[..64], "the stored bytes start like the plaintext");
        cx.log(format_args!(
            "posted message {message}, attachment {attachment} in the encrypted #vault: plaintext {size} B, the server holds {} B of ciphertext ({chunks} chunks of 64 KiB, +16 B each), none of the clip's bytes in it",
            stored.len()
        ));
        drop(stored);

        let at = card(cx, list, attachment).await?;
        let playback = cx.update(|_, cx| crate::playback::Playback::global(cx))?.context("no playback")?;
        let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
        let stream = hub.stream(&format!("media:{}:{attachment}", daily_gaming().wire()));
        let totals = reader_totals();
        let clicked = Instant::now();
        cx.click(at)?;
        let s = stream.clone();
        wait_for(cx, Duration::from_secs(15), "the first picture of the sealed clip", move |_| s.stats().presented > 0).await?;
        let first = clicked.elapsed();
        let opened = reader_totals().since(totals);
        let p = playback.clone();
        wait_for(cx, Duration::from_secs(10), "a second and a half of it", move |cx| p.read(cx).video_times().0 >= 1_500_000).await?;
        let stats = stream.stats();
        let (_, duration) = cx.update(|_, cx| playback.read(cx).video_times())?;
        cx.log(format_args!(
            "through the chunk decryption: first picture {} ms after the click, {} pictures in the first 1.5 s, decoded with {}, {}x{}, duration {:.2} s",
            first.as_millis(),
            stats.published,
            stats.backend.map_or("no decoder", |b| b.label()),
            stats.width,
            stats.height,
            duration as f64 / 1e6
        ));
        ensure!(duration > 2_000_000, "duration {duration}");
        log_totals(cx, "open + first picture", opened, size, chunks);

        // Paused (a click on the picture, as a person would), so the position after the seek is
        // the seek's and not the clock's.
        cx.click(at)?;
        let p = playback.clone();
        wait_for(cx, Duration::from_secs(5), "the pause", move |cx| p.read(cx).video_phase() == Some(Phase::Paused)).await?;
        let played = reader_totals().since(totals);
        log_totals(cx, "… + 1.5 s of play", played, size, chunks);

        let target = if duration >= 50_000_000 { 45_000_000 } else { duration * 3 / 4 };
        let (n, sought, before_seek) = (stream.stats().published, Instant::now(), reader_totals());
        cx.update(|_, cx| playback.update(cx, |p, cx| p.seek_video_to(target, cx)))?;
        let s = stream.clone();
        wait_for(cx, Duration::from_secs(15), "the picture of the seek", move |_| s.stats().published > n).await?;
        let seek_ms = sought.elapsed().as_millis();
        cx.sleep(Duration::from_millis(300)).await;
        let position = cx.update(|_, cx| playback.read(cx).video_times().0)?;
        let seek = reader_totals().since(before_seek);
        cx.log(format_args!(
            "seek to {:.2} s while paused: picture after {seek_ms} ms, position {:.3} s ({:+.1} ms from the target), {} picture(s) published for it",
            target as f64 / 1e6,
            position as f64 / 1e6,
            (position - target) as f64 / 1e3,
            stream.stats().published - n
        ));
        ensure!((position - target).abs() <= 40_000, "the seek to {target} landed at {position}: more than a picture (40 ms) away");
        log_totals(cx, "the seek alone", seek, size, chunks);
        let all = reader_totals().since(totals);
        log_totals(cx, "open + first picture + 1.5 s + the seek", all, size, chunks);
        if size >= 4 << 20 {
            // A file much larger than the read-ahead: by range, never the whole of it.
            ensure!(all.fetched < size / 2, "{} of {} B fetched: the reader did not read by range", all.fetched, size);
            ensure!(all.chunks < chunks / 2, "{} of {chunks} chunks opened", all.chunks);
            ensure!(seek.ranges >= 1, "the seek made no range request");
        } else {
            // Smaller than the read-ahead (256 KiB): one request brings all of it, once.
            ensure!(all.fetched <= size, "{} B fetched of a {size} B file: something was fetched twice", all.fetched);
            ensure!(all.chunks <= chunks, "{} chunks opened of {chunks}", all.chunks);
        }

        nothing_on_disk(cx, "while it plays", &marks, began, true)?;

        // On from there.
        cx.click(at)?;
        let p = playback.clone();
        wait_for(cx, Duration::from_secs(10), "playback after the seek", move |cx| p.read(cx).video_times().0 >= target + 500_000).await?;
        cx.log(format_args!("playing on from {:.2} s", target as f64 / 1e6));

        // The same file as sound alone (what an audio attachment's player does with the reader).
        let audio = cx.update(|_, cx| model(cx).chat.audio.clone())?;
        cx.update(|_, cx| {
            let media = decibell_model::chat::viewers::ActiveMedia { attachment_id: attachment, server: daily_gaming(), channel: channel.clone(), filename: "clip".into(), width: 0, height: 0 };
            audio.update(cx, |a, cx| a.set_active(Some(media), cx));
        })?;
        let a = audio.clone();
        wait_for(cx, Duration::from_secs(15), "the sealed clip's sound", move |cx| a.read(cx).playing && a.read(cx).time_secs > 0.5).await?;
        cx.log("the same sealed attachment as sound alone: playing");
        stop_players(cx).await?;
        log_totals(cx, "everything this test played", reader_totals().since(totals), size, chunks);
        nothing_on_disk(cx, "after the players were dropped", &marks, began, false)?;
        Ok(())
    }

    // ── media-memory ────────────────────────────────────────────────────────────────────

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Mallinfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }

    #[cfg(target_env = "gnu")]
    unsafe extern "C" {
        fn mallinfo2() -> Mallinfo2;
        fn malloc_trim(pad: usize) -> i32;
        fn raise(signal: i32) -> i32;
    }

    /// glibc's account of its heap, all arenas: (in use, free but kept, mapped directly), MB.
    fn heap() -> (f64, f64, f64) {
        #[cfg(target_env = "gnu")]
        // SAFETY: mallinfo2 takes no arguments and returns its struct by value.
        let info = unsafe { mallinfo2() };
        #[cfg(not(target_env = "gnu"))]
        let info = Mallinfo2::default();
        let mb = |v: usize| v as f64 / 1048576.;
        (mb(info.uordblks), mb(info.fordblks), mb(info.hblkhd))
    }

    /// Gives glibc's free pages back to the system; how long that took.
    fn trim() -> Duration {
        let started = Instant::now();
        #[cfg(target_env = "gnu")]
        // SAFETY: malloc_trim only releases free memory of the allocator.
        unsafe {
            malloc_trim(0);
        }
        started.elapsed()
    }

    const KINDS: [&str; 8] = ["heap", "arenas", "anon", "nvidia-dev", "nvidia-lib", "memfd/shm", "binary", "files"];

    /// Private memory (`Private_Clean + Private_Dirty`) by kind of mapping, MB:
    /// `heap` = glibc's main arena (`[heap]`), `arenas` = its per-thread arenas (64 MiB-aligned
    /// anonymous reservations), `anon` = every other anonymous mapping (large allocations
    /// mapped directly, thread stacks, the GPU driver's own), `nvidia-dev` = `/dev/nvidia*`
    /// mappings, `nvidia-lib` = the driver's libraries, `memfd/shm` = shared-memory files,
    /// `binary` = our executable, `files` = every other file.
    fn by_mapping() -> BTreeMap<&'static str, f64> {
        const ARENA: u64 = 64 << 20;
        let text = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
        let exe = std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let mut out: BTreeMap<&'static str, f64> = KINDS.iter().map(|k| (*k, 0.)).collect();
        let (mut kind, mut arena_end) = ("anon", 0u64);
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let Some(first) = fields.next() else { continue };
            if let Some((start, _)) = first.split_once('-').filter(|_| !first.ends_with(':')) {
                let start = u64::from_str_radix(start, 16).unwrap_or(0);
                let perms = fields.next().unwrap_or("");
                let path = fields.nth(3).unwrap_or("");
                kind = if path == "[heap]" {
                    "heap"
                } else if path.is_empty() || path.starts_with("[anon") || path == "[stack]" {
                    if perms.starts_with("rw") && start % ARENA == 0 {
                        arena_end = start + ARENA;
                    }
                    if start < arena_end && start >= arena_end - ARENA { "arenas" } else { "anon" }
                } else if path.starts_with("/dev/nvidia") {
                    "nvidia-dev"
                } else if path.contains("nvidia") {
                    "nvidia-lib"
                } else if path.starts_with("/memfd:") || path.starts_with("/dev/shm") {
                    "memfd/shm"
                } else if path == exe {
                    "binary"
                } else {
                    "files"
                };
            } else if first == "Private_Clean:" || first == "Private_Dirty:" {
                let kb: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.);
                *out.entry(kind).or_default() += kb / 1024.;
            }
        }
        out
    }

    struct Sample {
        what: String,
        private: f64,
        maps: BTreeMap<&'static str, f64>,
        heap: (f64, f64, f64),
        threads: usize,
    }

    fn sample(what: impl Into<String>) -> Sample {
        // `DECIBELL_MEMORY_MARK=1`: SIGUSR2 at every sample, for an allocation tracer preloaded
        // into the run that dumps its live table on it (never set otherwise: the signal's
        // default action ends the process).
        #[cfg(target_env = "gnu")]
        if std::env::var_os("DECIBELL_MEMORY_MARK").is_some() {
            // SAFETY: raise only sends this process a signal.
            unsafe {
                raise(12);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let threads = std::fs::read_dir("/proc/self/task").map(|d| d.count()).unwrap_or(0);
        Sample { what: what.into(), private: private_mb(), maps: by_mapping(), heap: heap(), threads }
    }

    fn table(cx: &TestCx, samples: &[Sample]) {
        cx.log(format_args!(
            "{:<26} {:>8} | {:>7} {:>7} {:>7} {:>10} {:>10} {:>9} {:>7} {:>7} | {:>7} {:>7} {:>7} | {:>3}",
            "MB private", "total", KINDS[0], KINDS[1], KINDS[2], KINDS[3], KINDS[4], KINDS[5], KINDS[6], KINDS[7], "in use", "free", "mmapped", "thr"
        ));
        for s in samples {
            let m = |k: &str| s.maps.get(k).copied().unwrap_or(0.);
            cx.log(format_args!(
                "{:<26} {:>8.1} | {:>7.1} {:>7.1} {:>7.1} {:>10.1} {:>10.1} {:>9.1} {:>7.1} {:>7.1} | {:>7.1} {:>7.1} {:>7.1} | {:>3}",
                s.what,
                s.private,
                m(KINDS[0]),
                m(KINDS[1]),
                m(KINDS[2]),
                m(KINDS[3]),
                m(KINDS[4]),
                m(KINDS[5]),
                m(KINDS[6]),
                m(KINDS[7]),
                s.heap.0,
                s.heap.1,
                s.heap.2,
                s.threads
            ));
        }
    }

    pub(crate) async fn check_memory(cx: TestCx) -> Result<()> {
        ensure!(std::env::var("DECIBELL_MEDIA_OUTPUT").is_ok_and(|v| v == "null"), "media self-tests run with DECIBELL_MEDIA_OUTPUT=null");
        let (channel, list) = open_channel(&cx, "off-topic").await?;
        let (name, size, source) = {
            // Only the upload holds the clip; nothing of it stays in this process.
            let clip = Clip::load()?;
            let name = clip.name("memory");
            (name.clone(), clip.bytes.len(), clip.source(&name))
        };
        let (message, attachment) = post(&cx, &channel, &list, source, &name).await?;
        cx.log(format_args!("posted message {message} with attachment {attachment} ({:.1} MB) to #off-topic", size as f64 / 1048576.));
        let result = memory_cycles(&cx, &list, attachment).await;
        let _ = stop_players(&cx).await;
        delete(&cx, &channel, &list, message).await?;
        result
    }

    async fn memory_cycles(cx: &TestCx, list: &Entity<decibell_chat::MessageList>, attachment: i64) -> Result<()> {
        let playback = cx.update(|_, cx| crate::playback::Playback::global(cx))?.context("no playback")?;
        let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
        let stream = hub.stream(&format!("media:{}:{attachment}", daily_gaming().wire()));
        // `DECIBELL_MEMORY_TRIM=each`: give the allocator's free pages back after every drop
        // (what a `malloc_trim(0)` at the player's end would do), to see what that returns.
        let trim_each = std::env::var("DECIBELL_MEMORY_TRIM").is_ok_and(|v| v == "each");
        let cycles: usize = std::env::var("DECIBELL_MEMORY_CYCLES").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
        // The upload's own buffers and the list's settling are not the player's.
        cx.sleep(Duration::from_secs(3)).await;
        let mut samples = vec![sample("before")];
        let mut after = Vec::new();
        let totals = reader_totals();
        for cycle in 1..=cycles {
            let at = card(cx, list, attachment).await?;
            let (presented, clicked) = (stream.stats().presented, Instant::now());
            cx.click(at)?;
            let s = stream.clone();
            wait_for(cx, Duration::from_secs(15), "the first picture", move |_| s.stats().presented > presented).await?;
            let first = clicked.elapsed();
            let published = stream.stats().published;
            cx.sleep(Duration::from_secs(5)).await;
            let stats = stream.stats();
            let (position, _) = cx.update(|_, cx| playback.read(cx).video_times())?;
            samples.push(sample(format!("{cycle}: playing, 5 s in")));
            cx.log(format_args!(
                "cycle {cycle}: first picture {} ms after the click, {} pictures in 5 s ({} dropped in all), {} {}x{}, at {:.1} s, threads {:?}",
                first.as_millis(),
                stats.published - published,
                stats.dropped,
                stats.backend.map_or("no decoder", |b| b.label()),
                stats.width,
                stats.height,
                position as f64 / 1e6,
                media_threads()
            ));
            ensure!(stats.published - published >= 20, "only {} pictures in 5 s", stats.published - published);
            stop_players(cx).await?;
            cx.sleep(Duration::from_secs(2)).await;
            samples.push(sample(format!("{cycle}: 2 s after the drop")));
            if trim_each {
                let took = trim();
                samples.push(sample(format!("{cycle}: … malloc_trim {:.2} ms", took.as_secs_f64() * 1e3)));
            }
            after.push(samples.last().map_or(0., |s| s.private));
        }
        let read = reader_totals().since(totals);
        // What only the allocator was keeping comes back with a trim; what is in use does not.
        let took = trim();
        samples.push(sample(format!("malloc_trim(0): {:.2} ms", took.as_secs_f64() * 1e3)));
        table(cx, &samples);
        let before = samples[0].private;
        let trimmed = samples.last().map_or(0., |s| s.private);
        let steps: Vec<String> = after.windows(2).map(|w| format!("{:+.1}", w[1] - w[0])).collect();
        cx.log(format_args!(
            "private memory: {before:.1} MB before; after each drop {} MB (the first cycle {:+.1}, then {} MB per cycle); after malloc_trim(0) {trimmed:.1} MB ({:+.1} against before); {} range requests, {:.1} MB fetched",
            after.iter().map(|a| format!("{a:.1}")).collect::<Vec<_>>().join(" / "),
            after[0] - before,
            steps.join(" / "),
            trimmed - before,
            read.ranges,
            read.fetched as f64 / 1048576.
        ));
        for (i, w) in after.windows(2).enumerate() {
            if w[1] - w[0] >= 1.5 {
                bail!("cycle {} left {:+.1} MB more than the one before it: something grows with every play", i + 2, w[1] - w[0]);
            }
        }
        Ok(())
    }
}
