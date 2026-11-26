//! Stream playback for the Decibell GPUI client: hardware video decode (H.264, HEVC, AV1) into
//! GPU textures that GPUI draws with no CPU copy, several streams at once, software decode as
//! the fallback. Design, measurements and the GPUI patches behind it:
//! `gpui-client/docs/video-design.md`.
//!
//! # Using it
//!
//! ```ignore
//! // 1. At boot, before the first window (the renderer's Vulkan device is then created with
//! //    a video decode queue; nothing else happens until a stream is displayed):
//! decibell_video::VideoDevice::install();
//!
//! // 2. Once, with the app context: the hub, and how it reaches the core.
//! let hub = VideoHub::global(cx);
//! hub.probe_capabilities(|support| /* advertise the codecs whose `path` is `Some` */);
//! hub.set_callbacks(HubCallbacks {
//!     request_keyframe: Box::new(|key| /* core: request_stream_keyframe / force_keyframe */),
//!     interest: Box::new(|key, on| /* core: set_stream_frame_sink(key, on) */),
//!     event: Box::new(|key, event| /* log, or a toast for FellBackToSoftware */),
//! });
//!
//! // 3. The core's frame sink (called on its receive thread; never blocks). With the `core`
//! //    feature: `decibell_video::enable_frame_pool()` at boot, and
//! hub.push_frame(frame);                     // a decibell_core::events::StreamFrame
//! //    which is
//! hub.push(&frame.key, EncodedFrame {
//!     codec: frame.codec,
//!     keyframe: frame.keyframe,
//!     timestamp: frame.timestamp,
//!     data: Packet::from(frame.data),        // the buffer moves into the packet
//!     description: frame.description,
//!     discontinuity: frame.discontinuity,
//! });
//!
//! // 4. A view that shows a stream holds a viewer and paints it:
//! let viewer = hub.stream(&key).viewer();                 // in the view's state
//! VideoView::new(&viewer).fit(Fit::Cover).rounded(radius) // in render()
//!
//! // 5. When the stream ends: hub.remove(&key).
//! ```
//!
//! # Threads
//!
//! - the caller of [`VideoHub::push`] (the core's video receive thread, or its encoder thread
//!   for our own preview): queues or sheds, never blocks;
//! - `decibell-video-<n>`, one per displayed stream that is receiving frames: decodes, waits for
//!   the GPU, publishes;
//! - the main thread: one `Window::request_present` per burst of new pictures per window, and
//!   the renderer's draw of the scene it already has.
//!
//! # Platforms
//!
//! Linux: Vulkan Video on GPUI's own device first; where that is missing or fails, NVDEC on
//! NVIDIA and VA-API on AMD and Intel; software only when no hardware decoder works. Which one
//! a stream got is in its [`StreamStats`] (`backend`, and `fallback_reason` when it is not the
//! first choice); [`VideoHub::probe_capabilities`] finds out by decoding what to advertise.
//! `DECIBELL_VIDEO_DECODER=vulkan|nvdec|vaapi|software` forces one. Windows: compiles; decodes
//! in software into a counting sink until `gpui_windows` draws surfaces (`video-design.md` §8).

// Without the Vulkan backend (Windows, `no-gpu`) the GPU-facing half of the shared code has
// no user yet.
#![cfg_attr(not(vk), allow(dead_code))]

mod codec;
#[cfg(feature = "core")]
mod core_glue;
mod cpu;
pub mod dbv;
mod decode;
mod hub;
#[cfg(vk)]
mod linux;
mod packet;
mod probe;
mod queue;
mod screens;
mod select;
mod selftests;
mod stats;
mod view;

pub use codec::{Codec, CodecSupport, DecodeBackend, DecodePath, SkippedBackend};
#[cfg(feature = "core")]
pub use core_glue::{POOL_BUFFER_BYTES, POOL_BUFFERS, POOLED_STREAMS, enable_frame_pool};
pub use decode::{LumaRow, barcode};
pub use hub::{
    EncodedFrame, EventCallback, Fault, HubCallbacks, InterestCallback, KeyframeCallback, Stream,
    StreamEvent, VideoHub, Viewer,
};
pub use packet::{
    PADDING, Packet, PacketBytes, PaddedVec, copies as packet_copies, regrows as packet_regrows,
};
pub use screens::screens;
pub use selftests::selftests;
pub use stats::{StatsSampler, StreamRates, StreamStats};
pub use view::{Fit, VideoView};

mod platform {
    use std::sync::Arc;

    use crate::decode::Backend;

    pub(crate) fn backend() -> Arc<dyn Backend> {
        #[cfg(vk)]
        return Arc::new(crate::linux::LinuxBackend::new());
        #[cfg(not(vk))]
        Arc::new(crate::cpu::CpuBackend::default())
    }
}

/// The GPU device video is decoded on: the renderer's own.
pub struct VideoDevice;

impl VideoDevice {
    /// The one call the app makes at boot, **before the first window opens**: registers the
    /// hook that lets GPUI's Vulkan device be created with the Vulkan Video extensions and a
    /// decode queue. Cheap when no stream is ever watched: the device gets two idle queues and
    /// nothing else exists (no FFmpeg context, no thread, no texture) until a displayed stream
    /// receives its first frame. Returns whether the hook was installed (false: not Linux,
    /// `DECIBELL_VIDEO=software`, or a second call).
    ///
    /// The hook also enables, where the GPU has them, the extensions the vendor decoders need
    /// to hand their pictures to the device (external memory and semaphores for NVDEC,
    /// DMA-BUF import with DRM format modifiers for VA-API). Without the hook every stream
    /// decodes in software.
    pub fn install() -> bool {
        // `DECIBELL_VIDEO_FASTPATH=0`: every present writes the whole scene to the GPU again,
        // as before the renderer kept frames (GPUI patch 0007). For measuring the difference.
        #[cfg(vk)]
        if std::env::var("DECIBELL_VIDEO_FASTPATH").is_ok_and(|value| value == "0") {
            gpui_wgpu::set_frame_reuse(false);
        }
        #[cfg(vk)]
        return linux::install_device_hook();
        #[cfg(not(vk))]
        false
    }
}
