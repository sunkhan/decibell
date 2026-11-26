//! The codecs a stream can use, and what this machine can do with each.

/// A stream's codec. The wire's `VideoCodec` (proto/messages.proto) has two H.264 values
/// (hardware- and software-encoded); they are one bitstream to a decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

    /// From the wire's `VideoCodec` byte (`StreamFrame::codec`): 1 and 2 H.264, 3 H.265, 4 AV1.
    pub fn from_wire(codec: u8) -> Option<Codec> {
        match codec {
            1 | 2 => Some(Codec::H264),
            3 => Some(Codec::Hevc),
            4 => Some(Codec::Av1),
            _ => None,
        }
    }

    /// The wire value a client advertises decode support with (H.264 as `H264_HW`, as the
    /// renderer's `decoderProbe.ts` does).
    pub fn wire(self) -> u8 {
        match self {
            Codec::H264 => 1,
            Codec::Hevc => 3,
            Codec::Av1 => 4,
        }
    }

    /// The name the stats overlay shows (the renderer's `codecLabel`).
    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "H.265",
            Codec::Av1 => "AV1",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn from_index(index: usize) -> Option<Codec> {
        Codec::ALL.get(index).copied()
    }
}

/// How a stream is (or would be) decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodePath {
    /// On the GPU, into textures the renderer samples: no picture data touches the CPU.
    Hardware,
    /// On the CPU, one upload per picture.
    Software,
}

impl DecodePath {
    pub fn label(self) -> &'static str {
        match self {
            DecodePath::Hardware => "hardware",
            DecodePath::Software => "software",
        }
    }
}

/// The decoder a stream is (or would be) decoded with. The order of the variants is the order
/// they are tried in: `gpui-client/docs/video-design.md` §0.5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodeBackend {
    /// Vulkan Video on the renderer's own device: every vendor's first choice.
    VulkanVideo,
    /// NVIDIA's NVDEC through CUDA, copied on the GPU into the renderer's textures.
    Nvdec,
    /// VA-API (AMD, Intel), its surfaces imported as DMA-BUFs into the renderer's device.
    Vaapi,
    /// FFmpeg's software decoders / dav1d, one upload per picture.
    Software,
}

impl DecodeBackend {
    pub const ALL: [DecodeBackend; 4] = [
        DecodeBackend::VulkanVideo,
        DecodeBackend::Nvdec,
        DecodeBackend::Vaapi,
        DecodeBackend::Software,
    ];

    pub fn path(self) -> DecodePath {
        match self {
            DecodeBackend::Software => DecodePath::Software,
            _ => DecodePath::Hardware,
        }
    }

    /// The name `DECIBELL_VIDEO_DECODER` takes and the stats print.
    pub fn label(self) -> &'static str {
        match self {
            DecodeBackend::VulkanVideo => "vulkan",
            DecodeBackend::Nvdec => "nvdec",
            DecodeBackend::Vaapi => "vaapi",
            DecodeBackend::Software => "software",
        }
    }

    /// What the client shows for this decoder of `codec` (Settings → Codecs, the stats
    /// overlay): "Vulkan Video", "NVDEC", "VA-API", "Software (dav1d)" / "Software (FFmpeg)".
    pub fn display_name(self, codec: Codec) -> &'static str {
        match (self, codec) {
            (DecodeBackend::VulkanVideo, _) => "Vulkan Video",
            (DecodeBackend::Nvdec, _) => "NVDEC",
            (DecodeBackend::Vaapi, _) => "VA-API",
            (DecodeBackend::Software, Codec::Av1) => "Software (dav1d)",
            (DecodeBackend::Software, _) => "Software (FFmpeg)",
        }
    }

    pub fn from_label(label: &str) -> Option<DecodeBackend> {
        DecodeBackend::ALL
            .into_iter()
            .find(|backend| backend.label() == label.trim())
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn from_index(index: usize) -> Option<DecodeBackend> {
        DecodeBackend::ALL.get(index).copied()
    }
}

/// A decoder that was tried for a codec and is not used: why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedBackend {
    pub codec: Codec,
    pub backend: DecodeBackend,
    pub reason: std::sync::Arc<str>,
}

/// What this machine does with one codec: the answer to "which codecs can this client decode"
/// (the renderer's `probeDecoders`), from the driver and the linked FFmpeg, not from a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecSupport {
    pub codec: Codec,
    /// `None`: no decoder at all; the client must not advertise the codec.
    pub path: Option<DecodePath>,
    /// The decoder a stream of this codec gets (the first of the order that works).
    pub backend: Option<DecodeBackend>,
    /// The answer comes from decoding a test frame on that decoder ([`crate::VideoHub::probe_capabilities`]),
    /// not only from what the driver and the FFmpeg build list.
    pub probed: bool,
    /// The largest picture the hardware decoder takes (the driver's answer). `None` for
    /// software and where the driver does not say.
    pub max_size: Option<(u32, u32)>,
}

impl CodecSupport {
    pub(crate) fn none(codec: Codec) -> CodecSupport {
        CodecSupport {
            codec,
            path: None,
            backend: None,
            probed: false,
            max_size: None,
        }
    }

    /// `self` when `keep`, else "cannot".
    pub(crate) fn filter(self, keep: bool) -> CodecSupport {
        if keep {
            self
        } else {
            CodecSupport::none(self.codec)
        }
    }
}
