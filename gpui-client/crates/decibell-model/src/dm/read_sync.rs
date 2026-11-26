//! A conversation read on another device of ours (`DM_READ_SYNC`, multi-device design;
//! `features/dm/readSync.ts`). Pure.

use std::sync::Arc;

use crate::chat::message::Message;
use crate::ids::Uid;

/// The unread count once the conversation is read up to `up_to_id`
/// (`unreadAfterReadSync`). Read up to the newest message known here: none. Otherwise,
/// when the loaded rows reach down to the cursor and up to the present, exactly the peer's
/// rows above it (never more than before). Otherwise there's no telling: unchanged.
pub fn unread_after_read_sync(
    unread: i64,
    last_message: Option<&Message>,
    rows: &[Arc<Message>],
    has_more_after: bool,
    up_to_id: i64,
    me: Uid,
) -> i64 {
    if unread <= 0 {
        return 0;
    }
    let mut newest = last_message.map_or(0, |m| m.id);
    let mut covers_cursor = false;
    for m in rows {
        newest = newest.max(m.id);
        if m.id > 0 && m.id <= up_to_id {
            covers_cursor = true;
        }
    }
    if newest > 0 && up_to_id >= newest {
        return 0;
    }
    if !covers_cursor || has_more_after {
        return unread;
    }
    let above = rows.iter().filter(|m| m.id > up_to_id && m.sender_uid != me).count() as i64;
    unread.min(above)
}

/// A live message at or below what we've read (here or on another device) isn't unread.
pub fn already_read(last_read_id: i64, message_id: i64) -> bool {
    message_id > 0 && message_id <= last_read_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::message::testing::stored;

    const ME: Uid = 7;
    const PEER: Uid = 3;

    #[test]
    fn read_sync_arithmetic() {
        let rows: Vec<_> = vec![stored(10, PEER, "a", 0), stored(11, ME, "b", 0), stored(12, PEER, "c", 0), stored(13, PEER, "d", 0)];
        // Read up to the newest: nothing unread.
        assert_eq!(unread_after_read_sync(3, None, &rows, false, 13, ME), 0);
        // The newest known (the sidebar's) is past the rows, but the loaded rows reach the
        // cursor and no peer row of theirs lies above it: counted as they are (none).
        assert_eq!(unread_after_read_sync(3, Some(&stored(20, PEER, "p", 0)), &rows, false, 13, ME), 0);
    }

    #[test]
    fn counts_only_the_peers_rows_above_the_cursor() {
        let rows: Vec<_> = vec![stored(10, PEER, "a", 0), stored(11, ME, "b", 0), stored(12, PEER, "c", 0), stored(13, PEER, "d", 0)];
        assert_eq!(unread_after_read_sync(5, None, &rows, false, 11, ME), 2);
        // Never more than before.
        assert_eq!(unread_after_read_sync(1, None, &rows, false, 11, ME), 1);
        // The cursor below the loaded rows, or a windowed slice: no telling.
        assert_eq!(unread_after_read_sync(5, None, &rows, false, 9, ME), 5);
        assert_eq!(unread_after_read_sync(5, None, &rows, true, 11, ME), 5);
        assert_eq!(unread_after_read_sync(0, None, &rows, false, 11, ME), 0);
        assert!(already_read(12, 12) && !already_read(12, 13) && !already_read(12, 0));
    }
}
