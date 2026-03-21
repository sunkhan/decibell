pub const PACKET_TYPE_AUDIO: u8 = 0;
pub const PACKET_TYPE_PING: u8 = 5;
pub const PACKET_TOTAL_SIZE: usize = 1437;
pub const SENDER_ID_SIZE: usize = 32;
pub const MAX_PAYLOAD_SIZE: usize = 1400;

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

    pub fn to_bytes(&self) -> [u8; PACKET_TOTAL_SIZE] {
        let mut buf = [0u8; PACKET_TOTAL_SIZE];
        buf[0] = self.packet_type;
        buf[1..33].copy_from_slice(&self.sender_id);
        buf[33..35].copy_from_slice(&self.sequence.to_le_bytes());
        buf[35..37].copy_from_slice(&self.payload_size.to_le_bytes());
        buf[37..1437].copy_from_slice(&self.payload);
        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < PACKET_TOTAL_SIZE {
            return None;
        }
        let packet_type = buf[0];
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        sender_id.copy_from_slice(&buf[1..33]);
        let sequence = u16::from_le_bytes([buf[33], buf[34]]);
        let payload_size = u16::from_le_bytes([buf[35], buf[36]]);
        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        payload.copy_from_slice(&buf[37..1437]);

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
