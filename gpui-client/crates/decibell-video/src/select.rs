//! Which decoder a stream gets.
//!
//! The platform says which decoders the machine could use, best first ([`Order`]; on Linux:
//! Vulkan Video on the renderer's device, then NVDEC on NVIDIA or VA-API elsewhere, then
//! software). A stream's decoder is the first of them that opens and decodes. One that fails to
//! open, or fails within its first [`FIRST_FRAMES`] pictures, is not tried again for that codec
//! for the rest of the session, by any stream: the reason is kept ([`Selector::report`]) and
//! logged once. A decoder that fails later only loses the stream it failed on.
//!
//! `DECIBELL_VIDEO_DECODER=vulkan|nvdec|vaapi|software` (or a comma-separated list) replaces
//! the order: exactly those decoders are tried, nothing else, software included. For tests and
//! for checking one path on a machine. `DECIBELL_VIDEO=software` is the older spelling of
//! `DECIBELL_VIDEO_DECODER=software` (and also leaves the renderer's device alone).

use std::sync::Arc;

use parking_lot::Mutex;

use crate::codec::{Codec, DecodeBackend, SkippedBackend};

/// A decoder that fails before it has published this many pictures never worked for the
/// codec: nobody tries it again this session. Half a second of a 60 fps stream.
pub(crate) const FIRST_FRAMES: u64 = 30;

/// Decoders to try, in order. At most one of each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Order {
    list: [DecodeBackend; 4],
    len: usize,
}

impl Order {
    pub const SOFTWARE: Order = Order {
        list: [DecodeBackend::Software; 4],
        len: 1,
    };

    pub fn new(backends: &[DecodeBackend]) -> Order {
        let mut order = Order {
            list: [DecodeBackend::Software; 4],
            len: 0,
        };
        for &backend in backends {
            if !order.as_slice().contains(&backend) {
                order.list[order.len] = backend;
                order.len += 1;
            }
        }
        order
    }

    pub fn as_slice(&self) -> &[DecodeBackend] {
        &self.list[..self.len]
    }

    /// `vulkan,nvdec`: the names of [`DecodeBackend::label`]. `None` for anything else.
    pub fn parse(names: &str) -> Option<Order> {
        let backends: Option<Vec<DecodeBackend>> =
            names.split(',').map(DecodeBackend::from_label).collect();
        backends
            .filter(|backends| !backends.is_empty())
            .map(|backends| Order::new(&backends))
    }
}

/// The order the environment forces, if it does.
pub(crate) fn forced_by_environment() -> Option<Order> {
    if let Ok(names) = std::env::var("DECIBELL_VIDEO_DECODER") {
        match Order::parse(&names) {
            Some(order) => return Some(order),
            None => log::warn!(
                "video: DECIBELL_VIDEO_DECODER={names:?} is not vulkan, nvdec, vaapi or software (or a list of them); ignored"
            ),
        }
    }
    std::env::var("DECIBELL_VIDEO")
        .is_ok_and(|value| value == "software")
        .then_some(Order::SOFTWARE)
}

#[derive(Default)]
pub(crate) struct Selector {
    /// Why a decoder is not tried for a codec any more, by `[codec][backend]`.
    skipped: Mutex<[[Option<Arc<str>>; 4]; 3]>,
    forced: Mutex<Option<Order>>,
}

impl Selector {
    pub fn new(forced: Option<Order>) -> Self {
        Self {
            skipped: Mutex::default(),
            forced: Mutex::new(forced),
        }
    }

    pub fn force(&self, order: Option<Order>) {
        *self.forced.lock() = order;
    }

    pub fn forced(&self) -> Option<Order> {
        *self.forced.lock()
    }

    /// The decoders to try on a machine whose own order is `platform`.
    pub fn order(&self, platform: Order) -> Order {
        self.forced().unwrap_or(platform)
    }

    /// Records that `backend` does not work for `codec`. False: it was known already.
    pub fn skip(&self, codec: Codec, backend: DecodeBackend, reason: &str) -> bool {
        let mut skipped = self.skipped.lock();
        let entry = &mut skipped[codec.index()][backend.index()];
        if entry.is_some() {
            return false;
        }
        *entry = Some(reason.into());
        drop(skipped);
        log::warn!(
            "video: {} decode with {} does not work here and is not tried again this session: {reason}",
            codec.label(),
            backend.label(),
        );
        true
    }

    pub fn skipped(&self, codec: Codec, backend: DecodeBackend) -> Option<Arc<str>> {
        self.skipped.lock()[codec.index()][backend.index()].clone()
    }

    /// Every decoder given up on this session, with the reason.
    pub fn report(&self) -> Vec<SkippedBackend> {
        let skipped = self.skipped.lock();
        let mut report = Vec::new();
        for codec in Codec::ALL {
            for backend in DecodeBackend::ALL {
                if let Some(reason) = &skipped[codec.index()][backend.index()] {
                    report.push(SkippedBackend {
                        codec,
                        backend,
                        reason: reason.clone(),
                    });
                }
            }
        }
        report
    }

    /// Forgets every failure (tests; a new GPU).
    pub fn forget(&self) {
        *self.skipped.lock() = Default::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_keeps_each_decoder_once_and_parses_names() {
        use DecodeBackend::*;
        assert_eq!(
            Order::new(&[VulkanVideo, Nvdec, VulkanVideo, Software]).as_slice(),
            [VulkanVideo, Nvdec, Software]
        );
        assert_eq!(Order::parse("vaapi").unwrap().as_slice(), [Vaapi]);
        assert_eq!(
            Order::parse("nvdec, software").unwrap().as_slice(),
            [Nvdec, Software]
        );
        assert_eq!(Order::parse("vdpau"), None);
        assert_eq!(Order::parse(""), None);
    }

    #[test]
    fn a_skip_is_remembered_once_per_codec_and_decoder() {
        let selector = Selector::default();
        assert!(selector.skip(Codec::Av1, DecodeBackend::VulkanVideo, "no AV1 decode"));
        assert!(!selector.skip(Codec::Av1, DecodeBackend::VulkanVideo, "again"));
        assert_eq!(
            selector
                .skipped(Codec::Av1, DecodeBackend::VulkanVideo)
                .as_deref(),
            Some("no AV1 decode")
        );
        assert!(selector.skipped(Codec::Hevc, DecodeBackend::VulkanVideo).is_none());
        let report = selector.report();
        assert_eq!(report.len(), 1);
        assert_eq!(
            (report[0].codec, report[0].backend),
            (Codec::Av1, DecodeBackend::VulkanVideo)
        );
        selector.forget();
        assert!(selector.report().is_empty());
    }

    #[test]
    fn a_forced_order_replaces_the_platforms() {
        use DecodeBackend::*;
        let platform = Order::new(&[VulkanVideo, Vaapi, Software]);
        let selector = Selector::default();
        assert_eq!(selector.order(platform), platform);
        selector.force(Order::parse("vaapi"));
        assert_eq!(selector.order(platform).as_slice(), [Vaapi]);
    }
}
