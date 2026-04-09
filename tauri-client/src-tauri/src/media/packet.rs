pub const PACKET_TYPE_AUDIO: u8 = 0;
pub const PACKET_TYPE_VIDEO: u8 = 1;
pub const PACKET_TYPE_KEYFRAME_REQUEST: u8 = 2;
pub const PACKET_TYPE_NACK: u8 = 3;
pub const PACKET_TYPE_STREAM_AUDIO: u8 = 6;
pub const PACKET_TYPE_PING: u8 = 5;
/// Fixed-size packet length (legacy — kept for recv buffer sizing only).
pub const PACKET_TOTAL_SIZE: usize = 1437;
pub const SENDER_ID_SIZE: usize = 32;
pub const MAX_PAYLOAD_SIZE: usize = 1400;
/// Minimum header size: 1 (type) + 32 (sender_id) + 2 (sequence) + 2 (payload_size) = 37
pub const AUDIO_HEADER_SIZE: usize = 1 + SENDER_ID_SIZE + 2 + 2;

#[derive(Debug)]
pub struct UdpAudioPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub sequence: u16,
    pub payload_size: u16,
    pub payload: [u8; MAX_PAYLOAD_SIZE],
}

impl UdpAudioPacket {
    pub fn new_audio(sender_id_str: &str, sequence: u16, opus_data: &[u8]) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let bytes = sender_id_str.as_bytes();
        let len = bytes.len().min(SENDER_ID_SIZE);
        sender_id[..len].copy_from_slice(&bytes[..len]);

        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        let data_len = opus_data.len().min(MAX_PAYLOAD_SIZE);
        payload[..data_len].copy_from_slice(&opus_data[..data_len]);

        UdpAudioPacket {
            packet_type: PACKET_TYPE_AUDIO,
            sender_id,
            sequence,
            payload_size: data_len as u16,
            payload,
        }
    }

    pub fn new_stream_audio(sender_id_str: &str, sequence: u16, opus_data: &[u8]) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let bytes = sender_id_str.as_bytes();
        let len = bytes.len().min(SENDER_ID_SIZE);
        sender_id[..len].copy_from_slice(&bytes[..len]);

        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        let data_len = opus_data.len().min(MAX_PAYLOAD_SIZE);
        payload[..data_len].copy_from_slice(&opus_data[..data_len]);

        UdpAudioPacket {
            packet_type: PACKET_TYPE_STREAM_AUDIO,
            sender_id,
            sequence,
            payload_size: data_len as u16,
            payload,
        }
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
        let ps = (payload_size as usize).min(MAX_PAYLOAD_SIZE);

        // Ensure the buffer actually contains the claimed payload
        if buf.len() < AUDIO_HEADER_SIZE + ps {
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

    pub fn sender_username(&self) -> String {
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
