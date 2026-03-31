use std::io::Cursor;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::capture::{PixelFormat, RawFrame};
use super::encoder::{EncoderConfig, H264Encoder};
use super::video_packet::{UdpVideoPacket, UDP_MAX_PAYLOAD};

pub enum VideoPipelineControl {
    ForceKeyframe,
    Shutdown,
}

pub enum VideoPipelineEvent {
    Started,
    Stopped,
    Error(String),
    ThumbnailReady(Vec<u8>),
}

/// Convert a raw frame to a small JPEG thumbnail.
/// Handles both NV12 (Windows) and BGRA/RGBA (Linux) input formats.
fn frame_to_jpeg_thumbnail(frame: &RawFrame) -> Option<Vec<u8>> {
    let w = frame.width as usize;
    let h = frame.height as usize;

    // Target thumbnail width ~320px, preserving aspect ratio
    let thumb_w = 320usize.min(w);
    let thumb_h = (thumb_w * h) / w;
    if thumb_w == 0 || thumb_h == 0 {
        return None;
    }

    let mut rgb = vec![0u8; thumb_w * thumb_h * 3];

    match frame.pixel_format {
        PixelFormat::NV12 => {
            let expected = w * h * 3 / 2;
            if frame.data.len() < expected {
                return None;
            }
            let y_plane = &frame.data[..w * h];
            let uv_plane = &frame.data[w * h..];

            for ty in 0..thumb_h {
                let sy = (ty * h) / thumb_h;
                let uv_row = sy / 2;
                for tx in 0..thumb_w {
                    let sx = (tx * w) / thumb_w;
                    let y_val = y_plane[sy * w + sx] as f32;
                    let uv_idx = uv_row * w + (sx & !1);
                    let u_val = uv_plane[uv_idx] as f32 - 128.0;
                    let v_val = uv_plane[uv_idx + 1] as f32 - 128.0;

                    let idx = (ty * thumb_w + tx) * 3;
                    rgb[idx]     = (y_val + 1.402 * v_val).clamp(0.0, 255.0) as u8;
                    rgb[idx + 1] = (y_val - 0.344 * u_val - 0.714 * v_val).clamp(0.0, 255.0) as u8;
                    rgb[idx + 2] = (y_val + 1.772 * u_val).clamp(0.0, 255.0) as u8;
                }
            }
        }
        PixelFormat::BGRA | PixelFormat::RGBA => {
            let stride = frame.stride;
            let is_bgra = frame.pixel_format == PixelFormat::BGRA;
            for ty in 0..thumb_h {
                let sy = (ty * h) / thumb_h;
                for tx in 0..thumb_w {
                    let sx = (tx * w) / thumb_w;
                    let src_idx = sy * stride + sx * 4;
                    let idx = (ty * thumb_w + tx) * 3;
                    if is_bgra {
                        rgb[idx]     = frame.data[src_idx + 2]; // R
                        rgb[idx + 1] = frame.data[src_idx + 1]; // G
                        rgb[idx + 2] = frame.data[src_idx];     // B
                    } else {
                        rgb[idx]     = frame.data[src_idx];     // R
                        rgb[idx + 1] = frame.data[src_idx + 1]; // G
                        rgb[idx + 2] = frame.data[src_idx + 2]; // B
                    }
                }
            }
        }
    }

    use image::ImageEncoder;
    let mut buf = Cursor::new(Vec::with_capacity(16 * 1024));
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 60);
    encoder
        .write_image(&rgb, thumb_w as u32, thumb_h as u32, image::ColorType::Rgb8.into())
        .ok()?;
    Some(buf.into_inner())
}

/// Run the video send pipeline on a dedicated thread.
/// Reads RawFrames from the channel, encodes them, packetizes, and sends via UDP.
///
/// IMPORTANT: `socket` must be the SAME UDP socket used by the voice audio pipeline.
/// The community server identifies senders by their UDP source address, which was
/// learned during voice connection. A different socket would have a different port
/// and the server would reject the packets.
pub fn run_video_send_pipeline(
    frame_rx: std::sync::mpsc::Receiver<RawFrame>,
    control_rx: std::sync::mpsc::Receiver<VideoPipelineControl>,
    event_tx: std::sync::mpsc::Sender<VideoPipelineEvent>,
    socket: Arc<UdpSocket>,
    sender_id: String,
    config: EncoderConfig,
    target_fps: u32,
) {
    // Initialize encoder
    let mut encoder = match H264Encoder::new(&config) {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VideoPipelineEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.send(VideoPipelineEvent::Started);
    eprintln!("[video-send] Pipeline started, target {}fps", target_fps);

    let mut frame_id: u32 = 0;
    let frame_interval = Duration::from_secs_f64(1.0 / target_fps as f64);
    let mut last_frame_time = Instant::now();

    // Cache the last frame for re-sending during idle periods (e.g. PipeWire
    // damage-based capture sends nothing when the screen is static). Re-sending
    // at a low rate keeps keyframes flowing so new viewers can join.
    let mut last_frame: Option<RawFrame> = None;
    let repeat_interval = Duration::from_millis(500);

    // Thumbnail generation: every 5 seconds, encode a JPEG from the raw frame
    let thumbnail_interval = Duration::from_secs(5);
    let mut last_thumbnail_time = Instant::now() - thumbnail_interval; // generate first one immediately

    loop {
        // Check control messages
        match control_rx.try_recv() {
            Ok(VideoPipelineControl::Shutdown) => break,
            Ok(VideoPipelineControl::ForceKeyframe) => {
                encoder.force_keyframe();
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        // Frame rate limiting: skip frames that arrive faster than target
        let now = Instant::now();
        if now.duration_since(last_frame_time) < frame_interval {
            match frame_rx.recv_timeout(Duration::from_millis(1)) {
                Ok(_frame) => continue, // drop frame, too soon
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Receive frame — or repeat the last frame if idle too long
        let frame = match frame_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(f) => {
                last_frame = Some(RawFrame {
                    data: f.data.clone(),
                    width: f.width,
                    height: f.height,
                    stride: f.stride,
                    pixel_format: f.pixel_format,
                    timestamp_us: f.timestamp_us,
                });
                f
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // No new frame — re-send last frame if idle long enough
                if last_frame_time.elapsed() >= repeat_interval {
                    if let Some(ref cached) = last_frame {
                        RawFrame {
                            data: cached.data.clone(),
                            width: cached.width,
                            height: cached.height,
                            stride: cached.stride,
                            pixel_format: cached.pixel_format,
                            timestamp_us: cached.timestamp_us,
                        }
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        last_frame_time = Instant::now();

        // Generate thumbnail periodically
        if last_frame_time.duration_since(last_thumbnail_time) >= thumbnail_interval {
            last_thumbnail_time = last_frame_time;
            if let Some(jpeg) = frame_to_jpeg_thumbnail(&frame) {
                eprintln!("[video-send] Thumbnail generated: {} bytes", jpeg.len());
                let _ = event_tx.send(VideoPipelineEvent::ThumbnailReady(jpeg));
            }
        }

        // Encode — dispatch based on pixel format
        let encode_result = match frame.pixel_format {
            PixelFormat::NV12 => {
                encoder.encode_nv12_frame(&frame.data, frame.width, frame.height)
            }
            PixelFormat::BGRA | PixelFormat::RGBA => {
                let is_bgra = frame.pixel_format == PixelFormat::BGRA;
                encoder.encode_bgra_frame(
                    &frame.data, frame.width, frame.height, frame.stride, is_bgra,
                )
            }
        };

        match encode_result {
            Ok(Some(encoded)) => {
                // Packetize: split encoded data into UDP_MAX_PAYLOAD-sized chunks
                let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
                let total = chunks.len() as u16;

                if frame_id % 30 == 0 {
                    eprintln!("[video-send] Frame {} encoded: {} bytes, {} packets, keyframe={}",
                        frame_id, encoded.data.len(), total, encoded.is_keyframe);
                }

                let mut send_ok = 0u32;
                let mut send_err = 0u32;
                for (i, chunk) in chunks.iter().enumerate() {
                    let pkt = UdpVideoPacket::new(
                        &sender_id,
                        frame_id,
                        i as u16,
                        total,
                        encoded.is_keyframe,
                        chunk,
                    );
                    match socket.send(&pkt.to_bytes()) {
                        Ok(_) => send_ok += 1,
                        Err(e) => {
                            if send_err == 0 {
                                eprintln!("[video-send] UDP send error on pkt {}/{}: {}", i, total, e);
                            }
                            send_err += 1;
                        }
                    }
                    // Pace large frames to avoid overwhelming the server's UDP buffer.
                    if total > 10 && i % 10 == 9 {
                        std::thread::sleep(Duration::from_micros(500));
                    }
                }
                if encoded.is_keyframe || send_err > 0 {
                    eprintln!("[video-send] Frame {} sent: {}/{} ok, {} errors, keyframe={}",
                        frame_id, send_ok, total, send_err, encoded.is_keyframe);
                }
                frame_id = frame_id.wrapping_add(1);
            }
            Ok(None) => {} // Encoder still buffering (startup)
            Err(e) => {
                eprintln!("[video-send] Encode error: {}", e);
                let _ = event_tx.send(VideoPipelineEvent::Error(format!("Encode: {}", e)));
            }
        }
    }

    // Flush encoder
    for encoded in encoder.flush() {
        let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
        let total = chunks.len() as u16;
        for (i, chunk) in chunks.iter().enumerate() {
            let pkt = UdpVideoPacket::new(&sender_id, frame_id, i as u16, total, encoded.is_keyframe, chunk);
            let _ = socket.send(&pkt.to_bytes());
        }
        frame_id = frame_id.wrapping_add(1);
    }

    let _ = event_tx.send(VideoPipelineEvent::Stopped);
}
