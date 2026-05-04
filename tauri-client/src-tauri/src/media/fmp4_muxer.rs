//! Fragmented MP4 (fMP4) muxer for Linux MSE playback.
//!
//! On Linux WebKitGTK, decoded NV12 over Tauri IPC was the wall — 5.5MB
//! per frame at 1440p saturated the IPC at ~75MB/s. Encoded H.264 / HEVC
//! / AV1 is ~50-100KB per frame for the same stream, two orders of
//! magnitude smaller. Feeding it to `<video>` via Media Source Extensions
//! lets WebKit's GStreamer backend handle decode (hardware-accelerated
//! via nvh264dec / nvh265dec / nvav1dec) and the browser compositor
//! handles render. No decode in Rust, no NV12 IPC, no WebGL upload.
//!
//! What we mux:
//!
//! - **Init segment** (one-time per codec/dimension change): `ftyp` +
//!   `moov` describing the codec, dimensions, and sample-description box
//!   that the SourceBuffer uses to configure its decoder.
//! - **Media segments** (per frame): `styp` + `moof` + `mdat`. The
//!   `moof` carries the sample's timing and size; `mdat` carries the
//!   encoded bytes themselves.
//!
//! Sample byte format (what goes inside mdat):
//! - H.264 / HEVC: AVCC (4-byte length-prefixed NAL units). The encoder
//!   already produces this format, so we copy through.
//! - AV1: raw OBU stream (low-overhead bitstream format), ditto.

use std::collections::VecDeque;
use std::time::Instant;
use crate::media::caps::CodecKind;

const TIMESCALE: u32 = 90_000;
/// Final clamp on the per-frame period derived from the arrival
/// window. 4_166µs ≈ 240fps, 200_000µs = 5fps. Catches degenerate
/// histories before they hit the timeline.
const MIN_PERIOD_US: u64 = 4_166;
const MAX_PERIOD_US: u64 = 200_000;
/// Default period before the arrival window has enough samples to
/// derive one (i.e. the very first frame). 60fps — benign for any
/// source; the window-mean replaces it within a few frames.
const SEED_PERIOD_US: u64 = 16_667;
/// Wall-clock window over which the muxer averages inter-arrival
/// times to compute each sample's display duration. 3s is long
/// enough that a one-off burst of ~10 frames shifts the period by
/// only a few percent (so motion smoothness stays at the source's
/// fps), short enough that an actual fps change settles within a
/// few seconds.
const WINDOW_SECS: f64 = 3.0;
/// Cap on the arrival-history length, in case a malformed source
/// fires frames at impossible rates and we'd otherwise allocate
/// without bound.
const HISTORY_MAX: usize = 1024;

pub struct Fmp4Muxer {
    codec: CodecKind,
    width: u32,
    height: u32,
    /// `mfhd` sequence number, 1-based, monotonic.
    moof_sequence: u32,
    /// `tfdt` to use for the next emitted sample. Strictly equal to
    /// the previous sample's `decode_time + sample_duration` — i.e. the
    /// timeline is GAP-FREE by construction. The video element can't
    /// auto-skip MSE buffered-range gaps, so any timestamp discontinuity
    /// caused a permanent freeze the moment the playhead reached it.
    next_decode_time: u64,
    /// Sliding window of recent arrival instants. Each sample's
    /// duration is `window_span / (window_count - 1)` — a true
    /// inter-arrival average over the last WINDOW_SECS of wall clock.
    /// Far more burst-resistant than EWMA: a 10-frame burst at 1ms
    /// inter-arrival shifts the per-frame period by only ~5% (vs ~70%
    /// for EWMA at α=0.07), which keeps the player's display rate
    /// close to source fps and stops the visible motion judder that
    /// short EWMA-derived durations cause.
    arrival_history: VecDeque<Instant>,
    /// Sub-tick remainder carried between frames. Per-frame integer
    /// division (`period_us · 90000 / 1_000_000`) truncates ~10µs
    /// every time, so over a few seconds the cumulative tfdt drifts
    /// far enough that WebKit's `MediaSourcePrivate::hasFutureTime` —
    /// which uses an 83.4ms `timeFudgeFactor` to decide whether the
    /// playhead is "inside" a buffered range — flips to false and
    /// fires `waiting` even when there's data ahead. Carrying the
    /// remainder keeps tfdt rationally exact and stops the drift.
    duration_remainder: u64,
}

impl Fmp4Muxer {
    pub fn new(codec: CodecKind, width: u32, height: u32) -> Self {
        Self {
            codec,
            width,
            height,
            moof_sequence: 1,
            next_decode_time: 0,
            arrival_history: VecDeque::with_capacity(HISTORY_MAX),
            duration_remainder: 0,
        }
    }

    /// Build the init segment. `codec_config` is the codec-specific
    /// config record:
    /// - H.264: avcC (we synthesize from inline SPS/PPS in the keyframe;
    ///   the caller passes that pre-built avcC blob)
    /// - HEVC: hvcC (the encoder shipped this in `frame.description`)
    /// - AV1: av1C (same)
    pub fn init_segment(&self, codec_config: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(1024);
        write_ftyp(&mut out);
        write_moov(&mut out, self.codec, self.width, self.height, codec_config);
        out
    }

    /// Build a media segment containing one encoded sample.
    /// `sample_bytes` is in the codec's storage format (AVCC NALs for
    /// H.264/HEVC, raw OBU stream for AV1).
    pub fn media_segment(&mut self, sample_bytes: &[u8], is_keyframe: bool) -> Vec<u8> {
        let now = Instant::now();
        // Maintain the sliding arrival window: drop entries older
        // than WINDOW_SECS, then append `now`.
        let cutoff = now - std::time::Duration::from_secs_f64(WINDOW_SECS);
        while let Some(&front) = self.arrival_history.front() {
            if front < cutoff || self.arrival_history.len() >= HISTORY_MAX {
                self.arrival_history.pop_front();
            } else {
                break;
            }
        }
        self.arrival_history.push_back(now);

        // Per-sample period = window-mean of inter-arrival deltas.
        // Window holds N timestamps → N−1 deltas → period = span/(N−1).
        // Far more stable across bursts than a per-frame delta or a
        // short-memory EWMA — keeps the timeline growing at the
        // long-term source cadence so the player's display rate
        // stays at source fps.
        let period_us = if self.arrival_history.len() < 2 {
            SEED_PERIOD_US
        } else {
            let span_us = now
                .duration_since(*self.arrival_history.front().unwrap())
                .as_micros() as u64;
            let deltas = (self.arrival_history.len() - 1) as u64;
            (span_us / deltas).clamp(MIN_PERIOD_US, MAX_PERIOD_US)
        };
        // Carry the µs remainder so tfdt stays rationally exact —
        // see the field's doc-comment for the WebKit fudge-factor
        // interaction this prevents.
        let scaled = period_us * TIMESCALE as u64 + self.duration_remainder;
        let sample_duration = (scaled / 1_000_000) as u32;
        self.duration_remainder = scaled % 1_000_000;
        // decode_time is GAP-FREE by construction: it's exactly the end
        // of the previous sample. The video element can't seek across
        // MSE buffered-range gaps, so a single discontinuity here meant
        // a permanent freeze the moment the playhead reached it.
        let decode_time = self.next_decode_time;

        // moof comes first; trun.data_offset points into the *following*
        // mdat. Build moof with a placeholder offset, then patch once we
        // know the moof size.
        let mut out = Vec::with_capacity(sample_bytes.len() + 256);
        write_styp(&mut out);

        let moof_start = out.len();
        let trun_data_offset_pos = write_moof(
            &mut out,
            self.moof_sequence,
            decode_time,
            sample_duration,
            sample_bytes.len() as u32,
            is_keyframe,
        );
        let moof_size = out.len() - moof_start;
        // trun.data_offset is from the start of the enclosing moof to
        // the first byte of mdat *payload*. mdat header is 8 bytes
        // (size + type), so payload starts at moof_size + 8.
        let data_offset = (moof_size + 8) as u32;
        out[trun_data_offset_pos..trun_data_offset_pos + 4]
            .copy_from_slice(&data_offset.to_be_bytes());

        write_mdat(&mut out, sample_bytes);

        self.moof_sequence += 1;
        self.next_decode_time = decode_time + sample_duration as u64;

        out
    }

    /// MSE codec string for `MediaSource.isTypeSupported(...)` and
    /// `SourceBuffer.changeType(...)`. We use generic-but-broad strings
    /// rather than parsing the actual codec config; the browser is
    /// permissive about exact level matching for `isTypeSupported`.
    pub fn mime_type(&self) -> String {
        let codec = match self.codec {
            CodecKind::H264Hw | CodecKind::H264Sw => "avc1.640033", // High@5.1 — covers up to 4K60
            CodecKind::H265 => "hev1.1.6.L186.B0",                  // Main, L6.2
            CodecKind::Av1 => "av01.0.13M.08",                       // Main, L6.1, 8-bit
            CodecKind::Unknown => "avc1.640033",
        };
        format!("video/mp4; codecs=\"{}\"", codec)
    }
}

// ── Box writing helpers ────────────────────────────────────────────────────

fn write_box<F: FnOnce(&mut Vec<u8>)>(out: &mut Vec<u8>, fourcc: &[u8; 4], body: F) {
    let size_pos = out.len();
    out.extend_from_slice(&[0u8; 4]); // size placeholder
    out.extend_from_slice(fourcc);
    body(out);
    let size = (out.len() - size_pos) as u32;
    out[size_pos..size_pos + 4].copy_from_slice(&size.to_be_bytes());
}

fn write_full_box<F: FnOnce(&mut Vec<u8>)>(
    out: &mut Vec<u8>, fourcc: &[u8; 4], version: u8, flags: u32, body: F,
) {
    write_box(out, fourcc, |buf| {
        buf.push(version);
        let f = flags.to_be_bytes();
        buf.extend_from_slice(&f[1..]); // 24-bit flags
        body(buf);
    });
}

fn put_u32(out: &mut Vec<u8>, v: u32) { out.extend_from_slice(&v.to_be_bytes()); }
fn put_u16(out: &mut Vec<u8>, v: u16) { out.extend_from_slice(&v.to_be_bytes()); }
fn put_u64(out: &mut Vec<u8>, v: u64) { out.extend_from_slice(&v.to_be_bytes()); }

// ── Init-segment boxes ────────────────────────────────────────────────────

fn write_ftyp(out: &mut Vec<u8>) {
    write_box(out, b"ftyp", |buf| {
        buf.extend_from_slice(b"iso5"); // major brand
        put_u32(buf, 1);                // minor version
        // compatible brands
        buf.extend_from_slice(b"iso5");
        buf.extend_from_slice(b"iso6");
        buf.extend_from_slice(b"mp41");
    });
}

fn write_moov(out: &mut Vec<u8>, codec: CodecKind, width: u32, height: u32, codec_config: &[u8]) {
    write_box(out, b"moov", |buf| {
        write_mvhd(buf);
        write_trak(buf, codec, width, height, codec_config);
        write_mvex(buf);
    });
}

fn write_mvhd(out: &mut Vec<u8>) {
    write_full_box(out, b"mvhd", 0, 0, |buf| {
        put_u32(buf, 0);             // creation_time
        put_u32(buf, 0);             // modification_time
        put_u32(buf, TIMESCALE);     // timescale
        put_u32(buf, 0);             // duration (0 = unknown / live)
        put_u32(buf, 0x00010000);    // rate (1.0)
        put_u16(buf, 0x0100);        // volume (1.0)
        put_u16(buf, 0);             // reserved
        put_u32(buf, 0); put_u32(buf, 0); // reserved
        // unity matrix
        for &v in &[0x00010000u32, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000u32] {
            put_u32(buf, v);
        }
        for _ in 0..6 { put_u32(buf, 0); } // pre_defined
        put_u32(buf, 2); // next_track_ID (we only use track 1)
    });
}

fn write_trak(out: &mut Vec<u8>, codec: CodecKind, width: u32, height: u32, codec_config: &[u8]) {
    write_box(out, b"trak", |buf| {
        write_tkhd(buf, width, height);
        write_mdia(buf, codec, width, height, codec_config);
    });
}

fn write_tkhd(out: &mut Vec<u8>, width: u32, height: u32) {
    // version 0, flags 0x000007 = enabled | in_movie | in_preview
    write_full_box(out, b"tkhd", 0, 0x000007, |buf| {
        put_u32(buf, 0);             // creation_time
        put_u32(buf, 0);             // modification_time
        put_u32(buf, 1);             // track_ID
        put_u32(buf, 0);             // reserved
        put_u32(buf, 0);             // duration
        put_u64(buf, 0);             // reserved
        put_u16(buf, 0);             // layer
        put_u16(buf, 0);             // alternate_group
        put_u16(buf, 0);             // volume
        put_u16(buf, 0);             // reserved
        // unity matrix
        for &v in &[0x00010000u32, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000u32] {
            put_u32(buf, v);
        }
        // width / height as 16.16 fixed-point
        put_u32(buf, width << 16);
        put_u32(buf, height << 16);
    });
}

fn write_mdia(out: &mut Vec<u8>, codec: CodecKind, width: u32, height: u32, codec_config: &[u8]) {
    write_box(out, b"mdia", |buf| {
        write_mdhd(buf);
        write_hdlr(buf);
        write_minf(buf, codec, width, height, codec_config);
    });
}

fn write_mdhd(out: &mut Vec<u8>) {
    write_full_box(out, b"mdhd", 0, 0, |buf| {
        put_u32(buf, 0);          // creation
        put_u32(buf, 0);          // modification
        put_u32(buf, TIMESCALE);  // timescale
        put_u32(buf, 0);          // duration
        put_u16(buf, 0x55c4);     // language: undetermined ('und')
        put_u16(buf, 0);          // pre_defined
    });
}

fn write_hdlr(out: &mut Vec<u8>) {
    write_full_box(out, b"hdlr", 0, 0, |buf| {
        put_u32(buf, 0);                  // pre_defined
        buf.extend_from_slice(b"vide");   // handler_type = video
        for _ in 0..3 { put_u32(buf, 0); } // reserved
        buf.extend_from_slice(b"VideoHandler\0"); // name (null-terminated UTF-8)
    });
}

fn write_minf(out: &mut Vec<u8>, codec: CodecKind, width: u32, height: u32, codec_config: &[u8]) {
    write_box(out, b"minf", |buf| {
        // vmhd: video media header
        write_full_box(buf, b"vmhd", 0, 0x000001, |b| {
            put_u16(b, 0);  // graphicsmode
            put_u16(b, 0); put_u16(b, 0); put_u16(b, 0); // opcolor
        });
        // dinf > dref > url (self-contained)
        write_box(buf, b"dinf", |b| {
            write_full_box(b, b"dref", 0, 0, |bb| {
                put_u32(bb, 1); // entry_count
                write_full_box(bb, b"url ", 0, 0x000001, |_| {}); // self-contained
            });
        });
        write_stbl(buf, codec, width, height, codec_config);
    });
}

fn write_stbl(out: &mut Vec<u8>, codec: CodecKind, width: u32, height: u32, codec_config: &[u8]) {
    write_box(out, b"stbl", |buf| {
        // stsd: sample description, contains the codec entry
        write_full_box(buf, b"stsd", 0, 0, |b| {
            put_u32(b, 1); // entry_count
            write_visual_sample_entry(b, codec, width, height, codec_config);
        });
        // stts/stsc/stsz/stco: empty for fragmented
        write_full_box(buf, b"stts", 0, 0, |b| { put_u32(b, 0); });
        write_full_box(buf, b"stsc", 0, 0, |b| { put_u32(b, 0); });
        write_full_box(buf, b"stsz", 0, 0, |b| { put_u32(b, 0); put_u32(b, 0); });
        write_full_box(buf, b"stco", 0, 0, |b| { put_u32(b, 0); });
    });
}

fn write_visual_sample_entry(
    out: &mut Vec<u8>, codec: CodecKind, width: u32, height: u32, codec_config: &[u8],
) {
    let fourcc: &[u8; 4] = match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => b"avc1",
        CodecKind::H265 => b"hev1",
        CodecKind::Av1 => b"av01",
        CodecKind::Unknown => b"avc1",
    };
    write_box(out, fourcc, |buf| {
        // VisualSampleEntry common header
        for _ in 0..6 { buf.push(0); }     // reserved
        put_u16(buf, 1);                    // data_reference_index
        put_u16(buf, 0);                    // pre_defined
        put_u16(buf, 0);                    // reserved
        for _ in 0..3 { put_u32(buf, 0); }  // pre_defined
        put_u16(buf, width as u16);
        put_u16(buf, height as u16);
        put_u32(buf, 0x00480000);           // horizresolution = 72 dpi
        put_u32(buf, 0x00480000);           // vertresolution
        put_u32(buf, 0);                    // reserved
        put_u16(buf, 1);                    // frame_count
        // 32-byte compressorname (length-prefixed)
        buf.push(0);
        for _ in 0..31 { buf.push(0); }
        put_u16(buf, 0x0018);               // depth = 24
        put_u16(buf, 0xffff);               // pre_defined = -1
        // codec config box
        let config_fourcc: &[u8; 4] = match codec {
            CodecKind::H264Hw | CodecKind::H264Sw => b"avcC",
            CodecKind::H265 => b"hvcC",
            CodecKind::Av1 => b"av1C",
            CodecKind::Unknown => b"avcC",
        };
        write_box(buf, config_fourcc, |b| {
            b.extend_from_slice(codec_config);
        });
    });
}

fn write_mvex(out: &mut Vec<u8>) {
    write_box(out, b"mvex", |buf| {
        // trex: track extends
        write_full_box(buf, b"trex", 0, 0, |b| {
            put_u32(b, 1);   // track_ID
            put_u32(b, 1);   // default_sample_description_index
            put_u32(b, 0);   // default_sample_duration
            put_u32(b, 0);   // default_sample_size
            put_u32(b, 0);   // default_sample_flags
        });
    });
}

// ── Media-segment boxes ───────────────────────────────────────────────────

fn write_styp(out: &mut Vec<u8>) {
    write_box(out, b"styp", |buf| {
        buf.extend_from_slice(b"msdh");
        put_u32(buf, 0);
        buf.extend_from_slice(b"msdh");
        buf.extend_from_slice(b"msix");
    });
}

/// Returns the offset within `out` where trun's `data_offset` field lives,
/// so the caller can patch it once mdat is appended.
fn write_moof(
    out: &mut Vec<u8>,
    sequence: u32,
    decode_time: u64,
    duration: u32,
    sample_size: u32,
    is_keyframe: bool,
) -> usize {
    let mut data_offset_pos = 0;
    write_box(out, b"moof", |buf| {
        // mfhd
        write_full_box(buf, b"mfhd", 0, 0, |b| { put_u32(b, sequence); });
        // traf
        write_box(buf, b"traf", |b| {
            // tfhd: default_base_is_moof + default_sample_flags
            // flags: 0x020000 = default-base-is-moof, 0x020 = default-sample-flags-present
            write_full_box(b, b"tfhd", 0, 0x020020, |bb| {
                put_u32(bb, 1); // track_ID
                // default_sample_flags: marks all samples as non-key by default;
                // the trun overrides flags for the keyframe sample.
                put_u32(bb, 0x01010000); // sample_depends_on=1, is_non_sync_sample=1
            });
            // tfdt: base media decode time (version 1 = 64-bit)
            write_full_box(b, b"tfdt", 1, 0, |bb| {
                put_u64(bb, decode_time);
            });
            // trun: one sample. flags:
            //   0x000001 data-offset-present
            //   0x000004 first-sample-flags-present
            //   0x000100 sample-duration-present
            //   0x000200 sample-size-present
            let trun_flags = 0x000001 | 0x000100 | 0x000200
                | if is_keyframe { 0x000004 } else { 0 };
            write_full_box(b, b"trun", 0, trun_flags, |bb| {
                put_u32(bb, 1); // sample_count
                // data_offset placeholder — caller patches after writing moof
                let off = bb.len();
                put_u32(bb, 0);
                if is_keyframe {
                    // first_sample_flags: sample_depends_on=2, is_sync_sample=0
                    put_u32(bb, 0x02000000);
                }
                put_u32(bb, duration);
                put_u32(bb, sample_size);
                data_offset_pos = off;
            });
        });
    });
    data_offset_pos
}

fn write_mdat(out: &mut Vec<u8>, sample_bytes: &[u8]) {
    write_box(out, b"mdat", |buf| {
        buf.extend_from_slice(sample_bytes);
    });
}

// ── avcC synthesis (H.264 path) ────────────────────────────────────────────

/// Build an avcC config record from inline SPS/PPS NAL units extracted
/// from an H.264 keyframe in AVCC format. Returns None if the keyframe
/// doesn't contain both an SPS (NAL type 7) and a PPS (NAL type 8).
///
/// HEVC and AV1 receive their config records pre-built via
/// `frame.description` (we set GLOBAL_HEADER on the encoder for those),
/// so the muxer only needs to synthesize for H.264.
pub fn build_avcc_from_keyframe(avcc_data: &[u8]) -> Option<Vec<u8>> {
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    let mut i = 0;
    while i + 4 <= avcc_data.len() {
        let nal_len = u32::from_be_bytes([
            avcc_data[i], avcc_data[i + 1], avcc_data[i + 2], avcc_data[i + 3],
        ]) as usize;
        i += 4;
        if i + nal_len > avcc_data.len() || nal_len == 0 { break; }
        let nal = &avcc_data[i..i + nal_len];
        let nal_type = nal[0] & 0x1f;
        match nal_type {
            7 => { if sps.is_none() { sps = Some(nal.to_vec()); } }
            8 => { if pps.is_none() { pps = Some(nal.to_vec()); } }
            _ => {}
        }
        i += nal_len;
    }
    let sps = sps?;
    let pps = pps?;
    if sps.len() < 4 { return None; }

    let mut avcc = Vec::with_capacity(7 + 3 + sps.len() + 3 + pps.len());
    avcc.push(1);                 // configurationVersion
    avcc.push(sps[1]);            // AVCProfileIndication
    avcc.push(sps[2]);            // profile_compatibility
    avcc.push(sps[3]);            // AVCLevelIndication
    avcc.push(0xFF);              // reserved (6 bits) + lengthSizeMinusOne (2 bits) = 3 (4-byte length)
    avcc.push(0xE1);              // reserved (3 bits) + numOfSequenceParameterSets (5 bits) = 1
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(&sps);
    avcc.push(1);                 // numOfPictureParameterSets
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(&pps);
    Some(avcc)
}
