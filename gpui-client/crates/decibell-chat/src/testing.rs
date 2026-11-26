//! Rows for unit tests.

use std::sync::Arc;

use decibell_model::chat::Message;

/// A stored message: `id`, sender uid, text, unix seconds.
pub fn message(id: i64, sender_uid: i64, content: &str, timestamp: i64) -> Arc<Message> {
    Arc::new(Message {
        id,
        nonce: None,
        channel: "general".into(),
        sender: format!("u{sender_uid}").into(),
        sender_uid,
        content: content.to_string().into(),
        timestamp: Some(timestamp),
        day: decibell_model::chat::time::local_day(timestamp),
        attachments: Arc::from(Vec::new()),
        pending_attachments: Arc::from(Vec::new()),
        edited_at: 0,
        reply: None,
        pending: false,
        sent_after_id: 0,
        encrypted: false,
        decrypt_error: None,
        suppress_embeds: false,
        preview: false,
    })
}
