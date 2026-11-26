/// Rust equivalents of the C++ structs in src/common/udp_packet.hpp.
/// Must be byte-compatible for UDP relay through the C++ community server.

/// Tag prepended to HEVC/AV1 keyframe payloads when the sender ships a
/// separate decoder description (hvcC / av1C). Receivers strip this
/// prefix so `frame.data` is just bitstream and `frame.description`
/// carries the WebCodecs decoder configuration record.
/// Wire layout when present: `[4 magic][u32 BE: desc_len][desc][bitstream]`.
/// Bit-identical to tauri-client's encoder so old and new streamers
/// interop.
pub const WIRE_DESCRIPTION_MAGIC: [u8; 4] = [0xDE, 0xC1, 0xBE, 0x11];

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
    /// Keyframe flag on the wire. Stored as a raw byte, not a Rust `bool`:
    /// `from_bytes` copies this straight off an untrusted datagram, and a
    /// `bool` holding any value other than 0/1 is instant undefined
    /// behavior. Read it through `is_keyframe()`.
    pub is_keyframe: u8,
    pub codec: u8,
    pub payload: [u8; UDP_MAX_PAYLOAD],
}

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpKeyframeRequest {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub target_id: [u8; SENDER_ID_SIZE],
}

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpNackPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub target_id: [u8; SENDER_ID_SIZE],
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

/// Bytes before a VIDEO / VIDEO_SEALED packet's payload.
pub const VIDEO_HEADER_SIZE: usize = std::mem::size_of::<UdpVideoPacket>() - UDP_MAX_PAYLOAD;

/// A VIDEO / VIDEO_SEALED datagram read in place: its header fields and a
/// borrow of its payload, so the receive loop copies a fragment's bytes only
/// once, into the frame being reassembled (`UdpVideoPacket::from_bytes`
/// copies the whole datagram into the struct first). Parsed by the same
/// rules as `from_bytes`.
#[derive(Clone, Copy)]
pub struct VideoFragment<'a> {
    pub sender_id: &'a [u8; SENDER_ID_SIZE],
    pub frame_id: u32,
    pub packet_index: u16,
    pub total_packets: u16,
    pub is_keyframe: bool,
    pub codec: u8,
    pub payload: &'a [u8],
}

impl<'a> VideoFragment<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < VIDEO_HEADER_SIZE {
            return None;
        }
        let u16_at = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
        // Like from_bytes: however many payload bytes arrived, up to
        // payload_size and UDP_MAX_PAYLOAD.
        let size = (u16_at(41) as usize)
            .min(buf.len() - VIDEO_HEADER_SIZE)
            .min(UDP_MAX_PAYLOAD);
        Some(VideoFragment {
            sender_id: buf[1..1 + SENDER_ID_SIZE].try_into().ok()?,
            frame_id: u32::from_le_bytes(buf[33..37].try_into().ok()?),
            packet_index: u16_at(37),
            total_packets: u16_at(39),
            is_keyframe: buf[43] != 0,
            codec: buf[44],
            payload: &buf[VIDEO_HEADER_SIZE..VIDEO_HEADER_SIZE + size],
        })
    }
}

/// A FEC datagram read in place (see [`VideoFragment`]). Only a full-size one
/// parses: a shorter datagram would leave a zero-filled payload tail that
/// silently corrupts XOR recovery.
#[derive(Clone, Copy)]
pub struct FecFragment<'a> {
    pub sender_id: &'a [u8; SENDER_ID_SIZE],
    pub frame_id: u32,
    pub group_start: u16,
    pub group_count: u16,
    pub payload_size_xor: u16,
    pub payload: &'a [u8; UDP_MAX_PAYLOAD],
}

impl<'a> FecFragment<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < std::mem::size_of::<UdpFecPacket>() {
            return None;
        }
        let u16_at = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
        Some(FecFragment {
            sender_id: buf[1..1 + SENDER_ID_SIZE].try_into().ok()?,
            frame_id: u32::from_le_bytes(buf[33..37].try_into().ok()?),
            group_start: u16_at(37),
            group_count: u16_at(39),
            payload_size_xor: u16_at(41),
            payload: buf[43..43 + UDP_MAX_PAYLOAD].try_into().ok()?,
        })
    }
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
            is_keyframe: is_keyframe as u8,
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
            is_keyframe: 0,
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
        // Clamp the size field to what was actually copied. A datagram
        // claiming payload_size > UDP_MAX_PAYLOAD (or longer than the
        // datagram itself) would otherwise survive with the lie intact
        // and panic payload_data()'s slice on the recv thread.
        pkt.payload_size = copy_len as u16;
        Some(pkt)
    }

    pub fn sender_id_str(&self) -> String {
        let end = self.sender_id.iter().position(|&b| b == 0).unwrap_or(SENDER_ID_SIZE);
        String::from_utf8_lossy(&self.sender_id[..end]).to_string()
    }

    /// Allocation-free comparison against an already-known sender id —
    /// the recv loop checks the streamer per packet (up to ~2000/sec),
    /// which must not cost a String per datagram.
    pub fn sender_id_matches(&self, other: &str) -> bool {
        let end = self.sender_id.iter().position(|&b| b == 0).unwrap_or(SENDER_ID_SIZE);
        self.sender_id[..end] == *other.as_bytes()
    }

    pub fn payload_data(&self) -> &[u8] {
        // Copy from packed struct to avoid unaligned access
        let size = { self.payload_size } as usize;
        &self.payload[..size]
    }

    /// Wire-safe keyframe flag. The field is a raw byte off the network,
    /// so it is stored as `u8` and normalized to a `bool` here.
    pub fn is_keyframe(&self) -> bool {
        self.is_keyframe != 0
    }

    /// This packet as the receive path's borrowed view.
    pub fn fragment(&self) -> VideoFragment<'_> {
        VideoFragment {
            sender_id: &self.sender_id,
            frame_id: { self.frame_id },
            packet_index: { self.packet_index },
            total_packets: { self.total_packets },
            is_keyframe: self.is_keyframe(),
            codec: self.codec,
            payload: self.payload_data(),
        }
    }
}

impl UdpKeyframeRequest {
    pub fn new(sender_id_str: &str, target: &str) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        fill_id(&mut sender_id, sender_id_str);
        let mut target_id = [0u8; SENDER_ID_SIZE];
        fill_id(&mut target_id, target);

        UdpKeyframeRequest {
            packet_type: PACKET_TYPE_KEYFRAME_REQUEST,
            sender_id,
            target_id,
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

    /// This packet as the receive path's borrowed view.
    pub fn fragment(&self) -> FecFragment<'_> {
        FecFragment {
            sender_id: &self.sender_id,
            frame_id: { self.frame_id },
            group_start: { self.group_start },
            group_count: { self.group_count },
            payload_size_xor: { self.payload_size_xor },
            payload: &self.payload,
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
        let mut target_id = [0u8; SENDER_ID_SIZE];
        fill_id(&mut target_id, target);

        let count = missing.len().min(NACK_MAX_ENTRIES);
        let mut missing_indices = [0u16; NACK_MAX_ENTRIES];
        missing_indices[..count].copy_from_slice(&missing[..count]);

        UdpNackPacket {
            packet_type: PACKET_TYPE_NACK,
            sender_id,
            target_id,
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
        let kf = decoded.is_keyframe();
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
        assert_eq!(decoded.sender_id_str(), "testuser");
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
        assert_eq!(decoded.sender_id_str(), "testuser");
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

    #[test]
    fn from_bytes_clamps_lying_payload_size() {
        // A datagram whose header claims payload_size=65535 but only
        // carries 4 bytes. Before the clamp this survived parsing with
        // the lie intact and panicked payload_data()'s slice on the
        // recv thread — remotely triggerable by anything that can hit
        // the media port.
        let pkt = UdpVideoPacket::new("streamer1", 1, 0, 1, false, b"data");
        let mut raw = pkt.to_bytes();
        // payload_size lives at offset 41..43 (packed, little-endian):
        // type(1) + sender_id(32) + frame_id(4) + index(2) + total(2).
        raw[41] = 0xFF;
        raw[42] = 0xFF;

        let parsed = UdpVideoPacket::from_bytes(&raw).expect("parses");
        let ps = { parsed.payload_size };
        assert_eq!(ps, 4); // clamped to what actually arrived
        assert_eq!(parsed.payload_data(), b"data"); // and no panic
    }

    /// The receive loop's in-place views read exactly what the struct copies
    /// read, lying sizes and short datagrams included.
    #[test]
    fn fragment_views_match_the_struct_parsers() {
        let pkt = UdpVideoPacket::new_with_codec("4242", 0xA1B2_C3D4, 7, 9, true, 3, b"payload bytes");
        let mut cases = vec![pkt.to_bytes()];
        // payload_size lies high (clamped to what arrived) and above the cap.
        let mut lying = pkt.to_bytes();
        lying[41] = 0xFF;
        lying[42] = 0x00;
        cases.push(lying.clone());
        lying.extend(std::iter::repeat_n(0xEE, 2000));
        cases.push(lying);
        cases.push(pkt.to_bytes()[..VIDEO_HEADER_SIZE].to_vec());
        for bytes in &cases {
            let a = UdpVideoPacket::from_bytes(bytes).unwrap();
            let b = VideoFragment::parse(bytes).unwrap();
            let f = a.fragment();
            for v in [b, f] {
                assert_eq!(v.sender_id, &a.sender_id);
                assert_eq!(v.frame_id, { a.frame_id });
                assert_eq!(v.packet_index, { a.packet_index });
                assert_eq!(v.total_packets, { a.total_packets });
                assert_eq!(v.is_keyframe, a.is_keyframe());
                assert_eq!(v.codec, a.codec);
                assert_eq!(v.payload, a.payload_data());
            }
        }
        assert!(VideoFragment::parse(&cases[0][..VIDEO_HEADER_SIZE - 1]).is_none());

        let mut xor = [0u8; UDP_MAX_PAYLOAD];
        xor[0] = 0xAB;
        xor[UDP_MAX_PAYLOAD - 1] = 0xCD;
        let fec = UdpFecPacket::new("4242", 42, 5, 5, 1234, &xor);
        let bytes = fec.to_bytes();
        let v = FecFragment::parse(&bytes).unwrap();
        assert_eq!(v.sender_id, &fec.sender_id);
        assert_eq!((v.frame_id, v.group_start, v.group_count, v.payload_size_xor), (42, 5, 5, 1234));
        assert_eq!(v.payload, &xor);
        let f = fec.fragment();
        assert_eq!((f.frame_id, f.group_start, f.group_count, f.payload_size_xor), (42, 5, 5, 1234));
        assert!(FecFragment::parse(&bytes[..bytes.len() - 1]).is_none());
    }

    #[test]
    fn sender_id_matches_is_alloc_free_equivalent() {
        let pkt = UdpVideoPacket::new("alice", 1, 0, 1, false, b"x");
        assert!(pkt.sender_id_matches("alice"));
        assert!(!pkt.sender_id_matches("alicia"));
        assert!(!pkt.sender_id_matches("ali"));
        assert!(!pkt.sender_id_matches(""));
        assert_eq!(pkt.sender_id_str(), "alice");
    }
}
