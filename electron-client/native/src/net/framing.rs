use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024; // 2MB

#[derive(Debug)]
pub struct LengthPrefixCodec;

impl Decoder for LengthPrefixCodec {
    type Item = Vec<u8>;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            src.reserve(4 - src.len());
            return Ok(None);
        }

        let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;

        if length > MAX_FRAME_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame size {} exceeds maximum {}", length, MAX_FRAME_SIZE),
            ));
        }

        if src.len() < 4 + length {
            src.reserve(4 + length - src.len());
            return Ok(None);
        }

        src.advance(4);
        let body = src.split_to(length).to_vec();
        Ok(Some(body))
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
        let mut codec = LengthPrefixCodec;
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
        let mut codec = LengthPrefixCodec;
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
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        buf.put_u8(0);
        buf.put_u8(0);

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, None);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn test_decode_incomplete_body() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        buf.put_u32(10);
        buf.put_slice(&[1, 2, 3]);

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, None);
        assert_eq!(buf.len(), 7);
    }

    #[test]
    fn test_decode_rejects_oversized_frame() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        buf.put_u32((MAX_FRAME_SIZE + 1) as u32);

        let result = codec.decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_roundtrip() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        let original = vec![0xDE, 0xAD, 0xBE, 0xEF];

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_multiple_frames_in_buffer() {
        let mut codec = LengthPrefixCodec;
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
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        codec.encode(vec![], &mut buf).unwrap();

        assert_eq!(buf.len(), 4);
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 0);
    }
}
