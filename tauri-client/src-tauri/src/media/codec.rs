use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Channels, MutSignals, SampleRate};
use std::convert::TryFrom;

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u16 = 1;
pub const FRAME_SIZE: usize = 960; // 20ms at 48kHz mono
pub const MAX_OPUS_FRAME_SIZE: usize = 1400;

pub struct OpusEncoder {
    encoder: Encoder,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, String> {
        let encoder =
            Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)
                .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
        Ok(OpusEncoder { encoder })
    }

    pub fn encode(
        &self,
        pcm: &[i16],
        output: &mut [u8; MAX_OPUS_FRAME_SIZE],
    ) -> Result<usize, String> {
        self.encoder
            .encode(pcm, output)
            .map_err(|e| format!("Opus encode error: {}", e))
    }

    pub fn encode_silence(
        &self,
        output: &mut [u8; MAX_OPUS_FRAME_SIZE],
    ) -> Result<usize, String> {
        let silence = [0i16; FRAME_SIZE];
        self.encode(&silence, output)
    }
}

pub struct OpusDecoder {
    decoder: Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, String> {
        let decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)
            .map_err(|e| format!("Failed to create Opus decoder: {}", e))?;
        Ok(OpusDecoder { decoder })
    }

    pub fn decode(
        &mut self,
        opus_data: &[u8],
        output: &mut [i16; FRAME_SIZE],
    ) -> Result<usize, String> {
        // Packet::try_from fails on empty slices; treat that as packet loss (pass None).
        let packet = if opus_data.is_empty() {
            None
        } else {
            Some(
                Packet::try_from(opus_data)
                    .map_err(|e| format!("Invalid Opus packet: {}", e))?,
            )
        };

        let mut_signals = MutSignals::try_from(output.as_mut_slice())
            .map_err(|e| format!("MutSignals error: {}", e))?;

        self.decoder
            .decode(packet, mut_signals, false)
            .map_err(|e| format!("Opus decode error: {}", e))
    }
}
