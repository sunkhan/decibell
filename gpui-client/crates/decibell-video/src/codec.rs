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

/// What this machine does with one codec: the answer to "which codecs can this client decode"
/// (the renderer's `probeDecoders`), from the driver and the linked FFmpeg, not from a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecSupport {
    pub codec: Codec,
    /// `None`: no decoder at all; the client must not advertise the codec.
    pub path: Option<DecodePath>,
    /// The largest picture the hardware decoder takes (the driver's answer). `None` for
    /// software and where the driver does not say.
    pub max_size: Option<(u32, u32)>,
}
