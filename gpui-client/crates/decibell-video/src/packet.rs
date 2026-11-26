//! One encoded frame's bytes on their way to FFmpeg, owned, never copied.
//!
//! The receive path hands the hub a buffer it owns; [`Packet`] carries that buffer (whatever
//! type it is) into an `AVPacket` as a refcounted `AVBufferRef` whose free callback drops the
//! buffer. If the buffer came from a pool, its `Drop` is what returns it there, so the producer
//! gets its allocation back when FFmpeg lets go of the packet.
//!
//! FFmpeg reads up to [`PADDING`] bytes past the end of a packet (its bitstream readers load
//! whole words), and requires them to be zero. A buffer says how much zeroed memory follows its
//! bytes ([`PacketBytes::zero_padding`]); one that cannot promise [`PADDING`] is copied (one
//! `memcpy` of the encoded frame) into a recycled buffer that can, and handed back at once. That
//! is counted ([`copies`]): it is what a producer's buffer type should make unnecessary.
//!
//! No allocation per packet on our side: the owner sits inline in a small fixed-size carrier,
//! and carriers are recycled.

use std::mem::{MaybeUninit, align_of, size_of};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// `AV_INPUT_BUFFER_PADDING_SIZE`.
pub const PADDING: usize = 64;

/// An owned byte buffer a [`Packet`] can carry. Implemented here for `Vec<u8>`-backed buffers;
/// the app implements it for the core's pooled frame buffer (one line, see the crate docs).
///
/// # Safety
/// `bytes()` must return the same slice (same address) for as long as the value lives, also
/// after the value itself has been moved, and `zero_padding()` bytes directly after it must be
/// readable and zero.
pub unsafe trait PacketBytes: Send + 'static {
    fn bytes(&self) -> &[u8];
    /// How many readable, zeroed bytes follow `bytes()`.
    fn zero_padding(&self) -> usize {
        0
    }
}

/// A `Vec<u8>` with [`PADDING`] zeroed bytes of spare capacity after its contents.
pub struct PaddedVec(Vec<u8>);

impl PaddedVec {
    /// Takes the vector as it is when it has the spare capacity (the usual case for a buffer
    /// that was reserved with it, or reused); otherwise grows it, which may move the bytes
    /// (counted in [`regrows`]).
    pub fn new(mut bytes: Vec<u8>) -> Self {
        if bytes.capacity() - bytes.len() < PADDING {
            REGROWS.fetch_add(1, Ordering::Relaxed);
            bytes.reserve_exact(PADDING);
        }
        // Safety: the spare capacity is at least PADDING bytes.
        unsafe { std::ptr::write_bytes(bytes.as_mut_ptr().add(bytes.len()), 0, PADDING) };
        Self(bytes)
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

// Safety: a Vec's heap block does not move with the Vec, and `new` zeroed PADDING bytes after it.
unsafe impl PacketBytes for PaddedVec {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
    fn zero_padding(&self) -> usize {
        PADDING
    }
}

/// Padded copies of frames whose own buffer had no padding, between uses.
static SPARES: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
const MAX_SPARES: usize = 16;

/// A padded copy that goes back to [`SPARES`] when FFmpeg is done with it.
struct Spare(PaddedVec);

// Safety: as `PaddedVec`.
unsafe impl PacketBytes for Spare {
    fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    fn zero_padding(&self) -> usize {
        PADDING
    }
}

impl Drop for Spare {
    fn drop(&mut self) {
        let bytes = std::mem::take(&mut self.0.0);
        let mut spares = SPARES.lock();
        if spares.len() < MAX_SPARES {
            spares.push(bytes);
        }
    }
}

const INLINE_WORDS: usize = 6;

/// Where a packet's owner lives while FFmpeg holds the packet. Boxed once, then recycled.
struct Carrier {
    owner: MaybeUninit<[usize; INLINE_WORDS]>,
    /// Drops the owner in `owner`. `None`: the carrier is empty.
    drop_owner: Option<unsafe fn(*mut u8)>,
    data: *const u8,
    len: usize,
}

// Safety: the owner is `Send` (`PacketBytes: Send`), and `data` points into it.
unsafe impl Send for Carrier {}

// Boxed: a carrier's address is the `opaque` FFmpeg hands back, so it must not move.
#[allow(clippy::vec_box)]
static CARRIERS: Mutex<Vec<Box<Carrier>>> = Mutex::new(Vec::new());
const MAX_IDLE_CARRIERS: usize = 32;
static COPIES: AtomicU64 = AtomicU64::new(0);
static REGROWS: AtomicU64 = AtomicU64::new(0);

/// Packets that had to be copied because their buffer had no zeroed padding.
pub fn copies() -> u64 {
    COPIES.load(Ordering::Relaxed)
}

/// `Vec<u8>` packets whose vector had to grow for the padding (a possible move of the bytes).
/// 0 when the producer reserves [`PADDING`] spare bytes.
pub fn regrows() -> u64 {
    REGROWS.load(Ordering::Relaxed)
}

/// One encoded frame's bytes, owned, with FFmpeg's padding in place.
pub struct Packet {
    /// `Some` until the packet is dropped or handed to C.
    carrier: Option<Box<Carrier>>,
}

impl Packet {
    /// Carries `owner` without copying its bytes. An owner without [`PADDING`] zeroed bytes
    /// after its contents is copied into a buffer that has them (counted in [`copies`]).
    pub fn new<T: PacketBytes>(owner: T) -> Packet {
        if owner.zero_padding() < PADDING {
            // One memcpy of the encoded frame into a buffer of ours that has the padding. The
            // buffers are recycled, so this path still allocates nothing once warm; the owner
            // goes back to where it came from at once.
            COPIES.fetch_add(1, Ordering::Relaxed);
            let bytes = owner.bytes();
            let mut padded = SPARES.lock().pop().unwrap_or_default();
            padded.clear();
            padded.reserve(bytes.len() + PADDING);
            padded.extend_from_slice(bytes);
            drop(owner);
            return Packet::new(Spare(PaddedVec::new(padded)));
        }
        let recycled = CARRIERS.lock().pop();
        let mut carrier = recycled.unwrap_or_else(|| {
            Box::new(Carrier {
                owner: MaybeUninit::uninit(),
                drop_owner: None,
                data: std::ptr::null(),
                len: 0,
            })
        });
        if size_of::<T>() <= size_of::<[usize; INLINE_WORDS]>()
            && align_of::<T>() <= align_of::<usize>()
        {
            unsafe fn drop_inline<T>(owner: *mut u8) {
                // Safety: `owner` holds a `T` written below.
                unsafe { std::ptr::drop_in_place(owner.cast::<T>()) }
            }
            let slot = carrier.owner.as_mut_ptr().cast::<T>();
            // Safety: the slot is large and aligned enough, and empty.
            let bytes = unsafe {
                slot.write(owner);
                (*slot).bytes()
            };
            carrier.data = bytes.as_ptr();
            carrier.len = bytes.len();
            carrier.drop_owner = Some(drop_inline::<T>);
        } else {
            // An owner too large to sit inline (none of ours is): one box per packet.
            unsafe fn drop_boxed<T>(owner: *mut u8) {
                // Safety: `owner` holds a `Box<T>` written below.
                unsafe { std::ptr::drop_in_place(owner.cast::<Box<T>>()) }
            }
            let boxed = Box::new(owner);
            let bytes = boxed.bytes();
            carrier.data = bytes.as_ptr();
            carrier.len = bytes.len();
            // Safety: a Box is one word.
            unsafe { carrier.owner.as_mut_ptr().cast::<Box<T>>().write(boxed) };
            carrier.drop_owner = Some(drop_boxed::<T>);
        }
        Packet {
            carrier: Some(carrier),
        }
    }

    fn carrier(&self) -> &Carrier {
        self.carrier
            .as_ref()
            .expect("a live packet has its carrier")
    }

    pub fn len(&self) -> usize {
        self.carrier().len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes(&self) -> &[u8] {
        let carrier = self.carrier();
        // Safety: `data` / `len` are the owner's slice, alive while the carrier holds it.
        unsafe { std::slice::from_raw_parts(carrier.data, carrier.len) }
    }

    /// Gives the packet to C: data, length and the `opaque` for [`Packet::free_raw`] (the body
    /// of an `av_buffer_create` free callback).
    pub(crate) fn into_raw(mut self) -> (*mut u8, usize, *mut std::ffi::c_void) {
        let carrier = self.carrier.take().expect("a live packet has its carrier");
        (
            carrier.data as *mut u8,
            carrier.len,
            Box::into_raw(carrier).cast(),
        )
    }

    /// # Safety
    /// `opaque` came from [`Packet::into_raw`] and is not used again.
    pub(crate) unsafe fn free_raw(opaque: *mut std::ffi::c_void) {
        // Safety: see above.
        let carrier = unsafe { Box::from_raw(opaque.cast::<Carrier>()) };
        drop(Packet {
            carrier: Some(carrier),
        });
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        let Some(mut carrier) = self.carrier.take() else {
            return;
        };
        if let Some(drop_owner) = carrier.drop_owner.take() {
            // Safety: the carrier holds the owner `drop_owner` was made for.
            unsafe { drop_owner(carrier.owner.as_mut_ptr().cast()) };
        }
        carrier.data = std::ptr::null();
        carrier.len = 0;
        let mut idle = CARRIERS.lock();
        if idle.len() < MAX_IDLE_CARRIERS {
            idle.push(carrier);
        }
    }
}

/// Today's receive path hands over a `Vec<u8>`: it is taken as it is (its bytes do not move
/// when it has [`PADDING`] bytes of spare capacity).
impl From<Vec<u8>> for Packet {
    fn from(bytes: Vec<u8>) -> Packet {
        Packet::new(PaddedVec::new(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bare(Vec<u8>);

    // Safety: a Vec's heap block does not move with it; no padding is promised.
    unsafe impl PacketBytes for Bare {
        fn bytes(&self) -> &[u8] {
            &self.0
        }
    }

    fn padding_is_zero(packet: &Packet) -> bool {
        // Safety: every packet has PADDING readable bytes after its contents.
        let tail = unsafe {
            std::slice::from_raw_parts(packet.bytes().as_ptr().add(packet.len()), PADDING)
        };
        tail.iter().all(|&byte| byte == 0)
    }

    #[test]
    fn a_vec_with_spare_capacity_is_carried_where_it_is() {
        let mut bytes = Vec::with_capacity(100 + PADDING);
        bytes.extend_from_slice(&[7u8; 100]);
        let address = bytes.as_ptr();
        let packet = Packet::from(bytes);
        assert_eq!(packet.bytes().as_ptr(), address);
        assert_eq!(packet.bytes(), &[7u8; 100]);
        assert!(padding_is_zero(&packet));
    }

    #[test]
    fn an_owner_without_padding_is_copied_and_dropped_at_once() {
        let before = copies();
        let packet = Packet::new(Bare(vec![9u8; 100]));
        assert!(copies() > before);
        assert_eq!(packet.bytes(), &[9u8; 100]);
        assert!(padding_is_zero(&packet));
        // Through the C hand-over and back.
        let (data, len, opaque) = packet.into_raw();
        assert_eq!((unsafe { *data }, len), (9, 100));
        unsafe { Packet::free_raw(opaque) };
    }
}
