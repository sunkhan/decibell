pub const PACKET_TYPE_AUDIO: u8 = 0;
pub const PACKET_TYPE_VIDEO: u8 = 1;
pub const PACKET_TYPE_KEYFRAME_REQUEST: u8 = 2;
pub const PACKET_TYPE_NACK: u8 = 3;
pub const PACKET_TYPE_STREAM_AUDIO: u8 = 6;
pub const PACKET_TYPE_PING: u8 = 5;
/// Largest audio datagram (header + MAX_PAYLOAD_SIZE) — sizes the voice
/// receive buffer.
pub const PACKET_TOTAL_SIZE: usize = AUDIO_HEADER_SIZE + MAX_PAYLOAD_SIZE;
pub const SENDER_ID_SIZE: usize = 32;
/// Max audio payload per datagram. Same cap as video (`video_packet.rs
/// UDP_MAX_PAYLOAD`) and the C++ header (`src/common/udp_packet.hpp`) since
/// 0.7.9 — was 1400. A 20 ms Opus frame is a few hundred bytes at any
/// configured bitrate (the encoder's output buffer is `MAX_PAYLOAD_SIZE - 1`
/// so a frame can never exceed it, and bitrates are clamped to 320 kbps),
/// and the sealed P2P envelope keeps every datagram under 1280 bytes.
pub const MAX_PAYLOAD_SIZE: usize = 1200;
/// Minimum header size: 1 (type) + 32 (sender_id) + 2 (sequence) + 2 (payload_size) = 37
pub const AUDIO_HEADER_SIZE: usize = 1 + SENDER_ID_SIZE + 2 + 2;

/// Relay return-routability (review S11; `src/common/udp_packet.hpp`
/// `UdpBindChallenge` / `UdpBindEcho`). The community relay binds our voice
/// and media endpoints only once the address has proven it can receive: a
/// keyed datagram from an unbound address (the first PING on joining, or
/// anything after a NAT rebinding) is answered with a BIND_CHALLENGE, and
/// a BIND_ECHO of its cookie from the same socket binds it. `MediaSocket`
/// answers on the Plain (relay) path; sealed P2P sockets never see one.
pub const PACKET_TYPE_BIND_CHALLENGE: u8 = 10;
pub const PACKET_TYPE_BIND_ECHO: u8 = 11;
pub const BIND_COOKIE_SIZE: usize = 8;
/// type + 32-byte id field + cookie, challenge and echo alike (41 bytes):
/// the echo is never bigger than the challenge that asked for it.
pub const BIND_PACKET_SIZE: usize = 1 + SENDER_ID_SIZE + BIND_COOKIE_SIZE;

/// The BIND_ECHO answering `challenge`, or `None` when it isn't a
/// well-formed challenge. The echo is the challenge with its type flipped
/// and our UDP key (`self_id`, NUL-padded) in the id field. No allocation.
pub fn bind_echo_for(challenge: &[u8], self_id: &[u8; SENDER_ID_SIZE]) -> Option<[u8; BIND_PACKET_SIZE]> {
    if challenge.len() != BIND_PACKET_SIZE || challenge[0] != PACKET_TYPE_BIND_CHALLENGE {
        return None;
    }
    let mut echo = [0u8; BIND_PACKET_SIZE];
    echo[0] = PACKET_TYPE_BIND_ECHO;
    echo[1..1 + SENDER_ID_SIZE].copy_from_slice(self_id);
    echo[1 + SENDER_ID_SIZE..].copy_from_slice(&challenge[1 + SENDER_ID_SIZE..]);
    Some(echo)
}

#[derive(Debug)]
pub struct UdpAudioPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub sequence: u16,
    pub payload_size: u16,
    pub payload: [u8; MAX_PAYLOAD_SIZE],
}

impl UdpAudioPacket {
    /// Build an audio-class packet. Returns `None` when `data` exceeds
    /// `MAX_PAYLOAD_SIZE` — an oversize frame is dropped by the caller
    /// rather than silently truncated into a frame the decoder can't use
    /// (the pre-0.7.9 constructors clamped).
    fn new_with(packet_type: u8, sender_id_str: &str, sequence: u16, data: &[u8]) -> Option<Self> {
        if data.len() > MAX_PAYLOAD_SIZE {
            return None;
        }
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let bytes = sender_id_str.as_bytes();
        let len = bytes.len().min(SENDER_ID_SIZE);
        sender_id[..len].copy_from_slice(&bytes[..len]);

        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        payload[..data.len()].copy_from_slice(data);

        Some(UdpAudioPacket {
            packet_type,
            sender_id,
            sequence,
            payload_size: data.len() as u16,
            payload,
        })
    }

    pub fn new_audio(sender_id_str: &str, sequence: u16, opus_data: &[u8]) -> Option<Self> {
        Self::new_with(PACKET_TYPE_AUDIO, sender_id_str, sequence, opus_data)
    }

    /// Audio-class packet of an explicit type — the sealed twins
    /// (`frame_crypto::PACKET_TYPE_AUDIO_SEALED` / `_STREAM_AUDIO_SEALED`)
    /// share this header with the plain types.
    pub fn new_typed(packet_type: u8, sender_id_str: &str, sequence: u16, data: &[u8]) -> Option<Self> {
        Self::new_with(packet_type, sender_id_str, sequence, data)
    }

    pub fn new_stream_audio(sender_id_str: &str, sequence: u16, opus_data: &[u8]) -> Option<Self> {
        Self::new_with(PACKET_TYPE_STREAM_AUDIO, sender_id_str, sequence, opus_data)
    }

    pub fn new_ping(sender_id_str: &str, timestamp_ns: u64) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let bytes = sender_id_str.as_bytes();
        let len = bytes.len().min(SENDER_ID_SIZE);
        sender_id[..len].copy_from_slice(&bytes[..len]);

        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        payload[..8].copy_from_slice(&timestamp_ns.to_le_bytes());

        UdpAudioPacket {
            packet_type: PACKET_TYPE_PING,
            sender_id,
            sequence: 0,
            payload_size: 8,
            payload,
        }
    }

    /// Serialize to a compact byte vector: header (37 bytes) + actual payload only.
    /// This saves ~1300 bytes per voice packet vs the old fixed 1437-byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let ps = self.payload_size as usize;
        let total = AUDIO_HEADER_SIZE + ps;
        let mut buf = vec![0u8; total];
        buf[0] = self.packet_type;
        buf[1..33].copy_from_slice(&self.sender_id);
        buf[33..35].copy_from_slice(&self.sequence.to_le_bytes());
        buf[35..37].copy_from_slice(&self.payload_size.to_le_bytes());
        buf[37..37 + ps].copy_from_slice(&self.payload[..ps]);
        buf
    }

    /// Parse from a variable-length buffer (minimum AUDIO_HEADER_SIZE = 37 bytes).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < AUDIO_HEADER_SIZE {
            return None;
        }
        let packet_type = buf[0];
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        sender_id.copy_from_slice(&buf[1..33]);
        let sequence = u16::from_le_bytes([buf[33], buf[34]]);
        let payload_size = u16::from_le_bytes([buf[35], buf[36]]);
        let ps = payload_size as usize;

        // A payload over the cap (a legacy sender at an absurd bitrate, or a
        // forged header) is refused outright — clamping it would hand the
        // decoder a truncated frame. Likewise if the datagram is shorter than
        // the header claims.
        if ps > MAX_PAYLOAD_SIZE || buf.len() < AUDIO_HEADER_SIZE + ps {
            return None;
        }

        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        payload[..ps].copy_from_slice(&buf[37..37 + ps]);

        Some(UdpAudioPacket {
            packet_type,
            sender_id,
            sequence,
            payload_size,
            payload,
        })
    }

    pub fn sender_id_str(&self) -> String {
        let end = self
            .sender_id
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(SENDER_ID_SIZE);
        String::from_utf8_lossy(&self.sender_id[..end]).to_string()
    }

    pub fn payload_data(&self) -> &[u8] {
        &self.payload[..self.payload_size as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_cap_matches_video_and_rejects_oversize() {
        assert_eq!(MAX_PAYLOAD_SIZE, super::super::video_packet::UDP_MAX_PAYLOAD);
        assert_eq!(PACKET_TOTAL_SIZE, 37 + 1200);
        let max = vec![0xABu8; MAX_PAYLOAD_SIZE];
        let pkt = UdpAudioPacket::new_audio("alice", 3, &max).expect("max-size frame fits");
        let bytes = pkt.to_bytes();
        assert_eq!(bytes.len(), PACKET_TOTAL_SIZE);
        let back = UdpAudioPacket::from_bytes(&bytes).unwrap();
        assert_eq!(back.payload_data(), &max[..]);
        assert_eq!(back.sequence, 3);
        assert_eq!(back.sender_id_str(), "alice");
        // One byte over: refused, never clamped.
        let over = vec![0u8; MAX_PAYLOAD_SIZE + 1];
        assert!(UdpAudioPacket::new_audio("alice", 4, &over).is_none());
        assert!(UdpAudioPacket::new_stream_audio("alice", 4, &over).is_none());
        // Receive side: a header claiming more than the cap is rejected, not
        // clamped — even when the datagram really is that long.
        let mut legacy = bytes.clone();
        legacy.extend_from_slice(&[0u8; 200]);
        let claimed = (MAX_PAYLOAD_SIZE + 200) as u16;
        legacy[35..37].copy_from_slice(&claimed.to_le_bytes());
        assert!(UdpAudioPacket::from_bytes(&legacy).is_none());
        // ...and a short datagram with an honest header still fails cleanly.
        assert!(UdpAudioPacket::from_bytes(&bytes[..bytes.len() - 1]).is_none());
    }

    fn challenge(cookie: [u8; BIND_COOKIE_SIZE]) -> [u8; BIND_PACKET_SIZE] {
        let mut c = [0u8; BIND_PACKET_SIZE];
        c[0] = PACKET_TYPE_BIND_CHALLENGE;
        c[1 + SENDER_ID_SIZE..].copy_from_slice(&cookie);
        c
    }

    #[test]
    fn bind_echo_carries_key_and_cookie() {
        // Mirrors the C++ static_asserts in udp_packet.hpp.
        assert_eq!(BIND_PACKET_SIZE, 41);
        // A PING (the usual trigger) is big enough to earn a challenge.
        assert!(UdpAudioPacket::new_ping("k", 1).to_bytes().len() >= BIND_PACKET_SIZE);
        let mut key = [0u8; SENDER_ID_SIZE];
        key[..31].copy_from_slice(b"abcdefghijklmnopqrstuvwxyz01234");
        let cookie = [1, 2, 3, 4, 5, 6, 7, 8];
        let echo = bind_echo_for(&challenge(cookie), &key).expect("well-formed challenge");
        assert_eq!(echo.len(), BIND_PACKET_SIZE);
        assert_eq!(echo[0], PACKET_TYPE_BIND_ECHO);
        assert_eq!(&echo[1..33], &key[..]);
        assert_eq!(&echo[33..41], &cookie[..]);
    }

    #[test]
    fn bind_echo_refuses_anything_else() {
        let key = [b'k'; SENDER_ID_SIZE];
        let good = challenge([9; BIND_COOKIE_SIZE]);
        // wrong type
        let mut wrong = good;
        wrong[0] = PACKET_TYPE_PING;
        assert!(bind_echo_for(&wrong, &key).is_none());
        // an echo is not a challenge (no ping-pong between two clients)
        let echo = bind_echo_for(&good, &key).unwrap();
        assert!(bind_echo_for(&echo, &key).is_none());
        // short / long
        assert!(bind_echo_for(&good[..BIND_PACKET_SIZE - 1], &key).is_none());
        let mut long = good.to_vec();
        long.push(0);
        assert!(bind_echo_for(&long, &key).is_none());
        assert!(bind_echo_for(&[], &key).is_none());
    }
}
