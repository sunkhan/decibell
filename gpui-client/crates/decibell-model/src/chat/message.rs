//! One message as the views draw it, channel or DM (the renderer's `Message` and
//! `DmMessage`, which `MessageBubble` already treats as one shape), built once from a core
//! payload and then shared: a slice holds `Arc<Message>`, an edit replaces the `Arc`, and
//! every text is a `SharedString` (an `Arc<str>`), so reading a row never copies.

use std::sync::Arc;

use decibell_attachments::AttachmentKind;
use decibell_core::events::{
    AttachmentPayload, ChannelMessagePayload, DmHistoryMessagePayload, EncryptedAttachmentMetaPayload,
    MessageReceivedPayload,
};
use gpui::SharedString;

use crate::chat::time::local_day;
use crate::ids::Uid;

/// A row's identity in a list (`messageKey`): the server id, else the client nonce of an
/// optimistic send. Stable for the life of the row; the echo of a send is a new row (a
/// nonce key gives way to an id key), as in the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Id(i64),
    Nonce(SharedString),
}

/// Why an encrypted body didn't open (`decrypt_error`); `content` is a placeholder then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecryptError {
    Locked,
    NoKey,
    PeerKey,
    Bad,
}

impl DecryptError {
    pub fn from_wire(text: &str) -> Option<DecryptError> {
        Some(match text {
            "" => return None,
            "locked" => DecryptError::Locked,
            "no_key" => DecryptError::NoKey,
            "peer_key" => DecryptError::PeerKey,
            // Anything else the core may add reads as "malformed" (the bubble's default).
            _ => DecryptError::Bad,
        })
    }
}

/// An attachment of a stored message (`Attachment` in `types/index.ts`). For an encrypted
/// channel's upload the server row is a stand-in; the real metadata and key from the
/// envelope are merged in here (`mapAttachment`), and the key is also registered with the
/// attachment service so its readers decrypt.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub id: i64,
    pub message_id: i64,
    pub kind: AttachmentKind,
    pub filename: SharedString,
    pub mime: SharedString,
    pub size_bytes: i64,
    pub url: SharedString,
    pub position: i32,
    pub created_at: i64,
    /// 0 = present; otherwise when retention removed the file (a tombstone).
    pub purged_at: i64,
    pub width: u32,
    pub height: u32,
    pub thumbnail_size_bytes: u32,
    pub thumbnail_sizes_mask: u32,
    pub duration_ms: u32,
    /// base64 ThumbHash, "" when none.
    pub placeholder: SharedString,
    pub encrypted: bool,
    /// Encrypted, opened: the per-file key (base64) and its chunk size. None for a plain
    /// attachment, or a sealed one whose message didn't open.
    pub key_b64: Option<SharedString>,
    pub chunk_bytes: u32,
}

fn kind_named(kind: &str) -> AttachmentKind {
    match kind {
        "image" => AttachmentKind::Image,
        "video" => AttachmentKind::Video,
        "audio" => AttachmentKind::Audio,
        _ => AttachmentKind::Document,
    }
}

impl Attachment {
    /// `mapAttachment`: the server row, with the envelope's real metadata when sealed.
    pub fn from_payload(a: &AttachmentPayload, metas: &[EncryptedAttachmentMetaPayload]) -> Attachment {
        let mut out = Attachment {
            id: a.id,
            message_id: a.message_id,
            kind: kind_named(&a.kind),
            filename: a.filename.clone().into(),
            mime: a.mime.clone().into(),
            size_bytes: a.size_bytes,
            url: a.url.clone().into(),
            position: a.position,
            created_at: a.created_at,
            purged_at: a.purged_at,
            width: a.width,
            height: a.height,
            thumbnail_size_bytes: a.thumbnail_size_bytes,
            thumbnail_sizes_mask: a.thumbnail_sizes_mask,
            duration_ms: a.duration_ms,
            placeholder: a.placeholder.clone().into(),
            encrypted: false,
            key_b64: None,
            chunk_bytes: 0,
        };
        if !a.encrypted {
            return out;
        }
        out.encrypted = true;
        // Without the envelope's entry (a message that didn't open) the stand-in shows.
        let Some(meta) = metas.iter().find(|m| m.id == a.id) else { return out };
        out.key_b64 = Some(meta.key_b64.clone().into());
        out.chunk_bytes = meta.chunk_bytes;
        out.filename = meta.filename.clone().into();
        out.mime = meta.mime.clone().into();
        out.size_bytes = meta.size_bytes;
        out.width = meta.width;
        out.height = meta.height;
        out.duration_ms = meta.duration_ms;
        out.placeholder = meta.placeholder.clone().into();
        if out.thumbnail_sizes_mask == 0 {
            out.thumbnail_sizes_mask = meta.thumbnail_sizes_mask;
        }
        out
    }
}

/// The parent a message replies to, as the server embedded it. `sender` empty (with an id)
/// = the parent was deleted. A loaded parent wins over this when the view draws it
/// (`ChatPanel.renderBubble`: `parent?.content ?? message.replyToContent`).
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub id: i64,
    pub sender: SharedString,
    pub sender_uid: Uid,
    pub content: SharedString,
    /// The parent's attachment kinds, position order (labels an attachment-only parent).
    pub attachment_kinds: Arc<[AttachmentKind]>,
}

/// An edit as the broadcast carries it (`applyEdit` / `applyDmEdit`). `suppress_embeds`
/// is re-evaluated at edit time for channels; `None` (DMs) keeps the row's.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub content: SharedString,
    pub edited_at: i64,
    pub encrypted: bool,
    pub decrypt_error: Option<DecryptError>,
    pub suppress_embeds: Option<bool>,
}

/// One row. Channel and DM messages share it; fields a kind doesn't use stay empty.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// The server's id; 0 for an optimistic send (or a legacy id-less DM).
    pub id: i64,
    /// Our optimistic send's nonce; also echoed on the stored row (channels: a community
    /// hands the sender its recent nonces back in history).
    pub nonce: Option<SharedString>,
    /// Channel messages and search results: the channel. Empty for DMs.
    pub channel: SharedString,
    /// The name the payload carried; draw `Model::display_name(server, sender_uid, sender)`.
    pub sender: SharedString,
    pub sender_uid: Uid,
    pub content: SharedString,
    /// Unix seconds; None when the payload's didn't parse.
    pub timestamp: Option<i64>,
    /// Local calendar day of `timestamp` as yyyymmdd (0 = none): what the day divider and
    /// grouping compare. Taken once, when the row is built.
    pub day: i32,
    pub attachments: Arc<[Attachment]>,
    /// Optimistic channel sends: the composer's pending uploads this bubble shows.
    pub pending_attachments: Arc<[SharedString]>,
    /// Unix seconds of the last edit; 0 = never ("(edited)").
    pub edited_at: i64,
    pub reply: Option<Arc<Reply>>,
    /// An optimistic send not echoed yet: drawn faded.
    pub pending: bool,
    /// Optimistic sends: the newest stored id in the slice when it was sent (the echo
    /// watchdog and the history merge look above it).
    pub sent_after_id: i64,
    pub encrypted: bool,
    pub decrypt_error: Option<DecryptError>,
    /// The sender lacked EMBED_LINKS: no link previews or GIF media.
    pub suppress_embeds: bool,
    /// DMs: synthesised from the conversation list's preview (its text may be cut); the
    /// first history page replaces it.
    pub preview: bool,
}

impl Message {
    pub fn key(&self) -> RowKey {
        match (&self.nonce, self.id) {
            (_, id) if id > 0 => RowKey::Id(id),
            (Some(nonce), _) => RowKey::Nonce(nonce.clone()),
            // A legacy id-less row without a nonce: unique enough by content, and rare.
            (None, _) => RowKey::Nonce(SharedString::new_static("")),
        }
    }

    /// A row the server stored (`realId > 0`).
    pub fn is_stored(&self) -> bool {
        self.id > 0
    }

    pub fn reply_to(&self) -> i64 {
        self.reply.as_ref().map_or(0, |r| r.id)
    }

    fn base(sender: SharedString, sender_uid: Uid, content: SharedString, timestamp: Option<i64>) -> Message {
        Message {
            id: 0,
            nonce: None,
            channel: SharedString::default(),
            sender,
            sender_uid,
            content,
            day: timestamp.map_or(0, local_day),
            timestamp,
            attachments: Arc::from([]),
            pending_attachments: Arc::from([]),
            edited_at: 0,
            reply: None,
            pending: false,
            sent_after_id: 0,
            encrypted: false,
            decrypt_error: None,
            suppress_embeds: false,
            preview: false,
        }
    }

    /// Our optimistic send (`handleSend`'s bubble): id 0, faded, at the slice's tail.
    pub fn optimistic(
        sender: SharedString,
        sender_uid: Uid,
        content: SharedString,
        now_secs: i64,
        nonce: SharedString,
        reply_to: i64,
        sent_after_id: i64,
    ) -> Message {
        let mut m = Message::base(sender, sender_uid, content, Some(now_secs));
        m.nonce = Some(nonce);
        m.pending = true;
        m.sent_after_id = sent_after_id;
        if reply_to > 0 {
            m.reply = Some(Arc::new(Reply {
                id: reply_to,
                sender: SharedString::default(),
                sender_uid: 0,
                content: SharedString::default(),
                attachment_kinds: Arc::from([]),
            }));
        }
        m
    }

    /// A live channel message or DM (`message_received`). `kinds` maps the wire numbers of
    /// the parent's attachments.
    pub fn from_received(p: MessageReceivedPayload) -> Message {
        let MessageReceivedPayload {
            context,
            sender,
            sender_uid,
            content,
            timestamp,
            id,
            attachments,
            nonce,
            edited_at,
            reply_to,
            reply_to_sender,
            reply_to_sender_uid,
            reply_to_content,
            reply_to_attachment_kinds,
            encrypted,
            decrypt_error,
            encrypted_attachments,
            suppress_embeds,
            ..
        } = p;
        let mut m = Message::base(sender.into(), sender_uid, content.into(), timestamp.trim().parse::<i64>().ok());
        m.id = id.max(0);
        m.nonce = (!nonce.is_empty()).then(|| nonce.into());
        if context != "dm" {
            m.channel = context.into();
        }
        m.attachments = attachments.iter().map(|a| Attachment::from_payload(a, &encrypted_attachments)).collect();
        m.edited_at = edited_at;
        m.reply = reply(reply_to, reply_to_sender, reply_to_sender_uid, reply_to_content, &reply_to_attachment_kinds);
        m.encrypted = encrypted;
        m.decrypt_error = DecryptError::from_wire(&decrypt_error);
        m.suppress_embeds = suppress_embeds;
        m
    }

    /// A stored channel message (a history page, a search result: `mapStoredMessage`).
    pub fn from_stored(p: ChannelMessagePayload) -> Message {
        let ChannelMessagePayload {
            id,
            sender,
            sender_uid,
            channel_id,
            content,
            timestamp,
            attachments,
            nonce,
            edited_at,
            reply_to,
            reply_to_sender,
            reply_to_sender_uid,
            reply_to_content,
            reply_to_attachment_kinds,
            encrypted,
            decrypt_error,
            encrypted_attachments,
            suppress_embeds,
        } = p;
        let mut m = Message::base(sender.into(), sender_uid, content.into(), Some(timestamp));
        m.id = id.max(0);
        m.nonce = (!nonce.is_empty()).then(|| nonce.into());
        m.channel = channel_id.into();
        m.attachments = attachments.iter().map(|a| Attachment::from_payload(a, &encrypted_attachments)).collect();
        m.edited_at = edited_at;
        m.reply = reply(reply_to, reply_to_sender, reply_to_sender_uid, reply_to_content, &reply_to_attachment_kinds);
        m.encrypted = encrypted;
        m.decrypt_error = DecryptError::from_wire(&decrypt_error);
        m.suppress_embeds = suppress_embeds;
        m
    }

    /// A stored DM (`fromHistory`).
    pub fn from_dm_history(p: DmHistoryMessagePayload) -> Message {
        let DmHistoryMessagePayload {
            id,
            sender,
            sender_uid,
            content,
            timestamp,
            edited_at,
            reply_to,
            reply_to_sender,
            reply_to_sender_uid,
            reply_to_content,
            encrypted,
            decrypt_error,
        } = p;
        let mut m = Message::base(sender.into(), sender_uid, content.into(), Some(timestamp));
        m.id = id.max(0);
        m.edited_at = edited_at;
        m.reply = reply(reply_to, reply_to_sender, reply_to_sender_uid, reply_to_content, &[]);
        m.encrypted = encrypted;
        m.decrypt_error = DecryptError::from_wire(&decrypt_error);
        m
    }

    /// The row a conversation preview stands for until its first page (`hydrateConversations`).
    #[allow(clippy::too_many_arguments)]
    pub fn dm_preview(
        id: i64,
        sender: SharedString,
        sender_uid: Uid,
        content: SharedString,
        timestamp: i64,
        encrypted: bool,
        decrypt_error: Option<DecryptError>,
    ) -> Message {
        let mut m = Message::base(sender, sender_uid, content, Some(timestamp));
        m.id = id.max(0);
        m.encrypted = encrypted;
        m.decrypt_error = decrypt_error;
        m.preview = true;
        m
    }

    /// An edit applied (`applyEdit` / `applyDmEdit`): content, edit time and the
    /// encryption facts replaced; everything else kept.
    pub fn edited(&self, edit: &Edit) -> Message {
        let mut m = self.clone();
        m.content = edit.content.clone();
        m.edited_at = edit.edited_at;
        m.encrypted = edit.encrypted;
        m.decrypt_error = edit.decrypt_error;
        if let Some(suppress) = edit.suppress_embeds {
            m.suppress_embeds = suppress;
        }
        m
    }

    /// The same row shown as sent (the echo of it was held during a re-sync).
    pub fn settled(&self) -> Message {
        let mut m = self.clone();
        m.pending = false;
        m
    }
}

fn reply(id: i64, sender: String, sender_uid: Uid, content: String, kinds: &[i32]) -> Option<Arc<Reply>> {
    (id > 0).then(|| {
        Arc::new(Reply {
            id,
            sender: sender.into(),
            sender_uid,
            content: content.into(),
            // Wire numbers → kinds (an unknown number reads as a document).
            attachment_kinds: kinds
                .iter()
                .map(|k| AttachmentKind::from_index(*k as i64).unwrap_or(AttachmentKind::Document))
                .collect(),
        })
    })
}

/// The encrypted attachments of a message, as the attachment service registers them.
pub fn attachment_keys(metas: &[EncryptedAttachmentMetaPayload]) -> Vec<decibell_attachments::EncryptedAttachmentMeta> {
    metas
        .iter()
        .map(|m| decibell_attachments::EncryptedAttachmentMeta {
            id: m.id,
            key_b64: m.key_b64.clone(),
            filename: m.filename.clone(),
            mime: m.mime.clone(),
            size_bytes: m.size_bytes,
            width: m.width,
            height: m.height,
            duration_ms: m.duration_ms,
            placeholder: m.placeholder.clone(),
            chunk_bytes: m.chunk_bytes,
            thumbnail_sizes_mask: m.thumbnail_sizes_mask,
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A stored row for tests: `id`, sender uid, text, unix seconds.
    pub fn stored(id: i64, sender_uid: Uid, content: &str, ts: i64) -> Arc<Message> {
        let mut m = Message::base(format!("u{sender_uid}").into(), sender_uid, content.to_string().into(), Some(ts));
        m.id = id;
        Arc::new(m)
    }

    pub fn bubble(nonce: &str, sender_uid: Uid, content: &str, ts: i64, sent_after_id: i64) -> Arc<Message> {
        Arc::new(Message::optimistic(
            format!("u{sender_uid}").into(),
            sender_uid,
            content.to_string().into(),
            ts,
            nonce.to_string().into(),
            0,
            sent_after_id,
        ))
    }

    pub fn with_nonce(m: &Arc<Message>, nonce: &str) -> Arc<Message> {
        let mut m = (**m).clone();
        m.nonce = Some(nonce.to_string().into());
        Arc::new(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(id: i64, encrypted: bool) -> AttachmentPayload {
        AttachmentPayload {
            id,
            message_id: 9,
            kind: "image".into(),
            filename: "encrypted".into(),
            mime: "application/octet-stream".into(),
            size_bytes: 100,
            url: "u".into(),
            position: 0,
            created_at: 1,
            purged_at: 0,
            width: 0,
            height: 0,
            thumbnail_size_bytes: 0,
            thumbnail_sizes_mask: 0,
            duration_ms: 0,
            placeholder: String::new(),
            encrypted,
        }
    }

    #[test]
    fn sealed_attachments_take_the_envelopes_metadata() {
        let meta = EncryptedAttachmentMetaPayload {
            id: 5,
            key_b64: "k".into(),
            filename: "cat.png".into(),
            mime: "image/png".into(),
            size_bytes: 42,
            width: 10,
            height: 20,
            duration_ms: 0,
            placeholder: "ph".into(),
            chunk_bytes: 65536,
            thumbnail_sizes_mask: 7,
        };
        let a = Attachment::from_payload(&att(5, true), std::slice::from_ref(&meta));
        assert_eq!((&*a.filename, &*a.mime, a.size_bytes, a.width, a.thumbnail_sizes_mask), ("cat.png", "image/png", 42, 10, 7));
        assert_eq!(a.key_b64.as_deref(), Some("k"));
        // No envelope entry: the stand-in, flagged.
        let b = Attachment::from_payload(&att(6, true), &[meta]);
        assert!(b.encrypted && b.key_b64.is_none());
        assert_eq!(&*b.filename, "encrypted");
        let plain = Attachment::from_payload(&att(7, false), &[]);
        assert!(!plain.encrypted);
    }

    #[test]
    fn keys_prefer_the_id_and_fall_back_to_the_nonce() {
        let m = testing::stored(3, 1, "a", 0);
        assert_eq!(m.key(), RowKey::Id(3));
        let b = testing::bubble("n1", 1, "a", 0, 0);
        assert_eq!(b.key(), RowKey::Nonce("n1".into()));
        assert_eq!(testing::with_nonce(&m, "n1").key(), RowKey::Id(3));
        assert_eq!(DecryptError::from_wire("peer_key"), Some(DecryptError::PeerKey));
        assert_eq!(DecryptError::from_wire("weird"), Some(DecryptError::Bad));
        assert_eq!(DecryptError::from_wire(""), None);
    }
}
