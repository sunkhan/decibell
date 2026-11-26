//! What this machine can decode, found out by decoding.
//!
//! The client advertises its decode capabilities to the server, and a codec it advertises is a
//! codec streamers will send it. "The driver lists the codec" is not that: a decoder can be
//! listed and fail to open, or open and fail on its first picture. So for each codec the
//! decoders of the order (`select.rs`) are tried with one small keyframe compiled into the
//! binary (`probe/keyframe.*`: 320 × 240, 2.5 to 3.6 KB each, made by `tools/streamgen` with the
//! streamer's encoder settings), and the first that returns the right picture is the answer. A
//! decoder that fails the probe is given up for the codec for the session, with the reason,
//! exactly as if a stream had failed on it: streams then never try it.
//!
//! The probe runs on a thread of its own, when the app asks ([`crate::VideoHub::probe_capabilities`]),
//! once per GPU and driver version: the result is kept in the process and, when the app names
//! a file, across runs. Cost on this machine (RTX 4080): docs/video-design.md §0.5.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

use crate::codec::{Codec, CodecSupport, DecodeBackend, SkippedBackend};
use crate::decode::{CaptureCell, Failure, FrameCx};
use crate::hub::{HubInner, StreamTarget};
use crate::packet::Packet;

const KEYFRAMES: [&[u8]; 3] = [
    include_bytes!("probe/keyframe.h264"),
    include_bytes!("probe/keyframe.hevc"),
    include_bytes!("probe/keyframe.av1"),
];
/// The same picture as a VP9 keyframe (libvpx, 156 bytes): VP9 is a codec only files use, so
/// it is not part of the start-up probe; see [`file_codec_works`].
const KEYFRAME_VP9: &[u8] = include_bytes!("probe/keyframe.vp9");
const KEYFRAME_SIZE: (u32, u32) = (320, 240);

/// The test frame of `codec`: frame 0 of a `tools/streamgen` stream. Empty: there is none.
pub(crate) fn keyframe(codec: Codec) -> &'static [u8] {
    match codec {
        Codec::Vp9 => KEYFRAME_VP9,
        Codec::Vp8 => &[],
        _ => KEYFRAMES[codec.index()],
    }
}

/// The probe of a codec only files use (VP9), run when a file first needs `backend` for it:
/// the same test as the start-up probe's (the test frame decoded, read back and compared), on
/// the decoder thread of the file that asks, once per decoder and codec in a session. A
/// decoder that fails it is given up for the codec with the reason, and the file goes on to
/// the next of the order (software at the end). `None`: the GPU device went away meanwhile.
pub(crate) fn file_codec_works(hub: &HubInner, backend: DecodeBackend, codec: Codec) -> Option<bool> {
    let slot = (codec.index(), backend.index());
    if hub.probe.files.lock()[slot.0][slot.1] {
        return Some(true);
    }
    if hub.selector.skipped(codec, backend).is_some() {
        return Some(false);
    }
    if keyframe(codec).is_empty() {
        return Some(true);
    }
    let started = Instant::now();
    match try_backend(hub, backend, codec) {
        Ok(()) => {
            log::info!(
                "video: {} decode with {} probed in {:.0} ms: works",
                codec.label(),
                backend.label(),
                started.elapsed().as_secs_f64() * 1000.0
            );
            hub.probe.files.lock()[slot.0][slot.1] = true;
            Some(true)
        }
        Err(None) => None,
        Err(Some(reason)) => {
            hub.selector.skip(codec, backend, &reason);
            Some(false)
        }
    }
}

/// Whether `row` is luma row 180 of the test frame: streamgen's gradient with its bars
/// (`32 + x / 2`, plus 30 where `(x / 64) & 1`), within what a low-bitrate keyframe loses.
pub(crate) fn is_the_test_picture(row: &[u8]) -> bool {
    row.len() == KEYFRAME_SIZE.0 as usize
        && [(16usize, 40i32), (96, 110), (176, 120), (304, 184)]
            .into_iter()
            .all(|(x, expected)| (row[x] as i32 - expected).abs() <= 20)
}

type Callback = Box<dyn FnOnce([CodecSupport; 3]) + Send>;

#[derive(Default)]
pub(crate) struct ProbeState {
    /// The last probe's answer and the device key (GPU + driver) it holds for.
    result: Mutex<Option<(String, [CodecSupport; 3])>>,
    /// `Some` while the probe thread runs: who waits for it.
    waiting: Mutex<Option<Vec<Callback>>>,
    /// `[codec][decoder]`: a file codec's test frame decoded right on that decoder this session.
    files: Mutex<[[bool; DecodeBackend::ALL.len()]; Codec::EVERY.len()]>,
    cache_file: Mutex<Option<PathBuf>>,
}

impl ProbeState {
    /// The answer no longer holds (a decoder failed that was not known to): the next request
    /// probes again, in this run and the next.
    pub fn invalidate(&self) {
        if self.result.lock().take().is_some()
            && let Some(path) = &*self.cache_file.lock()
        {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The order changed (a test, a setting): the kept answer was for another order. The
    /// file stays: it is the machine's own order's answer.
    pub fn forget(&self) {
        *self.result.lock() = None;
    }

    /// Nothing that was found out holds any more (the user asked for a new probe): the next
    /// request decodes again, whatever the file says.
    pub fn discard(&self) {
        *self.files.lock() = Default::default();
        *self.result.lock() = None;
        if let Some(path) = &*self.cache_file.lock() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The probe thread is decoding.
    pub fn is_running(&self) -> bool {
        self.waiting.lock().is_some()
    }

    pub fn set_cache_file(&self, path: PathBuf) {
        *self.cache_file.lock() = Some(path);
    }
}

/// The answer without decoding: the kept probe result for this device, else the first decoder
/// of the order that is listed for the codec.
pub(crate) fn known(hub: &HubInner) -> [CodecSupport; 3] {
    if let Some(key) = hub.backend.device_key()
        && let Some((probed_key, result)) = &*hub.probe.result.lock()
        && *probed_key == key
    {
        return *result;
    }
    listed(hub)
}

fn listed(hub: &HubInner) -> [CodecSupport; 3] {
    let order = hub.order();
    Codec::ALL.map(|codec| {
        order
            .as_slice()
            .iter()
            .filter(|backend| hub.selector.skipped(codec, **backend).is_none())
            .map(|backend| hub.backend.support(*backend, codec))
            .find(|support| support.path.is_some())
            .unwrap_or(CodecSupport::none(codec))
    })
}

pub(crate) fn start(hub: &Arc<HubInner>, done: Callback) {
    let Some(key) = hub.backend.device_key() else {
        // No GPU device yet: nothing to probe on, and nothing is kept.
        return done(listed(hub));
    };
    if let Some((probed_key, result)) = &*hub.probe.result.lock()
        && *probed_key == key
    {
        return done(*result);
    }
    {
        let mut waiting = hub.probe.waiting.lock();
        if let Some(waiting) = waiting.as_mut() {
            return waiting.push(done);
        }
        *waiting = Some(vec![done]);
    }
    let thread_hub = hub.clone();
    let spawned = std::thread::Builder::new()
        .name("decibell-video-probe".into())
        .spawn(move || {
            let hub = thread_hub;
            let result = run(&hub, &key);
            let waiting = hub.probe.waiting.lock().take().unwrap_or_default();
            for done in waiting {
                done(result);
            }
        });
    if let Err(error) = spawned {
        log::error!("video: starting the capability probe: {error}");
        let waiting = hub.probe.waiting.lock().take().unwrap_or_default();
        for done in waiting {
            done(listed(hub));
        }
    }
}

/// Probes every codec and keeps the answer for `key`.
fn run(hub: &HubInner, key: &str) -> [CodecSupport; 3] {
    // A forced order is somebody testing: its answers are not what the machine does by itself.
    let cacheable = !hub.order_is_forced();
    let cache_file = hub.probe.cache_file.lock().clone().filter(|_| cacheable);
    if let Some(path) = &cache_file
        && let Some((result, skipped)) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| parse_cache(&text, key))
    {
        log::info!(
            "video: decode capabilities from {}: {}",
            path.display(),
            describe(&result)
        );
        // What that probe found broken is not tried by streams of this run either, and
        // Settings → Codecs says why.
        for skip in skipped {
            hub.selector.skip(skip.codec, skip.backend, &skip.reason);
        }
        *hub.probe.result.lock() = Some((key.to_owned(), result));
        return result;
    }
    let started = Instant::now();
    let mut complete = true;
    let result = Codec::ALL.map(|codec| match probe_codec(hub, codec) {
        Some(support) => support,
        None => {
            complete = false;
            listed(hub)[codec.index()]
        }
    });
    log::info!(
        "video: decode capabilities probed in {:.0} ms: {}",
        started.elapsed().as_secs_f64() * 1000.0,
        describe(&result)
    );
    if complete {
        *hub.probe.result.lock() = Some((key.to_owned(), result));
        if let Some(path) = &cache_file
            && let Err(error) =
                std::fs::write(path, format_cache(key, &result, &hub.selector.report()))
        {
            log::warn!("video: writing {}: {error}", path.display());
        }
    }
    result
}

pub(crate) fn describe(result: &[CodecSupport; 3]) -> String {
    result
        .iter()
        .map(|support| {
            format!(
                "{} {}",
                support.codec.label(),
                support.backend.map_or("none", DecodeBackend::label)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first decoder of the order that decodes the test frame. `None`: the GPU device went
/// away meanwhile (nothing was learnt).
fn probe_codec(hub: &HubInner, codec: Codec) -> Option<CodecSupport> {
    for &backend in hub.order().as_slice() {
        if hub.selector.skipped(codec, backend).is_some() {
            continue;
        }
        match try_backend(hub, backend, codec) {
            Ok(()) => {
                let listed = hub.backend.support(backend, codec);
                return Some(CodecSupport {
                    codec,
                    path: Some(backend.path()),
                    backend: Some(backend),
                    probed: true,
                    max_size: listed.max_size,
                });
            }
            Err(None) => return None,
            Err(Some(reason)) => {
                hub.selector.skip(codec, backend, &reason);
            }
        }
    }
    Some(CodecSupport {
        probed: true,
        ..CodecSupport::none(codec)
    })
}

/// Decodes the test frame with `backend`. `Err(None)`: device lost.
fn try_backend(hub: &HubInner, backend: DecodeBackend, codec: Codec) -> Result<(), Option<String>> {
    let classify = |error: crate::decode::DecodeError| match error.failure {
        Failure::DeviceLost => None,
        Failure::Decoder => Some(error.message),
    };
    let target = StreamTarget::detached();
    let mut decoder = hub
        .backend
        .open(backend, codec, None, &target)
        .map_err(classify)?;
    let capture = CaptureCell::default();
    let row = capture.request(0.75);
    let bytes = keyframe(codec);
    let mut published = 0;
    // One packet in gives one picture out; a decoder that holds the first one back gets the
    // keyframe again (it is an IDR: valid any number of times).
    for serial in 1..=3 {
        let mut padded = Vec::with_capacity(bytes.len() + crate::packet::PADDING);
        padded.extend_from_slice(bytes);
        let cx = FrameCx {
            counters: &target.counters,
            serial,
            capture: &capture,
            timing: None,
        };
        published = decoder
            .decode(Packet::from(padded), true, serial as i64 * 33_333, &cx)
            .map_err(classify)?;
        if published > 0 {
            break;
        }
    }
    drop(decoder);
    target.clear();
    if published == 0 {
        return Err(Some("the decoder returned no picture for a keyframe".into()));
    }
    let row = row
        .try_recv()
        .map_err(|_| Some("the decoded picture could not be read back".to_owned()))?;
    if (row.width, row.height) != KEYFRAME_SIZE {
        return Err(Some(format!(
            "the test frame decoded to {}x{}",
            row.width, row.height
        )));
    }
    if !is_the_test_picture(&row.bytes) {
        return Err(Some("the test frame decoded to the wrong picture".into()));
    }
    Ok(())
}

/// The cache file: the device key on the first line, then `<codec index> <decoder or -> <max
/// width> <max height>` per codec, then `skip <codec index> <decoder> <reason>` for each
/// decoder the probe gave up (so the next run does not try it and can say why).
fn format_cache(key: &str, result: &[CodecSupport; 3], skipped: &[SkippedBackend]) -> String {
    let mut text = format!("{key}\n");
    for support in result {
        let (width, height) = support.max_size.unwrap_or((0, 0));
        text += &format!(
            "{} {} {width} {height}\n",
            support.codec.index(),
            support.backend.map_or("-", DecodeBackend::label)
        );
    }
    for skip in skipped {
        text += &format!(
            "skip {} {} {}\n",
            skip.codec.index(),
            skip.backend.label(),
            skip.reason.replace(['\n', '\r'], " ")
        );
    }
    text
}

fn parse_cache(text: &str, key: &str) -> Option<([CodecSupport; 3], Vec<SkippedBackend>)> {
    let mut lines = text.lines();
    if lines.next()? != key {
        return None;
    }
    let mut result = Codec::ALL.map(CodecSupport::none);
    for codec in Codec::ALL {
        let mut fields = lines.next()?.split(' ');
        if fields.next()?.parse::<usize>().ok()? != codec.index() {
            return None;
        }
        let backend = match fields.next()? {
            "-" => None,
            label => Some(DecodeBackend::from_label(label)?),
        };
        let width: u32 = fields.next()?.parse().ok()?;
        let height: u32 = fields.next()?.parse().ok()?;
        result[codec.index()] = CodecSupport {
            codec,
            path: backend.map(DecodeBackend::path),
            backend,
            probed: true,
            max_size: (width > 0).then_some((width, height)),
        };
    }
    let mut skipped = Vec::new();
    for line in lines {
        let mut fields = line.splitn(4, ' ');
        if fields.next()? != "skip" {
            return None;
        }
        skipped.push(SkippedBackend {
            codec: Codec::from_index(fields.next()?.parse().ok()?)?,
            backend: DecodeBackend::from_label(fields.next()?)?,
            reason: fields.next().unwrap_or("").into(),
        });
    }
    Some((result, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_round_trips_and_is_refused_for_another_device() {
        let result = [
            CodecSupport {
                codec: Codec::H264,
                path: Some(crate::DecodePath::Hardware),
                backend: Some(DecodeBackend::VulkanVideo),
                probed: true,
                max_size: Some((4096, 4096)),
            },
            CodecSupport {
                codec: Codec::Hevc,
                path: Some(crate::DecodePath::Hardware),
                backend: Some(DecodeBackend::Vaapi),
                probed: true,
                max_size: None,
            },
            CodecSupport {
                probed: true,
                ..CodecSupport::none(Codec::Av1)
            },
        ];
        let skipped = vec![
            SkippedBackend {
                codec: Codec::Hevc,
                backend: DecodeBackend::VulkanVideo,
                reason: "no video decode queue (VK_KHR_video_decode_h265)".into(),
            },
            SkippedBackend {
                codec: Codec::Av1,
                backend: DecodeBackend::Software,
                reason: "".into(),
            },
        ];
        let text = format_cache("10de:2704 driver 620.73", &result, &skipped);
        assert_eq!(
            parse_cache(&text, "10de:2704 driver 620.73"),
            Some((result, skipped))
        );
        assert_eq!(parse_cache(&text, "10de:2704 driver 621.0"), None);
        // A file from before the skips were kept still reads.
        let old = format_cache("10de:2704 driver 620.73", &result, &[]);
        assert_eq!(
            parse_cache(&old, "10de:2704 driver 620.73"),
            Some((result, Vec::new()))
        );
        assert_eq!(parse_cache("10de:2704 driver 620.73\n0 vdpau 0 0\n", "10de:2704 driver 620.73"), None);
    }

    /// The software decoders decode the embedded test frames to streamgen's picture: what
    /// the probe compares every other decoder's output with.
    #[test]
    fn every_test_frame_decodes_in_software_to_the_test_picture() {
        let hub = crate::VideoHub::headless();
        for codec in Codec::ALL {
            assert_eq!(
                try_backend(&hub.inner, DecodeBackend::Software, codec),
                Ok(()),
                "{codec:?}"
            );
        }
        // And the probe as a whole, on the backend without a GPU.
        let (sender, receiver) = std::sync::mpsc::channel();
        hub.probe_capabilities(move |result| sender.send(result).unwrap());
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        for support in result {
            assert_eq!(support.backend, Some(DecodeBackend::Software));
            assert!(support.probed);
        }
        assert_eq!(hub.capabilities(), result);
    }

    #[test]
    fn a_wrong_picture_is_not_the_test_picture() {
        assert!(!is_the_test_picture(&[0u8; 320]));
        assert!(!is_the_test_picture(&[128u8; 320]));
        let row: Vec<u8> = (0..320)
            .map(|x| (32 + x / 2 + ((x / 64) & 1) * 30) as u8)
            .collect();
        assert!(is_the_test_picture(&row));
        assert!(!is_the_test_picture(&row[..300]));
    }
}
