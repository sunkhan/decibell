//! The sealed-attachment format of encrypted text channels, byte for byte as the Electron client
//! writes it (`electron-client/src/features/chat/attachmentCrypto.ts`, WebCrypto) and reads it
//! (`electron-client/electron/main/attachmentCrypto.ts`, node:crypto):
//!
//! ```text
//! content   = ‖ over chunks i of AES-256-GCM(key, nonce_i, plain[i*C .. min((i+1)*C, size)],
//!                                           aad = "decibell-att-v1" ‖ 0x00 ‖ u64le(i)) ‖ tag16
//! nonce_i   = u64le(i) ‖ 00 00 00 01
//! thumbnail = AES-256-GCM(key, u64le(sizePx) ‖ 00 00 00 02, jpeg,
//!                         aad = "decibell-att-v1" ‖ 0x01 ‖ u64le(sizePx)) ‖ tag16
//! ```
//!
//! One random key per file; it travels in the message envelope, never to the server. The nonces
//! are deterministic (safe: the key is single-use), so a retried or resumed upload re-seals to
//! the same bytes, which is what lets [`crate::upload`] resume a sealed upload mid-chunk.
//! `C` (`chunk_bytes`) is carried in the envelope next to the key; uploads always use
//! [`CHUNK_BYTES`], readers honour whatever the envelope says.

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use base64::Engine as _;

use crate::Error;

/// Plaintext bytes per sealed chunk on upload (the envelope's `chunk_bytes`).
pub const CHUNK_BYTES: u64 = 64 * 1024;
/// AES-GCM tag appended to every sealed chunk and thumbnail.
pub const TAG_BYTES: u64 = 16;
/// Largest `chunk_bytes` a reader accepts from an envelope (main's `attachmentKeys.ts`).
pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

const AAD_DOMAIN: &[u8; 15] = b"decibell-att-v1";

/// A per-file AES-256-GCM key. Debug never prints it.
#[derive(Clone)]
pub struct FileKey {
    raw: [u8; 32],
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for FileKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileKey(..)")
    }
}

impl Drop for FileKey {
    fn drop(&mut self) {
        // Best effort: the cipher's own key schedule is the aes crate's business.
        for b in self.raw.iter_mut() {
            // SAFETY: a valid, aligned &mut u8.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

impl FileKey {
    /// A fresh random key (the renderer's `newAttachmentCipher`).
    pub fn generate() -> Self {
        let mut raw = [0u8; 32];
        getrandom::getrandom(&mut raw).expect("OS random source");
        Self::from_bytes(raw)
    }

    pub fn from_bytes(raw: [u8; 32]) -> Self {
        let cipher = Aes256Gcm::new(GenericArray::from_slice(&raw));
        Self { raw, cipher }
    }

    /// From the envelope's `key_b64` (standard base64 of 32 bytes).
    pub fn from_b64(b64: &str) -> Result<Self, Error> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|_| Error::Invalid("attachment key is not base64"))?;
        let raw: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Invalid("attachment key is not 32 bytes"))?;
        Ok(Self::from_bytes(raw))
    }

    /// Standard base64, as the envelope's `key_b64` carries it.
    pub fn to_b64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.raw)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.raw
    }

    /// Seal plaintext chunk `index` in place: `buf` holds the plaintext on entry and
    /// `plaintext ‖ tag` on return (it grows by [`TAG_BYTES`]).
    pub fn seal_chunk_in_place<B: Extend<u8> + AsMut<[u8]>>(&self, index: u64, buf: &mut B) {
        let tag = self
            .cipher
            .encrypt_in_place_detached(&content_nonce(index), &content_aad(index), buf.as_mut())
            .expect("AES-GCM seal of a bounded chunk");
        buf.extend(tag.iter().copied());
    }

    /// Open sealed chunk `index` (`ciphertext ‖ tag`) in place. Returns the plaintext length:
    /// `sealed[..n]` is the plaintext afterwards.
    pub fn open_chunk_in_place(&self, index: u64, sealed: &mut [u8]) -> Result<usize, Error> {
        open_in_place(&self.cipher, &content_nonce(index), &content_aad(index), sealed)
    }

    /// Seal one plaintext chunk into a new buffer.
    pub fn seal_chunk(&self, index: u64, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + TAG_BYTES as usize);
        out.extend_from_slice(plain);
        self.seal_chunk_in_place(index, &mut out);
        out
    }

    /// Seal a JPEG thumbnail of long edge `size_px` (320 / 640 / 1280).
    pub fn seal_thumbnail(&self, size_px: u32, jpeg: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(jpeg.len() + TAG_BYTES as usize);
        out.extend_from_slice(jpeg);
        let tag = self
            .cipher
            .encrypt_in_place_detached(&thumbnail_nonce(size_px), &thumbnail_aad(size_px), &mut out)
            .expect("AES-GCM seal of a thumbnail");
        out.extend_from_slice(&tag);
        out
    }

    /// Open a sealed thumbnail of long edge `size_px`.
    pub fn open_thumbnail(&self, size_px: u32, sealed: &[u8]) -> Result<Vec<u8>, Error> {
        let mut buf = sealed.to_vec();
        let n = open_in_place(&self.cipher, &thumbnail_nonce(size_px), &thumbnail_aad(size_px), &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Seal a whole plaintext (tests and small files; uploads stream chunk by chunk).
    pub fn seal_all(&self, plain: &[u8]) -> Vec<u8> {
        let c = CHUNK_BYTES as usize;
        let mut out = Vec::with_capacity(ciphertext_size(plain.len() as u64, CHUNK_BYTES) as usize);
        for (i, part) in plain.chunks(c).enumerate() {
            let start = out.len();
            out.extend_from_slice(part);
            let tag = self
                .cipher
                .encrypt_in_place_detached(&content_nonce(i as u64), &content_aad(i as u64), &mut out[start..])
                .expect("AES-GCM seal");
            out.extend_from_slice(&tag);
        }
        out
    }

    /// Open a whole sealed file of `size` plaintext bytes (tests and small files).
    pub fn open_all(&self, sealed: &[u8], size: u64, chunk_bytes: u64) -> Result<Vec<u8>, Error> {
        if sealed.len() as u64 != ciphertext_size(size, chunk_bytes) {
            return Err(Error::ShortRead);
        }
        let mut buf = sealed.to_vec();
        let mut out = Vec::with_capacity(size as usize);
        let sc = (chunk_bytes + TAG_BYTES) as usize;
        for (i, part) in buf.chunks_mut(sc).enumerate() {
            let n = self.open_chunk_in_place(i as u64, part)?;
            out.extend_from_slice(&part[..n]);
        }
        Ok(out)
    }
}

fn open_in_place(cipher: &Aes256Gcm, nonce: &Nonce<aes_gcm::aead::consts::U12>, aad: &[u8], sealed: &mut [u8]) -> Result<usize, Error> {
    let tb = TAG_BYTES as usize;
    if sealed.len() < tb {
        return Err(Error::Decrypt);
    }
    let n = sealed.len() - tb;
    let (body, tag) = sealed.split_at_mut(n);
    let tag = *Tag::from_slice(tag);
    cipher.decrypt_in_place_detached(nonce, aad, body, &tag).map_err(|_| Error::Decrypt)?;
    Ok(n)
}

fn content_nonce(index: u64) -> Nonce<aes_gcm::aead::consts::U12> {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(&index.to_le_bytes());
    n[11] = 1;
    *Nonce::from_slice(&n)
}

fn thumbnail_nonce(size_px: u32) -> Nonce<aes_gcm::aead::consts::U12> {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(&(size_px as u64).to_le_bytes());
    n[11] = 2;
    *Nonce::from_slice(&n)
}

fn content_aad(index: u64) -> [u8; 24] {
    let mut a = [0u8; 24];
    a[..15].copy_from_slice(AAD_DOMAIN);
    a[15] = 0;
    a[16..].copy_from_slice(&index.to_le_bytes());
    a
}

fn thumbnail_aad(size_px: u32) -> [u8; 24] {
    let mut a = [0u8; 24];
    a[..15].copy_from_slice(AAD_DOMAIN);
    a[15] = 1;
    a[16..].copy_from_slice(&(size_px as u64).to_le_bytes());
    a
}

/// Number of sealed chunks for a plaintext of `size` bytes.
pub fn chunk_count(size: u64, chunk_bytes: u64) -> u64 {
    if size == 0 {
        0
    } else {
        size.div_ceil(chunk_bytes)
    }
}

/// Ciphertext size of a plaintext of `size` bytes (what the server stores and `init` declares).
pub fn ciphertext_size(size: u64, chunk_bytes: u64) -> u64 {
    size + chunk_count(size, chunk_bytes) * TAG_BYTES
}

/// Ciphertext offset of plaintext chunk `index`.
pub fn ciphertext_offset(index: u64, chunk_bytes: u64) -> u64 {
    index * (chunk_bytes + TAG_BYTES)
}

/// Parse `bytes=a-b` / `bytes=a-` / `bytes=-n` against a plaintext size (main's `parseRange`).
/// `None` for a missing or unsatisfiable header. Returns an inclusive `(start, end)`.
pub fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let h = header.trim();
    let spec = h.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let digits = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    if !digits(a) || !digits(b) || (a.is_empty() && b.is_empty()) {
        return None;
    }
    // Digit runs too long for u64 behave like JS's huge numbers: past the end.
    let num = |s: &str| s.parse::<u64>().unwrap_or(u64::MAX);
    let (start, end) = if a.is_empty() {
        let n = num(b);
        (size.saturating_sub(n), size.checked_sub(1)?)
    } else {
        let start = num(a);
        let end = if b.is_empty() { size.checked_sub(1)? } else { num(b).min(size.checked_sub(1)?) };
        (start, end)
    };
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

/// Which ciphertext to fetch for a plaintext range, and how to trim the opened chunks
/// (main's `planRange`). Ranges are inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangePlan {
    pub first_chunk: u64,
    pub last_chunk: u64,
    /// Inclusive ciphertext range to ask the server for; `None` for an empty file.
    pub upstream: Option<(u64, u64)>,
    /// Plaintext bytes to drop from the start of the first opened chunk.
    pub trim_start: u64,
    /// End (exclusive) of the wanted bytes, counted from the first chunk's first byte.
    pub trim_end: u64,
    pub plain_start: u64,
    pub plain_end: u64,
}

pub fn plan_range(size: u64, chunk_bytes: u64, range: Option<(u64, u64)>) -> RangePlan {
    let (plain_start, plain_end) = range.unwrap_or((0, size.saturating_sub(1)));
    let first_chunk = plain_start / chunk_bytes;
    let last_chunk = plain_end / chunk_bytes;
    let sealed = chunk_bytes + TAG_BYTES;
    let ct_start = first_chunk * sealed;
    let ct_end = ((last_chunk + 1) * sealed).min(ciphertext_size(size, chunk_bytes)).saturating_sub(1);
    RangePlan {
        first_chunk,
        last_chunk,
        upstream: if size == 0 { None } else { Some((ct_start, ct_end)) },
        trim_start: plain_start - first_chunk * chunk_bytes,
        trim_end: plain_end - first_chunk * chunk_bytes + 1,
        plain_start,
        plain_end,
    }
}

/// Sealed length of chunk `index` of a file of `size` plaintext bytes.
pub fn sealed_chunk_len(index: u64, size: u64, chunk_bytes: u64) -> u64 {
    let start = index * chunk_bytes;
    (size - start).min(chunk_bytes) + TAG_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_offsets() {
        assert_eq!(ciphertext_size(0, CHUNK_BYTES), 0);
        assert_eq!(ciphertext_size(1, CHUNK_BYTES), 17);
        assert_eq!(ciphertext_size(65536, CHUNK_BYTES), 65552);
        assert_eq!(ciphertext_size(65537, CHUNK_BYTES), 65537 + 32);
        assert_eq!(ciphertext_offset(3, CHUNK_BYTES), 3 * 65552);
        assert_eq!(sealed_chunk_len(1, 65537, CHUNK_BYTES), 17);
    }

    #[test]
    fn seal_open_roundtrip_and_tamper() {
        let k = FileKey::generate();
        let plain: Vec<u8> = (0..200_000u32).map(|i| (i * 7 + 3) as u8).collect();
        let sealed = k.seal_all(&plain);
        assert_eq!(sealed.len() as u64, ciphertext_size(plain.len() as u64, CHUNK_BYTES));
        assert_eq!(k.open_all(&sealed, plain.len() as u64, CHUNK_BYTES).unwrap(), plain);
        let mut bad = sealed.clone();
        bad[70_000] ^= 1;
        assert!(matches!(k.open_all(&bad, plain.len() as u64, CHUNK_BYTES), Err(Error::Decrypt)));
        // A chunk opened under the wrong index fails (the index is in the nonce and the AAD).
        let mut c1 = sealed[65552..131104].to_vec();
        assert!(k.open_chunk_in_place(0, &mut c1).is_err());
        let t = k.seal_thumbnail(640, b"jpeg bytes");
        assert_eq!(k.open_thumbnail(640, &t).unwrap(), b"jpeg bytes");
        assert!(k.open_thumbnail(320, &t).is_err());
        assert_eq!(FileKey::from_b64(&k.to_b64()).unwrap().as_bytes(), k.as_bytes());
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 10), Some((0, 9)));
        assert_eq!(parse_range("bytes=5-100", 10), Some((5, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=-30", 10), Some((0, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=4-2", 10), None);
        assert_eq!(parse_range("bytes=-", 10), None);
        assert_eq!(parse_range("items=0-1", 10), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
        let p = plan_range(200_000, CHUNK_BYTES, Some((65535, 65536)));
        assert_eq!((p.first_chunk, p.last_chunk), (0, 1));
        assert_eq!(p.upstream, Some((0, 2 * 65552 - 1)));
        assert_eq!((p.trim_start, p.trim_end), (65535, 65537));
        let p = plan_range(65537, CHUNK_BYTES, None);
        assert_eq!(p.upstream, Some((0, 65537 + 32 - 1)));
        assert_eq!(plan_range(0, CHUNK_BYTES, None).upstream, None);
    }
}
