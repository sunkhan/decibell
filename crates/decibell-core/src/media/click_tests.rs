//! Where the voice path could click (the audit in gpui-client/HANDOFF.md §5):
//! each case is built on the engine's own receive, mix and capture code, with
//! a tone as the signal, and the rendered samples are checked for a
//! discontinuity: no step between neighbouring samples larger than the
//! tone's own slope allows.
//!
//! The receive side runs as in the pipeline: a peer's Opus packets through
//! `on_voice_datagram` into its jitter buffer, `drain_peer_voice` into its
//! ring, and a device (a thread that renders the real `OutputMix` when told
//! to, 10 ms at a time) pulling 20 ms per step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use ringbuf::{traits::Split, HeapCons, HeapProd, HeapRb};

use super::audio_device::{OutputCtl, OutputHandle, OutputMix, OutputPullStats, PeerList};
use super::codec::{OpusEncoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE};
use super::cue::CueBus;
use super::packet::UdpAudioPacket;
use super::peer::PeerAudio;
use super::pipeline::{
    drain_peer_voice, on_voice_datagram, peer_key, reap_retired, retire_output, PeerKey, Retiring, VoiceDrain, VoiceRx,
};

pub(crate) const TONE_HZ: f64 = 190.0;
pub(crate) const TONE_AMP: f64 = 7_000.0;
const PEER: &str = "101";

/// The talker: a 190 Hz tone whose level swings at syllable rate (3 Hz,
/// between 10 % and full). A steady tone won't do: after 800 ms the encoder's
/// voice detector calls it background noise and stops coding it (DTX).
pub(crate) fn talker(t: f64) -> i16 {
    let level = 0.55 + 0.45 * (2.0 * std::f64::consts::PI * 3.0 * t).sin();
    ((2.0 * std::f64::consts::PI * TONE_HZ * t).sin() * TONE_AMP * level) as i16
}

/// The largest step the tone itself makes between two samples, at `gain`.
pub(crate) fn tone_step(gain: f32) -> f32 {
    gain * (TONE_AMP as f32 / 32768.0) * (2.0 * std::f32::consts::PI * TONE_HZ as f32 / 48_000.0)
}

/// What a clean output may step by: the tone's slope with room for the
/// codec's error (a cut or an unfaded start is 10 to 40 times this).
pub(crate) fn clean_step(gain: f32) -> f32 {
    2.5 * tone_step(gain) + 4.0 / 32768.0
}

pub(crate) fn max_step(x: &[f32]) -> (f32, usize) {
    let mut worst = (0.0f32, 0usize);
    for (i, w) in x.windows(2).enumerate() {
        let d = (w[1] - w[0]).abs();
        if d > worst.0 {
            worst = (d, i + 1);
        }
    }
    worst
}

/// A device: a thread rendering the output's mix. It renders when told to
/// (so a test's timeline is exact), and by itself once its stream is being
/// closed (`OutputCtl::fade_out`), as a real device's clock would go on.
pub(crate) struct Device {
    tx: Option<mpsc::Sender<usize>>,
    ack: mpsc::Receiver<()>,
    /// Channel 0 of everything the device was handed.
    pub played: Arc<Mutex<Vec<f32>>>,
    closed: Arc<AtomicBool>,
}

/// The stream of a `Device`, as an `OutputHandle` holds it: dropping it
/// closes the device.
struct DeviceStream {
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DeviceStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.thread.take().unwrap().join();
        self.closed.store(true, Ordering::Release);
    }
}

impl Device {
    /// Open a device on `peers` (and stream audio, if given): the handle the
    /// pipeline would hold, and the test's end of the device.
    pub(crate) fn open(
        peers: &PeerList,
        stream: Option<(Arc<Mutex<HeapCons<i16>>>, Arc<AtomicBool>)>,
    ) -> (OutputHandle, Device) {
        let ctl = OutputCtl::new(48_000);
        let (cues, voices) = CueBus::new(48_000, std::thread::current(), u64::MAX);
        let mut mix = OutputMix::new(2, Arc::clone(peers), stream, Arc::new(OutputPullStats::new(None)), Arc::clone(&ctl), voices);
        let (tx, rx) = mpsc::channel::<usize>();
        let (ack_tx, ack) = mpsc::channel::<()>();
        let played = Arc::new(Mutex::new(Vec::new()));
        let (stop, closed) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (played_t, stop_t, ctl_t) = (Arc::clone(&played), Arc::clone(&stop), Arc::clone(&ctl));
        let thread = std::thread::spawn(move || {
            let mut buf = [0f32; 2 * 480];
            let mut render = |frames: usize| {
                mix.render(&mut buf[..2 * frames]);
                played_t.lock().unwrap().extend(buf[..2 * frames].chunks_exact(2).map(|f| f[0]));
            };
            while !stop_t.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_micros(300)) {
                    Ok(frames) => {
                        render(frames);
                        let _ = ack_tx.send(());
                    }
                    // Being closed: the device's clock runs on.
                    Err(_) if !ctl_t.is_live() => render(480),
                    Err(_) => {}
                }
            }
        });
        let stream = DeviceStream { stop, closed: Arc::clone(&closed), thread: Some(thread) };
        (
            OutputHandle { stream: Box::new(stream), rate: 48_000, ctl, cues: Some(cues) },
            Device { tx: Some(tx), ack, played, closed },
        )
    }

    /// One 10 ms callback.
    pub(crate) fn callback(&self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if self.tx.as_ref().unwrap().send(480).is_ok() {
            let _ = self.ack.recv_timeout(Duration::from_secs(2));
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub(crate) fn played(&self) -> Vec<f32> {
        self.played.lock().unwrap().clone()
    }
}

/// One talking peer received as the pipeline receives it, played on a
/// device.
pub(crate) struct Rx {
    pub peers: HashMap<PeerKey, PeerAudio>,
    pub list: PeerList,
    pub out: Option<OutputHandle>,
    pub device: Device,
    pub retiring: Vec<Retiring>,
    pub user_volumes: HashMap<String, f32>,
    pub deafened: bool,
    enc: OpusEncoder,
    sent: Vec<Vec<u8>>,
    seq: u16,
    pub step: usize,
    t0: Instant,
    pub event_tx: mpsc::Sender<super::pipeline::VoiceEvent>,
    _event_rx: mpsc::Receiver<super::pipeline::VoiceEvent>,
    /// What every device so far played, in order, with 10 ms of silence
    /// where one closed and the next opened.
    pub heard: Vec<f32>,
    pub stream_cons: Arc<Mutex<HeapCons<i16>>>,
    pub stream_prod: Mutex<HeapProd<i16>>,
    pub stream_stereo: Arc<AtomicBool>,
}

impl Rx {
    pub(crate) fn new() -> Rx {
        let list: PeerList = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let (sp, sc) = HeapRb::<i16>::new(FRAME_SIZE * 48).split();
        let stream_cons = Arc::new(Mutex::new(sc));
        let stream_stereo = Arc::new(AtomicBool::new(false));
        let (out, device) = Device::open(&list, Some((Arc::clone(&stream_cons), Arc::clone(&stream_stereo))));
        let (event_tx, _event_rx) = mpsc::channel();
        Rx {
            peers: HashMap::new(),
            list,
            out: Some(out),
            device,
            retiring: Vec::new(),
            user_volumes: HashMap::new(),
            deafened: false,
            enc: OpusEncoder::new(64_000).unwrap(),
            sent: Vec::new(),
            seq: 0,
            step: 0,
            t0: Instant::now(),
            event_tx,
            _event_rx,
            heard: Vec::new(),
            stream_cons,
            stream_prod: Mutex::new(sp),
            stream_stereo,
        }
    }

    pub(crate) fn now(&self) -> Instant {
        self.t0 + Duration::from_millis(self.step as u64 * 20)
    }

    pub(crate) fn peer(&mut self) -> &mut PeerAudio {
        self.peers.get_mut(&peer_key(PEER)).unwrap()
    }

    /// The peer's packet for this step: 20 ms of the tone, continuous across
    /// steps. Encoded every step (the encoder's state runs on whether or not
    /// the packet arrives) and kept, so a test can deliver one late.
    fn packet(&mut self) -> Vec<u8> {
        let mut frame = [0i16; FRAME_SIZE];
        for (i, s) in frame.iter_mut().enumerate() {
            let t = (self.step * FRAME_SIZE + i) as f64 / 48_000.0;
            *s = talker(t);
        }
        let mut opus = [0u8; MAX_OPUS_FRAME_SIZE];
        let n = self.enc.encode(&frame, &mut opus).unwrap();
        let mut flagged = vec![0u8];
        flagged.extend_from_slice(&opus[..n]);
        let pkt = UdpAudioPacket::new_audio(PEER, self.step as u16, &flagged).unwrap().to_bytes();
        self.sent.push(pkt.clone());
        pkt
    }

    /// A frame from the sender side of a test (`Tx`): encoded and delivered
    /// as the pipeline sends it, `flags` in front.
    pub(crate) fn send_pcm(&mut self, frame: Option<&[i16; FRAME_SIZE]>, flags: u8) {
        let now = self.now();
        let mut opus = [0u8; MAX_OPUS_FRAME_SIZE];
        let n = match frame {
            Some(f) => self.enc.encode(f, &mut opus).unwrap(),
            None => self.enc.encode_silence(&mut opus).unwrap(),
        };
        let mut flagged = vec![flags];
        flagged.extend_from_slice(&opus[..n]);
        let mut pkt = UdpAudioPacket::new_audio(PEER, self.seq, &flagged).unwrap().to_bytes();
        self.seq = self.seq.wrapping_add(1);
        let rx = VoiceRx { ring: None, self_key: &peer_key("9"), output_rate: 48_000, event_tx: &self.event_tx };
        if on_voice_datagram(&mut pkt, &rx, &mut self.peers, &move || now) == super::pipeline::Datagram::NewPeer {
            let snapshot = self.peers.values().map(|p| p.output_handle()).collect();
            self.list.store(Arc::new(snapshot));
        }
    }

    /// The receiving half of a step when the sender side is a `Tx`: decode
    /// what is due, and the device pulls 20 ms.
    pub(crate) fn play_step(&mut self) {
        let now = self.now();
        let low_water = 480 + 480;
        for peer in self.peers.values_mut() {
            drain_peer_voice(peer, &mut VoiceDrain {
                now,
                ring_paced: true,
                low_water,
                prime: low_water.div_ceil(FRAME_SIZE) + 1,
                deafened: false,
                aec_enabled: false,
                user_volumes: &self.user_volumes,
                event_tx: &self.event_tx,
                dump: None,
            });
        }
        self.device.callback();
        self.device.callback();
        self.step += 1;
    }

    /// The packet of an earlier step arrives now.
    pub(crate) fn late(&mut self, step: usize) {
        let now = self.now();
        let mut pkt = self.sent[step].clone();
        let rx = VoiceRx { ring: None, self_key: &peer_key("9"), output_rate: 48_000, event_tx: &self.event_tx };
        on_voice_datagram(&mut pkt, &rx, &mut self.peers, &move || now);
    }

    /// One 20 ms step of the pipeline's loop and the device's clock.
    /// `deliver`: whether this step's packet arrives.
    pub(crate) fn tick(&mut self, deliver: bool) {
        self.feed(deliver);
        self.device.callback();
        self.device.callback();
        reap_retired(&mut self.retiring);
    }

    /// The pipeline's half of a step: the packet in, due frames decoded into
    /// the ring (which then holds what the device is about to pull).
    pub(crate) fn feed(&mut self, deliver: bool) {
        let now = self.now();
        let mut pkt = self.packet();
        if deliver {
            let rx = VoiceRx { ring: None, self_key: &peer_key("9"), output_rate: 48_000, event_tx: &self.event_tx };
            let got = on_voice_datagram(&mut pkt, &rx, &mut self.peers, &move || now);
            if got == super::pipeline::Datagram::NewPeer {
                let snapshot = self.peers.values().map(|p| p.output_handle()).collect();
                self.list.store(Arc::new(snapshot));
            }
        }
        // The low-water mark of a 10 ms pull: pull + one tick of margin.
        let low_water = 480 + 480;
        for peer in self.peers.values_mut() {
            drain_peer_voice(peer, &mut VoiceDrain {
                now,
                ring_paced: !self.deafened,
                low_water,
                prime: low_water.div_ceil(FRAME_SIZE) + 1,
                deafened: self.deafened,
                aec_enabled: false,
                user_volumes: &self.user_volumes,
                event_tx: &self.event_tx,
                dump: None,
            });
        }
        self.step += 1;
    }

    pub(crate) fn run(&mut self, steps: usize) {
        for _ in 0..steps {
            self.tick(true);
        }
    }

    /// The user picks another output device: what the pipeline's
    /// `SetOutputDevice` does to the old stream, the rings and the new one.
    pub(crate) fn switch_output_device(&mut self) {
        // Mid-step, as it happens: the ring holds audio the old device has
        // not pulled yet.
        self.feed(true);
        if let Some(old) = self.out.take() {
            retire_output(old, &mut self.retiring);
        }
        for peer in self.peers.values_mut() {
            peer.drain_ring();
        }
        let (out, device) = Device::open(&self.list, Some((Arc::clone(&self.stream_cons), Arc::clone(&self.stream_stereo))));
        self.out = Some(out);
        let old = std::mem::replace(&mut self.device, device);
        // The old device plays on until the pipeline closes it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !old.is_closed() {
            assert!(Instant::now() < deadline, "the old stream was never closed");
            reap_retired(&mut self.retiring);
            std::thread::sleep(Duration::from_millis(1));
        }
        self.heard.extend(old.played());
        self.heard.extend(std::iter::repeat(0.0).take(480));
    }

    /// Deafen / undeafen, as the pipeline applies it: frames are decoded
    /// (for the speaking ring) but not played, and the output is told.
    pub(crate) fn set_deafen(&mut self, deafened: bool) {
        self.deafened = deafened;
        if let Some(out) = &self.out {
            out.ctl.set_voice(!deafened);
        }
    }

    /// Everything heard, the current device's part included.
    pub(crate) fn heard(&self) -> Vec<f32> {
        let mut all = self.heard.clone();
        all.extend(self.device.played());
        all
    }
}

fn assert_clean(heard: &[f32], gain: f32, what: &str) {
    let (step, at) = max_step(heard);
    let limit = clean_step(gain);
    assert!(
        step <= limit,
        "{what}: a step of {:.4} ({:.0} of 32768) at sample {at}, where the tone's own is {:.4} and the limit {:.4}",
        step,
        step * 32768.0,
        tone_step(gain),
        limit
    );
}

/// A steady talker, start to finish: the harness itself is clean (the
/// talkspurt's onset is already faded in by the receiver).
#[test]
fn a_steady_talker_is_clean() {
    let mut rx = Rx::new();
    rx.run(80);
    let heard = rx.heard();
    assert!(heard.iter().any(|s| s.abs() > 0.15), "the tone was played");
    assert_clean(&heard, 1.0, "steady");
}

/// Audit 1: the output device switched while someone talks. The old stream
/// fades out before it is closed and the first frame on the new one fades
/// in; before, the old one was closed mid-wave and the new one started at
/// full level.
#[test]
fn switching_the_output_device_mid_speech_fades_out_and_in() {
    // Five moments, so the switch lands on different phases of the tone.
    for at in 60..65 {
        let mut rx = Rx::new();
        rx.run(at);
        rx.switch_output_device();
        rx.run(40);
        let heard = rx.heard();
        let tail = &heard[heard.len() - 9_600..];
        assert!(tail.iter().any(|s| s.abs() > 0.15), "the tone plays on the new device");
        assert_clean(&heard, 1.0, &format!("device switch at step {at}"));
        assert!(rx.retiring.is_empty(), "the old stream was closed");
    }
}

/// Audit 2: deafen and undeafen while someone talks. Deafening ramps the
/// voices down in the output callback (before, what was queued played out
/// and stopped mid-wave); undeafening starts the peer's next frame with the
/// talkspurt fade-in (before, at full level out of silence).
#[test]
fn deafen_and_undeafen_mid_speech_are_ramps() {
    for at in 60..65 {
        let mut rx = Rx::new();
        rx.run(at);
        rx.feed(true);
        rx.set_deafen(true);
        rx.device.callback();
        rx.device.callback();
        rx.run(30);
        let deaf = rx.heard();
        assert!(deaf[deaf.len() - 20 * 960..].iter().all(|&s| s == 0.0), "silent while deafened");
        rx.set_deafen(false);
        rx.run(40);
        let heard = rx.heard();
        assert!(heard[heard.len() - 9_600..].iter().any(|s| s.abs() > 0.15), "the voice is back");
        assert_clean(&heard, 1.0, &format!("deafen at step {at}"));
    }
}

/// Audit 3: a user's volume dragged while they talk. The gain used to step
/// at a 20 ms frame boundary ("zipper" noise: a step of the wave's value
/// times the change); it now ramps across the frame.
#[test]
fn a_volume_change_mid_speech_is_a_ramp() {
    let mut rx = Rx::new();
    rx.run(40);
    // A drag: a new gain every few frames, down to a quarter and up to 1.5.
    let gains = [0.8, 0.55, 0.25, 0.4, 0.9, 1.2, 1.5, 1.1, 0.6, 1.0, 0.3, 1.5];
    for (i, g) in gains.iter().enumerate() {
        rx.user_volumes.insert(PEER.to_string(), *g);
        rx.run(2 + i % 3);
    }
    rx.run(20);
    let heard = rx.heard();
    // The ramp's own step on top of the wave's: the gain change spread over
    // a frame, times the wave's peak.
    let (step, at) = max_step(&heard);
    let limit = clean_step(1.5) + 1.25 * (TONE_AMP as f32 / 32768.0) / FRAME_SIZE as f32;
    assert!(step <= limit, "volume drag: a step of {:.4} ({:.0} of 32768) at sample {at}; limit {:.4}", step, step * 32768.0, limit);
    assert!(heard[heard.len() - 9_600..].iter().any(|s| s.abs() > 0.2), "ends at the last gain (1.5)");
}

/// Audit 7: the output callback finding the stream-audio ring's lock taken
/// (the pipeline's own look at it). It used to write a whole period of
/// silence, voices included; the voices now play on and only stream audio
/// sits that period out.
#[test]
fn a_busy_stream_audio_lock_does_not_silence_the_voices() {
    for at in 60..65 {
        let mut rx = Rx::new();
        rx.run(at);
        rx.feed(true);
        {
            let _held = rx.stream_cons.lock().unwrap();
            rx.device.callback();
        }
        rx.device.callback();
        rx.run(20);
        assert_clean(&rx.heard(), 1.0, &format!("lock held at step {at}"));
    }
}

/// Audit 8: ten lost frames in a row reset the jitter buffer, by design (the
/// audio is unintelligible by then). Not a click: the concealment fades out
/// before the reset and playback fades back in after it.
#[test]
fn a_run_of_lost_frames_resets_without_a_step() {
    for (from, lost) in [(100, 12), (101, 14), (103, 25), (102, 4)] {
        let mut rx = Rx::new();
        for _ in 0..180 {
            let deliver = !(from..from + lost).contains(&rx.step);
            rx.tick(deliver);
        }
        let heard = rx.heard();
        assert!(heard[heard.len() - 9_600..].iter().any(|s| s.abs() > 0.15), "the voice is back after the loss");
        let jb = &rx.peer().voice_jitter;
        println!("{lost} lost from {from}: lost-frame concealments {}, expansions {}, underruns {}", jb.plc_frames, jb.expand_frames, jb.underruns);
        assert_clean(&heard, 1.0, &format!("{lost} frames lost from step {from}"));
    }
}

// ── Stream audio ─────────────────────────────────────────────────────────────

/// A watched stream's audio received as the pipeline receives it (stereo
/// Opus into the stream jitter buffer, `drain_peer_stream` into the stream
/// ring) and played by a device with stream audio in its mix.
struct StreamRx {
    rx: Rx,
    enc: super::codec::StereoOpusEncoder,
    out: super::pipeline::StreamOut,
    render_fifo: std::collections::VecDeque<[f32; FRAME_SIZE]>,
    /// Packets held back by the network, delivered together later.
    held: Vec<Vec<u8>>,
}

const STREAMER: &str = "4712";

impl StreamRx {
    fn new() -> Self {
        super::watch_stream_add(STREAMER);
        StreamRx {
            rx: Rx::new(),
            enc: super::codec::StereoOpusEncoder::new(128_000).unwrap(),
            out: super::pipeline::StreamOut::new(48_000),
            render_fifo: Default::default(),
            held: Vec::new(),
        }
    }

    /// One 20 ms step. `hold`: this step's packet is kept back; `release`:
    /// everything kept back arrives now, before this step's own.
    fn tick(&mut self, hold: bool, release: bool) {
        let rx = &mut self.rx;
        let now = rx.now();
        let mut pcm = [0i16; 2 * FRAME_SIZE];
        for i in 0..FRAME_SIZE {
            let s = talker((rx.step * FRAME_SIZE + i) as f64 / 48_000.0);
            pcm[2 * i] = s;
            pcm[2 * i + 1] = s;
        }
        let mut opus = [0u8; MAX_OPUS_FRAME_SIZE];
        let n = self.enc.encode(&pcm, &mut opus).unwrap();
        let pkt = UdpAudioPacket::new_stream_audio(STREAMER, rx.step as u16, &opus[..n]).unwrap().to_bytes();
        let mut arriving = Vec::new();
        if release {
            arriving.append(&mut self.held);
        }
        if hold {
            self.held.push(pkt);
        } else {
            arriving.push(pkt);
        }
        for mut pkt in arriving {
            let vrx = VoiceRx { ring: None, self_key: &peer_key("9"), output_rate: 48_000, event_tx: &rx.event_tx };
            on_voice_datagram(&mut pkt, &vrx, &mut rx.peers, &move || now);
        }
        let low_water = 480 + 480;
        for peer in rx.peers.values_mut() {
            super::pipeline::drain_peer_stream(peer, &mut super::pipeline::StreamDrain {
                now,
                ring_paced: true,
                low_water,
                prime: low_water.div_ceil(FRAME_SIZE) + 1,
                stereo: false,
                volume: 1.0,
                aec_enabled: false,
                prod: &rx.stream_prod,
                out: &mut self.out,
                render_fifo: &mut self.render_fifo,
            });
        }
        rx.device.callback();
        rx.device.callback();
        rx.step += 1;
    }
}

/// Audit 9: stream audio. Its start (and its restart after the stream's
/// packets stopped for a while) fades in; a network stall ends in a faded
/// concealment; and when the burst that follows a stall leaves the buffer
/// more than 60 ms over its target, the frames cut to catch up are
/// crossfaded into what follows instead of spliced wave-to-wave.
#[test]
fn stream_audio_starts_stalls_and_catches_up_without_a_step() {
    let _one = STREAM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    // (the stall's first step, its length, whether its packets arrive late
    // in a burst or never)
    for (from, len, burst) in [(60, 10, true), (61, 12, true), (63, 14, true), (60, 10, false), (62, 30, false)] {
        let mut s = StreamRx::new();
        for _ in 0..160 {
            let step = s.rx.step;
            let stalled = (from..from + len).contains(&step);
            s.tick(stalled, burst && step == from + len);
        }
        let heard = s.rx.heard();
        let dropped = s.rx.peers[&peer_key(STREAMER)].stream_jitter.dropped_frames;
        println!("stall of {len} from step {from}, burst {burst}: {dropped} frames cut to catch up");
        assert!(!burst || dropped > 0, "the burst was cut down to the target");
        assert!(heard[heard.len() - 9_600..].iter().any(|v| v.abs() > 0.15), "the stream's audio plays again");
        assert_clean(&heard, 1.0, &format!("stream stall of {len} from step {from} (burst {burst})"));
    }
    super::watch_stream_remove(STREAMER);
}

/// The watch set is process-wide.
static STREAM_TESTS: Mutex<()> = Mutex::new(());

/// The same for voice: a network stall whose packets then arrive in a burst
/// leaves the jitter buffer far over its target, and past 200 ms over even
/// speech is cut to catch up. The cut is crossfaded.
#[test]
fn a_late_burst_of_voice_is_cut_down_without_a_step() {
    for (from, len) in [(60usize, 22usize), (61, 26), (63, 30)] {
        let mut rx = Rx::new();
        let mut held: Vec<usize> = Vec::new();
        for _ in 0..200 {
            let step = rx.step;
            if (from..from + len).contains(&step) {
                held.push(step);
                rx.tick(false);
                continue;
            }
            // The held packets arrive together, just before this step's own.
            for seq in held.drain(..) {
                rx.late(seq);
            }
            rx.tick(true);
        }
        let dropped = rx.peer().voice_jitter.dropped_frames;
        println!("voice stall of {len} from step {from}: {dropped} frames cut to catch up");
        let heard = rx.heard();
        assert!(heard[heard.len() - 9_600..].iter().any(|s| s.abs() > 0.15));
        assert_clean(&heard, 1.0, &format!("voice stall of {len} from step {from}"));
    }
}

/// A short stall (the packets are late, not lost): the receiver conceals,
/// fading, and the late packets then resume playback. The first real frame
/// ramps back up from the level the concealment had faded to; at full level
/// straight after a faded frame it was a step.
#[test]
fn late_packets_resume_from_the_concealment_without_a_step() {
    let mut concealed = 0;
    // Around a peak of the talker's level (steps 84 to 92).
    for from in [83usize, 84, 85, 86] {
        for stall in 2..=6usize {
            let mut rx = Rx::new();
            let mut held: Vec<usize> = Vec::new();
            for _ in 0..140 {
                let step = rx.step;
                if (from..from + stall).contains(&step) {
                    held.push(step);
                    rx.tick(false);
                    continue;
                }
                for seq in held.drain(..) {
                    rx.late(seq);
                }
                rx.tick(true);
            }
            concealed += rx.peer().voice_jitter.expand_frames;
            let expansions = rx.peer().voice_jitter.expand_frames;
            println!("stall {stall} from {from}: {expansions} expansions, largest step {:.4} (limit {:.4})", max_step(&rx.heard()).0, clean_step(1.0));
            assert_clean(&rx.heard(), 1.0, &format!("stall of {stall} from step {from}"));
        }
    }
    assert!(concealed > 20, "the longer stalls were concealed ({concealed} frames)");
}

// ── The sending side ─────────────────────────────────────────────────────────

use super::capture_chain::{rms_db, CaptureChain, TxGate};
use super::pipeline::FLAG_SILENCE;
use super::voice_gate::Gate as VoiceGate;

/// A microphone into the pipeline's capture path (the capture ring, the
/// chain, the transmit gate), sending as the pipeline's loop sends: each
/// captured frame is processed, gated and, when the gate says so, encoded
/// and delivered to an `Rx`, which plays it.
struct Tx {
    chain: CaptureChain,
    gate: TxGate,
    prod: HeapProd<i16>,
    cons: HeapCons<i16>,
    /// The signal at the microphone, by sample index.
    mic: fn(usize) -> i16,
    at: usize,
    muted: bool,
    hotkey: VoiceGate,
}

fn mic_talker(i: usize) -> i16 {
    talker(i as f64 / 48_000.0)
}

/// The same talker, quiet (a tenth): room for gain control to lift it.
fn mic_quiet(i: usize) -> i16 {
    talker(i as f64 / 48_000.0) / 10
}

/// Another microphone: the talker at 80 %, 13.7 ms later in its wave.
fn mic_other(i: usize) -> i16 {
    (talker((i + 658) as f64 / 48_000.0) as f32 * 0.8) as i16
}

impl Tx {
    fn new(mic: fn(usize) -> i16) -> Tx {
        let (prod, cons) = HeapRb::<i16>::new(FRAME_SIZE * 48).split();
        Tx { chain: CaptureChain::new(48_000), gate: TxGate::new(), prod, cons, mic, at: 0, muted: false, hotkey: VoiceGate::Vad }
    }

    /// 20 ms at the microphone, through to the listener's device.
    fn step(&mut self, rx: &mut Rx) {
        use ringbuf::traits::Producer;
        for _ in 0..FRAME_SIZE {
            let _ = self.prod.try_push((self.mic)(self.at));
            self.at += 1;
        }
        self.chain.pull(&mut self.cons);
        while let Some(mut frame) = self.chain.next_frame() {
            if self.gate.wants_processing(self.muted) {
                self.chain.process(&mut frame);
            }
            let level = rms_db(&frame);
            let tx = self.gate.step(&mut frame, level, -50.0, self.muted, self.hotkey);
            if let Some(pre) = &tx.pre_roll {
                rx.send_pcm(Some(pre), 0);
            }
            if tx.transmit_voice || tx.tail_index.is_some() {
                rx.send_pcm(Some(&frame), if tx.transmit_voice { 0 } else { FLAG_SILENCE });
            }
        }
        rx.play_step();
    }

    fn run(&mut self, rx: &mut Rx, steps: usize) {
        for _ in 0..steps {
            self.step(rx);
        }
    }

    /// The user picks another microphone; opening it takes `gap` steps, in
    /// which nothing is captured or sent (the pipeline's `SetInputDevice`).
    fn switch_input_device(&mut self, rx: &mut Rx, mic: fn(usize) -> i16, gap: usize) {
        use ringbuf::traits::Consumer;
        if self.gate.cut() {
            rx.send_pcm(None, FLAG_SILENCE);
        }
        while self.cons.try_pop().is_some() {}
        self.chain.clear_capture();
        self.chain.reset_processor();
        for _ in 0..gap {
            rx.play_step();
            self.at += FRAME_SIZE;
        }
        self.mic = mic;
    }
}

/// What a clean signal steps by, measured on `steady` (a stretch of the same
/// talker with nothing happening): for tests where the level heard depends
/// on gain control, so the tone's slope can't be computed ahead.
fn assert_no_step_beyond(heard: &[f32], steady: std::ops::Range<usize>, what: &str) {
    let own = max_step(&heard[steady]).0;
    let (step, at) = max_step(heard);
    assert!(own > 0.0, "{what}: nothing was heard in the steady stretch");
    assert!(
        step <= 1.6 * own + 4.0 / 32768.0,
        "{what}: a step of {:.4} ({:.0} of 32768) at sample {at}, where the steady signal's largest is {:.4}",
        step,
        step * 32768.0,
        own
    );
}

/// The sending side as a whole is clean: a talker through the capture
/// chain and the gate to the listener's device (the gate's pre-roll and the
/// receiver's onset fade shape the start).
#[test]
fn a_steady_sender_is_clean() {
    let mut rx = Rx::new();
    let mut tx = Tx::new(mic_talker);
    tx.run(&mut rx, 120);
    let heard = rx.heard();
    assert!(heard.iter().any(|s| s.abs() > 0.15));
    assert_clean(&heard, 1.0, "steady sender");
}

/// Audit 4: the microphone switched mid-word. The talkspurt is ended on
/// the wire before the old device closes (a flagged frame: the listener
/// fades the last voice frame out) and the new device's first frame starts
/// a new one (faded in, no pre-roll from the old device). Before, the gate
/// stayed open and the new microphone's signal was butted against the old.
#[test]
fn switching_the_microphone_mid_word_ends_the_talkspurt() {
    for at in [84usize, 85, 86, 87] {
        for gap in [0usize, 1, 2, 3, 6] {
            let mut rx = Rx::new();
            let mut tx = Tx::new(mic_talker);
            tx.run(&mut rx, at);
            tx.switch_input_device(&mut rx, mic_other, gap);
            tx.run(&mut rx, 60);
            let heard = rx.heard();
            assert!(heard[heard.len() - 9_600..].iter().any(|s| s.abs() > 0.1), "the new microphone is heard");
            assert_clean(&heard, 1.0, &format!("microphone switch at step {at}, {} ms to open", gap * 20));
        }
    }
}

/// Audit 10, first half: a push-to-talk press mid-sound. No pre-roll by
/// design (the frame before the press is audio from before the key went
/// down), so the first frame is faded in over 10 ms; the release ends with
/// the 60 ms tail. Neither is a step.
#[test]
fn push_to_talk_presses_and_releases_are_faded() {
    for at in [84usize, 85, 86, 87] {
        let mut rx = Rx::new();
        let mut tx = Tx::new(mic_talker);
        tx.hotkey = VoiceGate::Closed; // push-to-talk mode, key up
        tx.run(&mut rx, at);
        assert!(rx.peers.is_empty(), "nothing is sent with the key up");
        tx.hotkey = VoiceGate::Open;
        tx.run(&mut rx, 30);
        tx.hotkey = VoiceGate::Closed;
        tx.run(&mut rx, 30);
        let heard = rx.heard();
        assert!(heard.iter().any(|s| s.abs() > 0.1), "the held key was heard");
        assert!(heard[heard.len() - 9_600..].iter().all(|&s| s == 0.0), "silent after the release");
        assert_clean(&heard, 1.0, &format!("push-to-talk pressed at step {at}"));
    }
}

/// Audit 10, second half: mute mid-word, with gain control on. The 60 ms
/// tail that follows the mute is the processed signal fading out, like the
/// frames before it; it used to be the raw microphone (processing stopped
/// at the mute), a jump in level at the mute.
#[test]
fn muting_mid_word_fades_the_processed_signal() {
    for at in [150usize, 151, 152, 153] {
        let mut rx = Rx::new();
        let mut tx = Tx::new(mic_quiet);
        tx.chain.set_agc(true);
        tx.run(&mut rx, at);
        tx.muted = true;
        tx.run(&mut rx, 30);
        let heard = rx.heard();
        assert!(heard[heard.len() - 9_600..].iter().all(|&s| s == 0.0), "silent once muted");
        // Gain control starts high and settles: everything before the mute
        // is the measure of the signal's own steps.
        assert_no_step_beyond(&heard, 0..(at - 6) * 960, &format!("mute at step {at}"));
    }
}

/// Audit 5: echo cancellation, gain control or noise suppression switched
/// mid-word. The frame after the switch is a crossfade from the old
/// processing to the new (the processor has to be rebuilt, and with it the
/// gain it had settled on), where it used to jump.
#[test]
fn toggling_the_voice_processing_mid_word_is_a_crossfade() {
    let toggles: [(&str, fn(&mut CaptureChain), fn(&mut CaptureChain)); 5] = [
        ("gain control off", |c| drop(c.set_agc(true)), |c| drop(c.set_agc(false))),
        ("gain control on", |_| {}, |c| drop(c.set_agc(true))),
        ("echo cancellation off, gain control on", |c| drop((c.set_aec(true), c.set_agc(true))), |c| drop(c.set_aec(false))),
        ("noise suppression off", |c| drop(c.set_ns_level(2)), |c| drop(c.set_ns_level(0))),
        ("noise suppression on", |_| {}, |c| drop(c.set_ns_level(2))),
    ];
    for (what, before, toggle) in toggles {
        for at in [150usize, 151, 152] {
            let mut rx = Rx::new();
            let mut tx = Tx::new(mic_quiet);
            before(&mut tx.chain);
            tx.run(&mut rx, at);
            toggle(&mut tx.chain);
            tx.run(&mut rx, 60);
            let heard = rx.heard();
            // The louder of the two settings sets what a clean step is.
            // (Gain control starts high and settles: all of it before the
            // switch counts, and the settled stretch after.)
            let own = max_step(&heard[..(at - 6) * 960]).0.max(max_step(&heard[heard.len() - 30 * 960..]).0);
            let (step, sample) = max_step(&heard);
            assert!(own > 0.0, "{what}: heard nothing");
            assert!(step <= 1.6 * own + 4.0 / 32768.0, "{what} at step {at}: a step of {:.4} at sample {sample}; the steady signal's largest is {:.4}", step, own);
        }
    }
}

/// Changing the noise-suppression level between two "on" levels changes
/// nothing in the path (the level only says on or off): the samples sent are
/// the same as without the change. It used to rebuild the suppressor and the
/// echo canceller / gain control with it, restarting all three mid-word.
#[test]
fn a_noise_suppression_level_change_restarts_nothing() {
    let run = |change: bool| {
        let mut rx = Rx::new();
        let mut tx = Tx::new(mic_quiet);
        tx.chain.set_ns_level(1);
        tx.chain.set_agc(true);
        tx.run(&mut rx, 120);
        if change {
            tx.chain.set_ns_level(3);
        }
        tx.run(&mut rx, 40);
        rx.heard()
    };
    assert!(run(false) == run(true), "the level change altered the signal");
}
