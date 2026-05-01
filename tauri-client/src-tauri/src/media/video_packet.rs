/// Rust equivalents of the C++ structs in src/common/udp_packet.hpp.
/// Must be byte-compatible for UDP relay through the C++ community server.

pub const SENDER_ID_SIZE: usize = 32;
/// Max bytes of codec data per UDP video packet. Sized to fit comfortably
/// within typical PPPoE / VPN / tunnel MTUs (1492 - 28 IP/UDP - 45 packet
/// header = 1419 max). 1200 mirrors Discord's choice and gives ~200 bytes
/// of margin against the most aggressive consumer-network MTU shrinkage.
/// Was 1400 through 0.5.3.
pub const UDP_MAX_PAYLOAD: usize = 1200;
pub const NACK_MAX_ENTRIES: usize = 64;

pub const PACKET_TYPE_VIDEO: u8 = 1;
pub const PACKET_TYPE_KEYFRAME_REQUEST: u8 = 2;
pub const PACKET_TYPE_NACK: u8 = 3;
pub const PACKET_TYPE_FEC: u8 = 4;

pub const FEC_GROUP_SIZE: u16 = 5;

pub const CODEC_UNKNOWN: u8 = 0;
pub const CODEC_H264_HW: u8 = 1;
pub const CODEC_H264_SW: u8 = 2;
pub const CODEC_H265: u8 = 3;
pub const CODEC_AV1: u8 = 4;

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpVideoPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub frame_id: u32,
    pub packet_index: u16,
    pub total_packets: u16,
    pub payload_size: u16,
    pub is_keyframe: bool,
    pub codec: u8,
    pub payload: [u8; UDP_MAX_PAYLOAD],
}

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpKeyframeRequest {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub target_username: [u8; SENDER_ID_SIZE],
}

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpNackPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub target_username: [u8; SENDER_ID_SIZE],
    pub frame_id: u32,
    pub nack_count: u16,
    pub missing_indices: [u16; NACK_MAX_ENTRIES],
}

/// XOR-based Forward Error Correction packet. One FEC packet per group of
/// `FEC_GROUP_SIZE` video packets. If exactly 1 packet in the group is lost,
/// the receiver can reconstruct it by XOR-ing the FEC payload with all other
/// received packets in the group.
#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpFecPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub frame_id: u32,
    pub group_start: u16,
    pub group_count: u16,
    pub payload_size_xor: u16,
    pub payload: [u8; UDP_MAX_PAYLOAD],
}

fn fill_id(dest: &mut [u8; SENDER_ID_SIZE], src: &str) {
    let bytes = src.as_bytes();
    let len = bytes.len().min(SENDER_ID_SIZE);
    dest[..len].copy_from_slice(&bytes[..len]);
}

impl UdpVideoPacket {
    /// Construct a video packet with an explicit codec byte. Used by the
    /// video send pipeline so the per-packet codec field reflects the
    /// active encoder rather than always the H.264_HW default.
    pub fn new_with_codec(
        sender_id_str: &str,
        frame_id: u32,
        packet_index: u16,
        total_packets: u16,
        is_keyframe: bool,
        codec: u8,
        data: &[u8],
    ) -> Self {
        let mut pkt = Self::new(sender_id_str, frame_id, packet_index, total_packets, is_keyframe, data);
        pkt.codec = codec;
        pkt
    }

    pub fn new(
        sender_id_str: &str,
        frame_id: u32,
        packet_index: u16,
        total_packets: u16,
        is_keyframe: bool,
        data: &[u8],
    ) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        fill_id(&mut sender_id, sender_id_str);

        let mut payload = [0u8; UDP_MAX_PAYLOAD];
        let data_len = data.len().min(UDP_MAX_PAYLOAD);
        payload[..data_len].copy_from_slice(&data[..data_len]);

        UdpVideoPacket {
            packet_type: PACKET_TYPE_VIDEO,
            sender_id,
            frame_id,
            packet_index,
            total_packets,
            payload_size: data_len as u16,
            is_keyframe,
            codec: CODEC_H264_HW,
            payload,
        }
    }

    /// Serialize to a compact byte vector: header (45 bytes) + actual payload only.
    /// This saves hundreds of bytes per packet vs the old fixed 1445-byte format.
    /// The C++ server broadcasts `bytes_recvd` so compact packets relay correctly.
    pub fn to_bytes(&self) -> Vec<u8> {
        let ps = { self.payload_size } as usize;
        let header_size = std::mem::size_of::<Self>() - UDP_MAX_PAYLOAD; // 45 bytes
        let total = header_size + ps;
        let mut buf = vec![0u8; total];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                header_size,
            );
        }
        buf[header_size..header_size + ps].copy_from_slice(&self.payload[..ps]);
        buf
    }

    /// Deserialize from bytes. Accepts both compact (header + payload) and full-size buffers.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        let header_size = std::mem::size_of::<Self>() - UDP_MAX_PAYLOAD; // 45 bytes
        if buf.len() < header_size {
            return None;
        }
        let mut pkt = Self {
            packet_type: 0,
            sender_id: [0; SENDER_ID_SIZE],
            frame_id: 0,
            packet_index: 0,
            total_packets: 0,
            payload_size: 0,
            is_keyframe: false,
            codec: 0,
            payload: [0; UDP_MAX_PAYLOAD],
        };
        // Copy header fields
        unsafe {
            std::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                &mut pkt as *mut Self as *mut u8,
                header_size,
            );
        }
        // Copy payload (however many bytes are available, up to payload_size)
        let ps = { pkt.payload_size } as usize;
        let available = buf.len() - header_size;
        let copy_len = ps.min(available).min(UDP_MAX_PAYLOAD);
        pkt.payload[..copy_len].copy_from_slice(&buf[header_size..header_size + copy_len]);
        Some(pkt)
    }

    pub fn sender_username(&self) -> String {
        let end = self.sender_id.iter().position(|&b| b == 0).unwrap_or(SENDER_ID_SIZE);
        String::from_utf8_lossy(&self.sender_id[..end]).to_string()
    }

    pub fn payload_data(&self) -> &[u8] {
        // Copy from packed struct to avoid unaligned access
        let size = { self.payload_size } as usize;
        &self.payload[..size]
    }
}

impl UdpKeyframeRequest {
    pub fn new(sender_id_str: &str, target: &str) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        fill_id(&mut sender_id, sender_id_str);
        let mut target_username = [0u8; SENDER_ID_SIZE];
        fill_id(&mut target_username, target);

        UdpKeyframeRequest {
            packet_type: PACKET_TYPE_KEYFRAME_REQUEST,
            sender_id,
            target_username,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let total = std::mem::size_of::<Self>();
        let mut buf = vec![0u8; total];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                total,
            );
        }
        buf
    }
}

impl UdpFecPacket {
    pub fn new(
        sender_id_str: &str,
        frame_id: u32,
        group_start: u16,
        group_count: u16,
        payload_size_xor: u16,
        xor_payload: &[u8; UDP_MAX_PAYLOAD],
    ) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        fill_id(&mut sender_id, sender_id_str);

        UdpFecPacket {
            packet_type: PACKET_TYPE_FEC,
            sender_id,
            frame_id,
            group_start,
            group_count,
            payload_size_xor,
            payload: *xor_payload,
        }
    }

    /// Serialize to compact bytes: header + actual max payload.
    /// FEC payloads are always UDP_MAX_PAYLOAD (XOR of zero-padded payloads),
    /// so the compact form is the full struct size.
    pub fn to_bytes(&self) -> Vec<u8> {
        let total = std::mem::size_of::<Self>();
        let mut buf = vec![0u8; total];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                total,
            );
        }
        buf
    }
}

impl UdpNackPacket {
    pub fn new(sender_id_str: &str, target: &str, frame_id: u32, missing: &[u16]) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        fill_id(&mut sender_id, sender_id_str);
        let mut target_username = [0u8; SENDER_ID_SIZE];
        fill_id(&mut target_username, target);

        let count = missing.len().min(NACK_MAX_ENTRIES);
        let mut missing_indices = [0u16; NACK_MAX_ENTRIES];
        missing_indices[..count].copy_from_slice(&missing[..count]);

        UdpNackPacket {
            packet_type: PACKET_TYPE_NACK,
            sender_id,
            target_username,
            frame_id,
            nack_count: count as u16,
            missing_indices,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let total = std::mem::size_of::<Self>();
        let mut buf = vec![0u8; total];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                total,
            );
        }
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_packet_roundtrip() {
        let data = b"hello video frame";
        let pkt = UdpVideoPacket::new("testuser", 42, 0, 3, true, data);
        let bytes = pkt.to_bytes();
        let decoded = UdpVideoPacket::from_bytes(&bytes).unwrap();
        // Copy fields from packed struct to avoid unaligned reference UB
        let ptype = decoded.packet_type;
        let fid = decoded.frame_id;
        let pidx = decoded.packet_index;
        let total = decoded.total_packets;
        let kf = decoded.is_keyframe;
        let codec = decoded.codec;
        let psize = decoded.payload_size;
        assert_eq!(ptype, PACKET_TYPE_VIDEO);
        assert_eq!(fid, 42);
        assert_eq!(pidx, 0);
        assert_eq!(total, 3);
        assert!(kf);
        assert_eq!(codec, CODEC_H264_HW);
        assert_eq!(psize, data.len() as u16);
        assert_eq!(decoded.payload_data(), data);
        assert_eq!(decoded.sender_username(), "testuser");
    }

    #[test]
    fn video_packet_size_matches_cpp() {
        // C++ struct: 1 + 32 + 4 + 2 + 2 + 2 + 1 + 1 + UDP_MAX_PAYLOAD
        // = 45 + UDP_MAX_PAYLOAD bytes. Server uses bytes_recvd for
        // relay so the actual wire size (header + payload_size) is what
        // matters per packet; this test guards against header drift.
        const VIDEO_HEADER_SIZE: usize = 45;
        assert_eq!(std::mem::size_of::<UdpVideoPacket>(), VIDEO_HEADER_SIZE + UDP_MAX_PAYLOAD);
    }

    #[test]
    fn video_packet_compact_serialization() {
        let data = b"hello video frame";
        let pkt = UdpVideoPacket::new("testuser", 42, 0, 3, true, data);
        let bytes = pkt.to_bytes();
        // Header (45 bytes) + payload (17 bytes) = 62 bytes, NOT 1445
        let header_size = std::mem::size_of::<UdpVideoPacket>() - UDP_MAX_PAYLOAD;
        assert_eq!(bytes.len(), header_size + data.len());
        // Should still round-trip correctly
        let decoded = UdpVideoPacket::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.payload_data(), data);
        assert_eq!(decoded.sender_username(), "testuser");
    }

    #[test]
    fn keyframe_request_roundtrip() {
        let req = UdpKeyframeRequest::new("viewer1", "streamer1");
        let bytes = req.to_bytes();
        assert_eq!(bytes[0], PACKET_TYPE_KEYFRAME_REQUEST);
        let sender = String::from_utf8_lossy(&bytes[1..8]).trim_matches('\0').to_string();
        assert_eq!(sender, "viewer1");
    }

    #[test]
    fn fec_packet_size_matches_cpp() {
        // C++ struct: 1 + 32 + 4 + 2 + 2 + 2 + UDP_MAX_PAYLOAD
        // = 43 + UDP_MAX_PAYLOAD bytes. FEC payload is always full
        // UDP_MAX_PAYLOAD (XOR of zero-padded packet payloads).
        const FEC_HEADER_SIZE: usize = 43;
        assert_eq!(std::mem::size_of::<UdpFecPacket>(), FEC_HEADER_SIZE + UDP_MAX_PAYLOAD);
    }

    #[test]
    fn fec_packet_roundtrip() {
        let mut xor_payload = [0u8; UDP_MAX_PAYLOAD];
        xor_payload[0] = 0xAB;
        xor_payload[1] = 0xCD;
        let pkt = UdpFecPacket::new("streamer1", 42, 0, 5, 1234, &xor_payload);
        let bytes = pkt.to_bytes();
        assert_eq!(bytes[0], PACKET_TYPE_FEC);
        assert_eq!(bytes.len(), std::mem::size_of::<UdpFecPacket>());
        // Verify sender_id at offset 1
        let sender = String::from_utf8_lossy(&bytes[1..10]).trim_matches('\0').to_string();
        assert_eq!(sender, "streamer1");
    }

    #[test]
    fn nack_packet_stores_missing_indices() {
        let missing = vec![2u16, 5, 7];
        let nack = UdpNackPacket::new("viewer1", "streamer1", 100, &missing);
        let count = nack.nack_count;
        let m0 = nack.missing_indices[0];
        let m1 = nack.missing_indices[1];
        let m2 = nack.missing_indices[2];
        assert_eq!(count, 3);
        assert_eq!(m0, 2);
        assert_eq!(m1, 5);
        assert_eq!(m2, 7);
    }
}
