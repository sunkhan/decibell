//! The engine end to end, without a window or a sound card: files in, pictures to the hub's
//! counting sink, sound to a sink that keeps every sample it is handed.

mod common;

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::*;
use decibell_media::{Phase, TestOutput};
use decibell_video::{Codec, VideoHub};
use parking_lot::Mutex;

const LIMIT: Duration = Duration::from_secs(30);

/// The steepest step of the renderer's 10 ms raised cosine, per sample at full scale.
const RAMP_STEP: f32 = std::f32::consts::FRAC_PI_2 / 480.0 * 1.02;

fn collect_presented(player: &decibell_media::Player) -> Arc<Mutex<Vec<(i64, Instant)>>> {
    let presented = Arc::new(Mutex::new(Vec::new()));
    player.on_presented({
        let presented = presented.clone();
        move |pts, at| presented.lock().push((pts, at))
    });
    presented
}

#[test]
fn every_file_of_the_matrix_plays_to_its_end() {
    // (file, video codec, video frames, has sound)
    let matrix: &[(&str, Option<Codec>, bool)] = &[
        ("h264_aac.mp4", Some(Codec::H264), true),
        ("h264_aac_tail.mp4", Some(Codec::H264), true),
        ("hevc_aac.mp4", Some(Codec::Hevc), true),
        ("av1_opus.mp4", Some(Codec::Av1), true),
        ("h264_pcm.mov", Some(Codec::H264), true),
        ("vp9_opus.webm", Some(Codec::Vp9), true),
        ("vp8_vorbis.webm", Some(Codec::Vp8), true),
        ("av1_opus.webm", Some(Codec::Av1), true),
        ("h264_aac.mkv", Some(Codec::H264), true),
        ("h264_only.mp4", Some(Codec::H264), false),
        // 10-bit 4:2:0 and the HDR signallings (320 x 240): decoded here in software, their 8
        // most significant bits counted by the headless sink.
        ("vp9_hw.webm", Some(Codec::Vp9), false),
        ("hevc10.mp4", Some(Codec::Hevc), false),
        ("av1_10.mp4", Some(Codec::Av1), false),
        ("vp9_p2.webm", Some(Codec::Vp9), false),
        ("hevc10_pq.mp4", Some(Codec::Hevc), false),
        ("hevc10_hlg.mp4", Some(Codec::Hevc), false),
        ("cbr.mp3", None, true),
        ("vbr_noxing.mp3", None, true),
        ("vorbis.ogg", None, true),
        ("opus.ogg", None, true),
        ("opus.opus", None, true),
        ("flac.flac", None, true),
        ("s16.wav", None, true),
        ("f32.wav", None, true),
        ("s24.wav", None, true),
        ("aac.m4a", None, true),
        ("adts.aac", None, true),
    ];
    let threads: Vec<_> = matrix
        .iter()
        .map(|&(name, codec, sound)| {
            std::thread::spawn(move || {
                let hub = VideoHub::headless();
                let (output, recording) = TestOutput::recording(RATE, 8);
                let player = open_bytes(asset(name), codec.map(|_| &hub), name, output, false);
                let presented = collect_presented(&player);
                wait_phase(&player, Phase::Paused, LIMIT);
                let status = player.status();
                assert_eq!(status.video.map(|video| video.codec), codec, "{name}");
                assert_eq!(status.has_audio, sound, "{name}");
                if let Some(video) = status.video {
                    let size = if name.contains("10") || name.contains("p2") || name.contains("hw") { (320, 240) } else { (128, 72) };
                    assert_eq!((video.width, video.height), size, "{name}");
                }
                assert!((1_600_000..=2_200_000).contains(&status.duration), "{name}: duration {}", status.duration);
                player.play();
                wait_phase(&player, Phase::Ended, LIMIT);
                assert_eq!(player.position(), status.duration, "{name}: at the end");
                if codec.is_some() {
                    let presented = presented.lock();
                    let times: Vec<i64> = presented.iter().map(|&(pts, _)| pts).collect();
                    assert!(times.windows(2).all(|pair| pair[1] > pair[0]), "{name}: pictures out of order: {times:?}");
                    let stats = player.stats();
                    assert_eq!(times.len() as u64 + stats.late, 50, "{name}: {} shown, {} late of 50 ({stats:?})", times.len(), stats.late);
                    assert_eq!(times[0], 0, "{name}: the first picture is the file's first");
                    assert!((times[times.len() - 1] - 1_960_000).abs() < 2_000, "{name}: the last picture is the file's last: {times:?}");
                }
                if sound {
                    let recording = recording.lock();
                    let found = clicks(&recording.samples, 0.08);
                    assert_eq!(found.len(), 3, "{name}: clicks at {found:?}");
                    for pair in found.windows(2) {
                        let gap = pair[1] - pair[0];
                        assert!(gap.abs_diff(RATE as usize / 2) <= 96, "{name}: {gap} samples between clicks");
                    }
                    assert_eq!(*recording.samples.last().unwrap(), 0.0, "{name}: ends in silence");
                }
            })
        })
        .collect();
    let mut failed = Vec::new();
    for (thread, entry) in threads.into_iter().zip(matrix) {
        if let Err(panic) = thread.join() {
            failed.push(format!("{}: {:?}", entry.0, panic.downcast_ref::<String>()));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Sixty seconds of pictures stamped with their time and a click track: how far the moment a
/// picture is handed to the screen is from the moment its sound is heard.
#[test]
fn pictures_stay_within_twenty_milliseconds_of_the_sound_for_a_minute() {
    let hub = VideoHub::headless();
    let (output, recording) = TestOutput::recording(RATE, 70);
    let player = open_bytes(asset("sync60.mp4"), Some(&hub), "sync", output, true);
    let presented = collect_presented(&player);
    wait_phase(&player, Phase::Ended, Duration::from_secs(90));
    let recording = recording.lock();
    let found = clicks(&recording.samples, 0.08);
    assert_eq!(found.len(), 119, "a click every half second");
    // When the sound of media time `t` was heard: from the nearest click (clicks are at
    // 0.5 s, 1.0 s, …), sample-exact in between.
    let heard_at = |pts: i64| -> Instant {
        let click = ((pts as f64 / 500_000.0).round() as usize).clamp(1, found.len());
        let click_pts = click as i64 * 500_000;
        let at = recording.played_at(found[click - 1], RATE).unwrap();
        if pts >= click_pts {
            at + Duration::from_micros((pts - click_pts) as u64)
        } else {
            at - Duration::from_micros((click_pts - pts) as u64)
        }
    };
    let presented = presented.lock();
    let stats = player.stats();
    assert!(presented.len() as u64 + stats.late == 1500, "{} pictures, {stats:?}", presented.len());
    let mut worst = 0i64;
    let mut sum = 0i64;
    let mut signed = 0i64;
    // The first picture is the position's own (shown at once, before the sound starts).
    for &(pts, at) in presented.iter().skip(1) {
        let heard = heard_at(pts);
        let offset = if at >= heard {
            at.duration_since(heard).as_micros() as i64
        } else {
            -(heard.duration_since(at).as_micros() as i64)
        };
        worst = worst.max(offset.abs());
        sum += offset.abs();
        signed += offset;
    }
    let count = presented.len() as i64 - 1;
    println!(
        "A/V sync over 60 s: {} pictures shown, {} dropped late; offset max {:.2} ms, mean |offset| {:.2} ms, mean {:+.2} ms (picture after sound is positive)",
        presented.len(),
        stats.late,
        worst as f64 / 1000.0,
        sum as f64 / count as f64 / 1000.0,
        signed as f64 / count as f64 / 1000.0
    );
    assert!(worst <= 20_000, "a picture was {worst} µs from its sound");
    assert!(stats.late <= 15, "{} pictures dropped", stats.late);
}

#[test]
fn a_seek_lands_on_the_picture_and_the_sample_asked_for() {
    // The picture: across a ten-second group of pictures, forwards and backwards.
    let hub = VideoHub::headless();
    let (output, _recording) = TestOutput::recording(RATE, 30);
    let player = open_bytes(asset("sync60.mp4"), Some(&hub), "seek", output, false);
    let presented = collect_presented(&player);
    wait_phase(&player, Phase::Paused, LIMIT);
    let mut worst = 0;
    for target in [7_300_000i64, 42_040_000, 41_990_000, 0, 59_900_000, 19_999_000, 20_000_000, 33_333_333] {
        let before = presented.lock().len();
        let started = Instant::now();
        player.seek(target);
        wait_for("the picture of the seek", LIMIT, || presented.lock().len() > before);
        let (pts, _) = *presented.lock().last().unwrap();
        // Pictures are 40 ms apart: the one shown is the one covering the target.
        let error = (pts - target).abs();
        assert!(error <= 20_000, "seek to {target}: picture {pts}");
        worst = worst.max(error);
        assert_eq!(player.position(), target, "the position is the target while paused");
        println!("seek to {:>9.3} s: picture at {:>9.3} s after {:.0} ms", target as f64 / 1e6, pts as f64 / 1e6, started.elapsed().as_secs_f64() * 1e3);
    }
    println!("seek accuracy, picture: worst {:.1} ms (a picture lasts 40 ms)", worst as f64 / 1000.0);
    drop(player);

    // The sample: a WAV whose every sample says where it is in the file.
    let samples: Vec<i16> = (0..RATE as usize * 20).map(|n| 8000 + (n % 2000) as i16 * 8).collect();
    let (output, recording) = TestOutput::recording(RATE, 30);
    let player = open_bytes(wav(&samples), None, "seek-audio", output, false);
    wait_phase(&player, Phase::Paused, LIMIT);
    for target_sample in [123_457usize, 48_000 * 15 + 1, 7] {
        let target = (target_sample as i64 * 1_000_000 + RATE as i64 - 1) / RATE as i64;
        let first = (target as usize * RATE as usize) / 1_000_000;
        player.seek(target);
        let from = recording.lock().samples.len();
        player.play();
        wait_for("sound after the seek", LIMIT, || recording.lock().samples.len() > from + RATE as usize / 2);
        player.pause();
        // The pause's fade-out must not be taken for the next round's first sound.
        std::thread::sleep(Duration::from_millis(150));
        let recording = recording.lock();
        let heard = &recording.samples[from..];
        let start = heard.iter().position(|&x| x != 0.0).expect("sound");
        // 480 samples later the envelope is at full level: that sample is the file's
        // `first + 480`th.
        let value = (heard[start + 480] * 32768.0).round() as i32;
        let expected = samples[first + 480] as i32;
        assert!(
            (value - expected).abs() <= 1,
            "seek to sample {target_sample}: the sample 480 after the first heard is {value}, the file's {} is {expected}",
            first + 480
        );
    }
    println!("seek accuracy, sound: the first sample heard is the target's sample (0 samples off, three targets)");
}

/// Every transition a user can cause, rendered from a full-scale constant: the only changes in
/// what the device is handed are the envelope's.
#[test]
fn no_transition_hands_the_device_a_step() {
    let samples = vec![16_384i16; RATE as usize * 6];
    let (output, recording) = TestOutput::recording(RATE, 30);
    let player = open_bytes(wav(&samples), None, "pops", output.clone(), true);
    let nap = |ms| std::thread::sleep(Duration::from_millis(ms));
    wait_phase(&player, Phase::Playing, LIMIT);
    nap(300); // start
    player.pause();
    nap(120); // pause, resumed before the device closes
    player.play();
    nap(200);
    player.pause();
    nap(500); // pause until the device has closed
    assert_eq!(output.live.load(Ordering::Relaxed), 0, "the device stream is closed 250 ms after a pause");
    player.play(); // resume on a new stream
    nap(200);
    player.seek(3_000_000); // seek while playing
    nap(200);
    player.set_volume(0.3, false);
    nap(60);
    player.set_volume(0.3, true); // mute
    nap(60);
    player.set_volume(1.0, false);
    nap(100);
    player.pause();
    nap(100);
    player.seek(1_000_000); // seek while paused
    nap(400);
    player.play();
    nap(200);
    player.seek(5_600_000);
    wait_phase(&player, Phase::Ended, LIMIT); // the end of the file
    nap(400);
    drop(player); // close
    wait_for("the device to close", LIMIT, || output.live.load(Ordering::Relaxed) == 0);
    let recording = recording.lock();
    let mut worst = 0.0f32;
    let mut bounds = recording.streams.clone();
    bounds.push(recording.samples.len());
    assert!(bounds.len() >= 4, "the device was opened {} times", bounds.len() - 1);
    for stream in bounds.windows(2) {
        let heard = &recording.samples[stream[0]..stream[1]];
        assert!(heard[0].abs() <= RAMP_STEP * 0.5, "a stream starts from silence, not {}", heard[0]);
        assert_eq!(*heard.last().unwrap(), 0.0, "a stream is closed in silence");
        for pair in heard.windows(2) {
            worst = worst.max((pair[1] - pair[0]).abs());
        }
    }
    let level = 0.5;
    println!(
        "pop test (start, pause, resume, resume on a new stream, seek playing, volume, mute, seek paused, end, close): {} streams, {} samples, largest step {:.5} of full scale (the 10 ms ramp's steepest: {:.5})",
        bounds.len() - 1,
        recording.samples.len(),
        worst,
        RAMP_STEP * level
    );
    assert!(worst <= RAMP_STEP * level, "a step of {worst}");
    assert!(recording.samples.iter().any(|&x| (x - level).abs() < 1e-4), "it did play at full level");
}

/// A reader that stops delivering for a while in the middle of the file.
struct Stalling {
    inner: Cursor<Vec<u8>>,
    stall_at: u64,
    stalled: bool,
}

impl Read for Stalling {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if !self.stalled && self.inner.position() >= self.stall_at {
            self.stalled = true;
            std::thread::sleep(Duration::from_millis(4200));
        }
        self.inner.read(buffer)
    }
}

impl Seek for Stalling {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

#[test]
fn an_underrun_and_an_unplugged_device_are_silent_too() {
    // The network stops for 4.2 s at the file's third second (about 2.5 s of sound are buffered
    // ahead of the reader by then): the ring runs dry.
    let samples = vec![16_384i16; RATE as usize * 6];
    let (output, recording) = TestOutput::recording(RATE, 30);
    let source = Stalling {
        inner: Cursor::new(wav(&samples)),
        stall_at: 44 + RATE as u64 * 2 * 3,
        stalled: false,
    };
    let player = open(source, None, "underrun", output.clone(), true);
    wait_phase(&player, Phase::Ended, LIMIT);
    drop(player);
    wait_for("the device to close", LIMIT, || output.live.load(Ordering::Relaxed) == 0);
    {
        let recording = recording.lock();
        let heard = &recording.samples;
        let worst = heard.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max);
        // Silence in the middle: after the first sound and before the last.
        let first = heard.iter().position(|&x| x > 0.4).unwrap();
        let last = heard.iter().rposition(|&x| x > 0.4).unwrap();
        let gap = heard[first..last].iter().filter(|&&x| x == 0.0).count();
        println!("underrun: {:.0} ms of silence in the middle, largest step {worst:.5}", gap as f64 * 1000.0 / RATE as f64);
        assert!(gap > RATE as usize / 2, "the ring did not run dry ({gap} silent samples)");
        assert!(worst <= RAMP_STEP * 0.5, "a step of {worst}");
        let played = heard.iter().filter(|&&x| x > 0.0).count();
        assert!(played.abs_diff(samples.len()) < 2000, "every sample was played once: {played} of {}", samples.len());
    }

    // The device is unplugged while playing: the old stream is cut off by the device, the new
    // one starts from silence.
    let (output, recording) = TestOutput::recording(RATE, 30);
    let player = open_bytes(wav(&samples), None, "unplug", output.clone(), true);
    wait_for("sound", LIMIT, || recording.lock().samples.iter().any(|&x| x > 0.4));
    output.unplug();
    wait_for("the next stream", LIMIT, || output.opened.load(Ordering::Relaxed) == 2);
    wait_phase(&player, Phase::Ended, LIMIT);
    drop(player);
    wait_for("the device to close", LIMIT, || output.live.load(Ordering::Relaxed) == 0);
    let recording = recording.lock();
    let second = &recording.samples[recording.streams[1]..];
    let worst = second.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max);
    println!("device change: the new stream starts at {} and its largest step is {worst:.5}", second[0]);
    assert!(second[0].abs() <= RAMP_STEP * 0.5, "the new stream starts from silence, not {}", second[0]);
    assert!(worst <= RAMP_STEP * 0.5, "a step of {worst}");
    assert!(second.iter().any(|&x| (x - 0.5).abs() < 1e-4));
}

/// A sealed attachment as the player meets it: the reader holds ciphertext and opens 64 KiB
/// chunks in memory as ranges are asked for.
struct Sealed {
    ciphertext: Vec<u8>,
    position: u64,
    opened: Arc<AtomicU64>,
    largest_read: Arc<AtomicU64>,
}

const CHUNK: usize = 64 * 1024;

fn keystream(chunk: usize, offset: usize) -> u8 {
    ((chunk * 131 + offset * 7 + 0x5a) % 251) as u8
}

impl Read for Sealed {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let position = self.position as usize;
        if position >= self.ciphertext.len() {
            return Ok(0);
        }
        // One chunk at a time, as the real reader's pieces arrive.
        let chunk = position / CHUNK;
        let end = ((chunk + 1) * CHUNK).min(self.ciphertext.len());
        let count = buffer.len().min(end - position);
        for (index, byte) in buffer[..count].iter_mut().enumerate() {
            *byte = self.ciphertext[position + index] ^ keystream(chunk, (position + index) % CHUNK);
        }
        self.opened.fetch_add(count as u64, Ordering::Relaxed);
        self.largest_read.fetch_max(count as u64, Ordering::Relaxed);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for Sealed {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let size = self.ciphertext.len() as i64;
        let target = match to {
            SeekFrom::Start(position) => position as i64,
            SeekFrom::Current(delta) => self.position as i64 + delta,
            SeekFrom::End(delta) => size + delta,
        };
        self.position = target.clamp(0, size) as u64;
        Ok(self.position)
    }
}

#[test]
fn a_sealed_file_plays_from_a_reader_that_decrypts_in_memory() {
    for name in ["sync60.mp4", "h264_aac_tail.mp4"] {
        let plain = asset(name);
        let ciphertext: Vec<u8> = plain
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ keystream(index / CHUNK, index % CHUNK))
            .collect();
        assert_ne!(&ciphertext[..64], &plain[..64]);
        let (opened, largest_read) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let source = Sealed {
            ciphertext,
            position: 0,
            opened: opened.clone(),
            largest_read: largest_read.clone(),
        };
        let hub = VideoHub::headless();
        let (output, recording) = TestOutput::recording(RATE, 12);
        let player = open(source, Some(&hub), name, output, true);
        let presented = collect_presented(&player);
        wait_for("two seconds of it", LIMIT, || player.position() >= 1_900_000 || player.status().phase == Phase::Ended);
        assert!(presented.lock().len() >= 45, "{name}: {} pictures", presented.lock().len());
        assert!(clicks(&recording.lock().samples, 0.08).len() >= 3, "{name}: its clicks are heard");
        // It read by range and no further ahead than its buffers: not the whole file.
        let read = opened.load(Ordering::Relaxed);
        assert!(largest_read.load(Ordering::Relaxed) <= decibell_media::source::IO_BUFFER as u64);
        if name == "sync60.mp4" {
            assert!(read < plain.len() as u64 / 2, "{name}: {read} of {} bytes read for 2 of 60 s", plain.len());
        }
    }
}

#[test]
fn files_that_cannot_be_played_say_why() {
    let (output, _) = TestOutput::recording(RATE, 1);
    let hub = VideoHub::headless();
    let player = open_bytes(b"not a media file at all, just some text".repeat(100), Some(&hub), "junk", output.clone(), true);
    wait_for("the failure", LIMIT, || matches!(player.status().phase, Phase::Failed(_)));
    let Phase::Failed(reason) = player.status().phase else { unreachable!() };
    assert!(reason.starts_with("This file can't be played"), "{reason}");
    // An audio card given a video file plays its sound; a video card given a sound file fails.
    let player = open_bytes(asset("h264_aac.mp4"), None, "sound-of-video", output.clone(), true);
    wait_phase(&player, Phase::Ended, LIMIT);
    assert!(player.status().video.is_none());
    let player = open_bytes(asset("h264_only.mp4"), None, "no-sound", output, true);
    wait_for("the failure", LIMIT, || matches!(player.status().phase, Phase::Failed(_)));
}

#[test]
fn the_header_alone_says_what_a_file_is() {
    let probe = decibell_media::probe(Box::new(Cursor::new(asset("hevc_aac.mp4")))).unwrap();
    assert_eq!(probe.video, Some((Codec::Hevc, 128, 72)));
    assert_eq!(probe.audio.as_deref(), Some("aac"));
    assert_eq!(probe.duration, 2_000_000);
    // An MP3 without a Xing header and raw ADTS have no duration in them: an estimate from
    // the bitrate, finite (the renderer guards against Chromium's `Infinity` here).
    for name in ["vbr_noxing.mp3", "adts.aac"] {
        let probe = decibell_media::probe(Box::new(Cursor::new(asset(name)))).unwrap();
        assert!(probe.video.is_none() && probe.duration > 1_000_000 && probe.duration < 3_000_000, "{name}: {probe:?}");
    }
}

#[test]
fn a_loop_and_play_at_the_end_start_again() {
    let hub = VideoHub::headless();
    let (output, _) = TestOutput::recording(RATE, 20);
    let player = open_bytes(asset("h264_aac.mp4"), Some(&hub), "loop", output, true);
    player.set_looping(true);
    let presented = collect_presented(&player);
    wait_for("the second time round", LIMIT, || presented.lock().iter().filter(|&&(pts, _)| pts == 0).count() >= 2);
    player.set_looping(false);
    wait_phase(&player, Phase::Ended, LIMIT);
    let before = presented.lock().len();
    player.play();
    wait_for("the whole file again", LIMIT, || presented.lock().len() >= before + 45);
    wait_phase(&player, Phase::Ended, LIMIT);
}

#[test]
fn a_paused_video_holds_its_picture_and_resumes_on_the_next_one() {
    let hub = VideoHub::headless();
    let (output, _) = TestOutput::recording(RATE, 30);
    let player = open_bytes(asset("sync60.mp4"), Some(&hub), "pause", output.clone(), true);
    let presented = collect_presented(&player);
    wait_for("a second of it", LIMIT, || player.position() >= 1_000_000);
    player.pause();
    std::thread::sleep(Duration::from_millis(100));
    let (count, last) = {
        let presented = presented.lock();
        (presented.len(), presented.last().unwrap().0)
    };
    // Long enough for the device to close and for the hub's three-second idle rule to matter.
    std::thread::sleep(Duration::from_millis(4000));
    assert_eq!(presented.lock().len(), count, "pictures while paused");
    assert_eq!(output.live.load(Ordering::Relaxed), 0, "the device is closed while paused");
    let position = player.position();
    player.play();
    wait_for("pictures after the resume", LIMIT, || presented.lock().len() >= count + 25);
    let presented = presented.lock();
    assert_eq!(presented[count].0, last + 40_000, "the picture after the pause is the next one");
    assert!(presented[count..].windows(2).all(|pair| pair[1].0 == pair[0].0 + 40_000), "none skipped or repeated");
    assert!((position - last).abs() <= 60_000, "paused at {position}, the last picture was {last}");
    assert_eq!(player.stats().seeks, 0, "a resume needs no seek");
}

#[test]
fn a_file_with_a_display_matrix_is_drawn_turned() {
    use decibell_media::Orientation;
    // Stored 128 x 72; a phone's "held upright" matrix turns it a quarter clockwise.
    for (name, orientation, size) in [
        ("rot90.mp4", Orientation::ROTATE_90, (72, 128)),
        ("rot180.mp4", Orientation::ROTATE_180, (128, 72)),
        ("rot270.mkv", Orientation::ROTATE_270, (72, 128)),
        ("h264_only.mp4", Orientation::IDENTITY, (128, 72)),
    ] {
        let probe = decibell_media::probe(Box::new(Cursor::new(asset(name)))).unwrap();
        assert_eq!(probe.orientation, orientation, "{name}");
        assert_eq!(probe.video, Some((Codec::H264, size.0, size.1)), "{name}");
        let hub = VideoHub::headless();
        let (output, _) = TestOutput::recording(RATE, 20);
        let player = open_bytes(asset(name), Some(&hub), name, output, true);
        wait_for("the first picture", LIMIT, || player.status().has_picture);
        let video = player.status().video.unwrap();
        // The decoder reports the stored size; the status stays the drawn one.
        assert_eq!((video.width, video.height, video.orientation), (size.0, size.1, orientation), "{name}");
        assert_eq!(hub.stream(name).orientation(), orientation, "{name}: the surface is told");
        wait_phase(&player, Phase::Ended, LIMIT);
        let video = player.status().video.unwrap();
        assert_eq!((video.width, video.height), size, "{name} at the end");
    }
}

/// The output device is changed while a file plays (`Player::set_output`): the old device is
/// handed a fade to zero and then nothing, the new one starts at zero, and the sound comes back
/// on the sample that is due then: the file went on by the wall clock in between.
#[test]
fn a_change_of_output_device_moves_the_sound_without_a_step_or_a_delay() {
    // Every sample says where it is in the file (16 samples a step: smooth, so that every
    // jump in what the devices are handed is the envelope's).
    let samples: Vec<i16> = (0..RATE as usize * 8).map(|n| (n / 16) as i16).collect();
    let (first, first_recording) = TestOutput::recording(RATE, 10);
    let (second, second_recording) = TestOutput::recording(RATE, 10);
    let player = open_bytes(wav(&samples), None, "move", first.clone(), true);
    wait_for("sound on the first device", LIMIT, || first_recording.lock().samples.len() > RATE as usize * 6 / 10);
    let before = player.position();
    let moved = Instant::now();
    player.set_output(second.clone());
    wait_for("sound on the second device", LIMIT, || second_recording.lock().samples.iter().filter(|&&x| x != 0.0).count() > RATE as usize / 2);
    let after = player.position();
    let elapsed = moved.elapsed().as_micros() as i64;
    player.pause();
    std::thread::sleep(Duration::from_millis(150));

    let (a, b) = (first_recording.lock(), second_recording.lock());
    assert_eq!(first.live.load(Ordering::Relaxed), 0, "the first device's stream is closed");
    assert_eq!((a.streams.len(), b.streams.len()), (1, 1), "one stream on each device");
    // The largest level the file reaches here is 24000 / 32768; the ramp's steepest step at it.
    let allowed = std::f32::consts::FRAC_PI_2 / 480.0 * (24_000.0 / 32_768.0) * 1.02 + 2.0 / 32_768.0;
    for (name, recording) in [("first", &a), ("second", &b)] {
        let step = recording.samples.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max);
        assert!(step <= allowed, "the {name} device was handed a jump of {step} (allowed {allowed})");
    }
    assert_eq!(*a.samples.last().unwrap(), 0.0, "the first device ends in silence");
    assert!(b.samples[0].abs() < 1e-4, "the second device starts from silence ({})", b.samples[0]);
    // A sample well before the move and one past the second device's ramp: the file's frames
    // they are, and when each was played. The difference is the same on both clocks.
    let frame = |value: f32| (value * 32_768.0).round() as i64 * 16;
    let early = RATE as usize * 3 / 10;
    let late = b.samples.iter().position(|&x| x != 0.0).expect("sound on the second device") + 960;
    let (early_frame, late_frame) = (frame(a.samples[early]), frame(b.samples[late]));
    let wall = b.played_at(late, RATE).unwrap().duration_since(a.played_at(early, RATE).unwrap()).as_micros() as i64;
    let file = (late_frame - early_frame) * 1_000_000 / RATE as i64;
    println!(
        "device change: {:.1} ms without sound on either device; the file advanced {:.1} ms in {:.1} ms of wall clock between a sample on the old device and one on the new ({:+.2} ms); position before {:.3} s, {:.0} ms later {:.3} s",
        b.played_at(b.samples.iter().position(|&x| x != 0.0).unwrap(), RATE).unwrap().duration_since(a.played_at(a.samples.iter().rposition(|&x| x != 0.0).unwrap(), RATE).unwrap()).as_secs_f64() * 1e3,
        file as f64 / 1e3,
        wall as f64 / 1e3,
        (file - wall) as f64 / 1e3,
        before as f64 / 1e6,
        elapsed as f64 / 1e3,
        after as f64 / 1e6,
    );
    // 16 samples a step is a third of a millisecond; the sinks' 10 ms blocks are stamped exactly.
    assert!((file - wall).abs() <= 2_000, "the sound is {} us off the wall clock after the move", file - wall);
    // The position the UI shows went on through the move.
    assert!(((after - before) - elapsed).abs() <= 40_000, "the position advanced {} us in {elapsed} us", after - before);
}
