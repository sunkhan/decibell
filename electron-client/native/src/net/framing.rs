use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Largest frame handed on. Servers keep their list and page replies under
/// this (search pages stop at 1.5 MiB).
const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024; // 2MB
/// A frame above `MAX_FRAME_SIZE` but no bigger than this is read and thrown
/// away, keeping the connection (review A1). Treating it as a read error made
/// the client reconnect, ask for the same reply again and fail again, in a
/// loop. A length above this is no reply we asked for: the connection is
/// dropped rather than reading up to 4 GiB of whatever follows.
const MAX_SKIP_SIZE: usize = 64 * 1024 * 1024;
/// Body bytes awaited before an oversized frame is skipped, so the warning
/// can name its packet type: the tag byte plus a varint of up to 10 bytes.
const TYPE_PEEK: usize = 11;

#[derive(Debug, Default)]
pub struct LengthPrefixCodec {
    /// Bytes of an oversized frame still to discard. They are dropped as
    /// they arrive, so a skipped frame never sits in memory.
    skip_remaining: usize,
}

impl LengthPrefixCodec {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The `Packet.type` of a frame, read from its first body bytes: protobuf
/// writes field 1 (a varint, tag 0x08) first, and omits it only for UNKNOWN.
fn peek_packet_type(body: &[u8]) -> Option<i32> {
    if body.first() != Some(&0x08) {
        return None;
    }
    let mut v: u64 = 0;
    for (i, b) in body[1..].iter().take(10).enumerate() {
        v |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some(v as i32);
        }
    }
    None
}

fn packet_type_name(t: Option<i32>) -> String {
    match t {
        Some(t) => match super::proto::packet::Type::try_from(t) {
            Ok(ty) => ty.as_str_name().to_string(),
            Err(_) => format!("type {t}"),
        },
        None => "untyped".to_string(),
    }
}

impl Decoder for LengthPrefixCodec {
    type Item = Vec<u8>;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            if self.skip_remaining > 0 {
                let n = self.skip_remaining.min(src.len());
                src.advance(n);
                self.skip_remaining -= n;
                if self.skip_remaining > 0 {
                    return Ok(None);
                }
            }

            if src.len() < 4 {
                src.reserve(4 - src.len());
                return Ok(None);
            }

            let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;

            if length > MAX_SKIP_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Frame size {} exceeds maximum {}", length, MAX_SKIP_SIZE),
                ));
            }

            if length > MAX_FRAME_SIZE {
                if src.len() < 4 + TYPE_PEEK {
                    return Ok(None);
                }
                log::warn!(
                    "[net] skipping a {} B {} frame (limit {} B); the connection stays up",
                    length,
                    packet_type_name(peek_packet_type(&src[4..4 + TYPE_PEEK])),
                    MAX_FRAME_SIZE
                );
                src.advance(4);
                self.skip_remaining = length;
                // Drop what is already buffered; if the whole frame is
                // here, the next one decodes in this same call.
                continue;
            }

            if src.len() < 4 + length {
                src.reserve(4 + length - src.len());
                return Ok(None);
            }

            src.advance(4);
            let body = src.split_to(length).to_vec();
            return Ok(Some(body));
        }
    }
}

impl Encoder<Vec<u8>> for LengthPrefixCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.put_u32(item.len() as u32);
        dst.put_slice(&item);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_prepends_length() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        let data = vec![1, 2, 3, 4, 5];
        codec.encode(data.clone(), &mut buf).unwrap();

        assert_eq!(buf.len(), 9); // 4 + 5
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 5);
        assert_eq!(&buf[4..], &data[..]);
    }

    #[test]
    fn test_decode_complete_frame() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        let data = vec![10, 20, 30];
        buf.put_u32(3);
        buf.put_slice(&data);

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, Some(data));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_decode_incomplete_header() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(0);
        buf.put_u8(0);

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, None);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn test_decode_incomplete_body() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u32(10);
        buf.put_slice(&[1, 2, 3]);

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, None);
        assert_eq!(buf.len(), 7);
    }

    /// A Packet body of exactly `len` bytes whose type is `ty`: the type
    /// varint first, then filler.
    fn body(ty: u8, len: usize) -> Vec<u8> {
        let mut b = vec![0x08, ty];
        b.resize(len, 0xAB);
        b
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut f = (body.len() as u32).to_be_bytes().to_vec();
        f.extend_from_slice(body);
        f
    }

    /// Feed `stream` to one codec `chunk` bytes at a time, as socket reads
    /// would deliver it, and collect every frame decoded.
    fn decode_in_chunks(stream: &[u8], chunk: usize) -> Result<Vec<Vec<u8>>, std::io::Error> {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        let mut out = Vec::new();
        for piece in stream.chunks(chunk) {
            buf.extend_from_slice(piece);
            while let Some(f) = codec.decode(&mut buf)? {
                out.push(f);
            }
            // A frame being skipped is dropped as it arrives, never kept.
            if codec.skip_remaining > 0 {
                assert!(buf.is_empty());
            }
        }
        Ok(out)
    }

    #[test]
    fn test_decode_skips_oversized_frame_between_normal_ones() {
        let first = body(9, 100);
        let big = body(44, MAX_FRAME_SIZE + 1);
        let last = body(5, 7);
        let mut stream = frame(&first);
        stream.extend(frame(&big));
        stream.extend(frame(&last));
        // Whole, and split across reads of assorted sizes: the oversized
        // frame's header, type peek and tail straddle read boundaries.
        for chunk in [stream.len(), 64 * 1024, 4093, 7, 1] {
            let frames = decode_in_chunks(&stream, chunk).unwrap();
            assert_eq!(frames, vec![first.clone(), last.clone()], "chunk {chunk}");
        }
    }

    #[test]
    fn test_decode_skips_back_to_back_oversized_frames() {
        let big = body(44, MAX_FRAME_SIZE + 10);
        let last = body(5, 3);
        let mut stream = frame(&big);
        stream.extend(frame(&big));
        stream.extend(frame(&last));
        let frames = decode_in_chunks(&stream, stream.len()).unwrap();
        assert_eq!(frames, vec![last]);
    }

    #[test]
    fn test_decode_accepts_frame_at_limit() {
        let at_limit = body(9, MAX_FRAME_SIZE);
        let frames = decode_in_chunks(&frame(&at_limit), 1 << 20).unwrap();
        assert_eq!(frames, vec![at_limit]);
    }

    #[test]
    fn test_decode_rejects_length_above_ceiling() {
        for len in [MAX_SKIP_SIZE as u32 + 1, u32::MAX] {
            let mut codec = LengthPrefixCodec::new();
            let mut buf = BytesMut::new();
            buf.put_u32(len);
            assert!(codec.decode(&mut buf).is_err(), "{len}");
        }
        // Even after a skip: the ceiling holds for every frame.
        let mut stream = frame(&body(44, MAX_FRAME_SIZE + 1));
        stream.extend_from_slice(&(MAX_SKIP_SIZE as u32 + 1).to_be_bytes());
        assert!(decode_in_chunks(&stream, 64 * 1024).is_err());
    }

    #[test]
    fn test_decode_waits_for_type_peek_before_skipping() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u32((MAX_FRAME_SIZE + 1) as u32);
        buf.put_slice(&[0x08, 0x2c]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        // Nothing consumed until the peek is complete.
        assert_eq!(buf.len(), 6);
        assert_eq!(codec.skip_remaining, 0);
        buf.put_slice(&[0; TYPE_PEEK]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        assert!(buf.is_empty());
        assert_eq!(codec.skip_remaining, MAX_FRAME_SIZE + 1 - 2 - TYPE_PEEK);
    }

    #[test]
    fn test_peek_packet_type() {
        assert_eq!(peek_packet_type(&[0x08, 0x05, 0x10]), Some(5));
        // Multi-byte varint: 300 = 0xAC 0x02.
        assert_eq!(peek_packet_type(&[0x08, 0xAC, 0x02]), Some(300));
        // Type omitted (UNKNOWN), or a varint that never ends.
        assert_eq!(peek_packet_type(&[0x10, 0x01]), None);
        assert_eq!(peek_packet_type(&[0x08, 0x80]), None);
        assert_eq!(packet_type_name(Some(5)), "LOGIN_RES");
        assert_eq!(packet_type_name(None), "untyped");
    }

    #[test]
    fn test_roundtrip() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        let original = vec![0xDE, 0xAD, 0xBE, 0xEF];

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_multiple_frames_in_buffer() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();

        codec.encode(vec![1, 2], &mut buf).unwrap();
        codec.encode(vec![3, 4, 5], &mut buf).unwrap();

        let frame1 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame1, vec![1, 2]);

        let frame2 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame2, vec![3, 4, 5]);
    }

    #[test]
    fn test_encode_empty_payload() {
        let mut codec = LengthPrefixCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(vec![], &mut buf).unwrap();

        assert_eq!(buf.len(), 4);
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 0);
    }
}
