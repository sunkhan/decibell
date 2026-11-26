//! What a loaded channel slice retains per message, counted exactly: a counting global
//! allocator (this test binary only) around building a 150-row slice the way a history
//! page builds it. Rows shaped like the stack's `#general` (measured there: 74 bytes of
//! text on average, ~5 % with an attachment, ~5 % replies).
//!
//! `cargo test -p decibell-model --test retained -- --nocapture` prints the figures; the
//! assertion keeps them from creeping up unnoticed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};

use decibell_core::events::{AttachmentPayload, ChannelMessagePayload};
use decibell_model::chat::message::Message;
use decibell_model::chat::slice::{MessageSlice, SliceKind};
use decibell_model::ids::ServerId;
use gpui::{AppContext as _, TestAppContext};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size as isize - layout.size() as isize, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const SENDERS: [&str; 6] = ["alice", "bob", "carol", "dave", "erin", "regular_7"];

fn payload(id: i64) -> ChannelMessagePayload {
    let text = "gg nice clip, the stream tonight works fine on wayland now, bitrate ok ".to_string();
    ChannelMessagePayload {
        id,
        sender: SENDERS[(id % 6) as usize].into(),
        sender_uid: id % 6 + 1,
        channel_id: "general".into(),
        content: text[..74.min(text.len())].to_string(),
        timestamp: 1_764_000_000 + id * 40,
        attachments: if id % 20 == 0 {
            vec![AttachmentPayload {
                id: id * 10,
                message_id: id,
                kind: "image".into(),
                filename: "IMG_2041.jpg".into(),
                mime: "image/jpeg".into(),
                size_bytes: 812_000,
                url: format!("/attachments/{}", id * 10),
                position: 0,
                created_at: 1_764_000_000,
                purged_at: 0,
                width: 1600,
                height: 1200,
                thumbnail_size_bytes: 90_000,
                thumbnail_sizes_mask: 7,
                duration_ms: 0,
                placeholder: "1QcSHQRnh493V4dIh4eXh1h4kJUI".into(),
                encrypted: false,
            }]
        } else {
            vec![]
        },
        nonce: String::new(),
        edited_at: if id % 25 == 0 { 1 } else { 0 },
        reply_to: if id % 20 == 7 { id - 3 } else { 0 },
        reply_to_sender: if id % 20 == 7 { "bob".into() } else { String::new() },
        reply_to_sender_uid: if id % 20 == 7 { 2 } else { 0 },
        reply_to_content: if id % 20 == 7 { "the parent, as embedded".into() } else { String::new() },
        reply_to_attachment_kinds: vec![],
        encrypted: false,
        decrypt_error: String::new(),
        encrypted_attachments: vec![],
        suppress_embeds: false,
    }
}

#[gpui::test]
fn bytes_retained_per_message(cx: &mut TestAppContext) {
    const ROWS: i64 = 150;
    let slice = cx.new(|_| MessageSlice::new(SliceKind::Channel { server: ServerId::from_central(1), channel: "general".into() }, 7, false));
    let channel = gpui::SharedString::from("general");
    // Warm the lazily created shared empties and the slice's own buffers.
    slice.update(cx, |s, cx| {
        let warm: Vec<_> = (1..=3).map(|id| Arc::new(Message::from_stored(payload(id)))).collect();
        s.prepend_history(&warm, true, true, 7, cx);
        s.apply_wiped(cx);
    });
    let before = LIVE.load(Ordering::Relaxed);
    slice.update(cx, |s, cx| {
        // As the router builds a page: rows share the slice's channel id.
        let page: Vec<_> = (100..100 + ROWS)
            .map(|id| {
                let mut m = Message::from_stored(payload(id));
                m.channel = channel.clone();
                Arc::new(m)
            })
            .collect();
        s.prepend_history(&page, true, true, 7, cx);
    });
    let after = LIVE.load(Ordering::Relaxed);
    let per_row = (after - before) as f64 / ROWS as f64;
    slice.read_with(cx, |s, _| assert_eq!(s.rows().len(), ROWS as usize));
    println!(
        "RETAINED {} bytes for {ROWS} rows: {per_row:.0} bytes a message (74-byte texts; size_of::<Message>() = {})",
        after - before,
        std::mem::size_of::<Message>()
    );
    // ~200 B the row, ~90 B its text, ~30 B its sender, ~25 B its place and facts, the
    // occasional attachment and reply; the change log adds one op.
    assert!(per_row < 600.0, "{per_row} bytes a message");
}
