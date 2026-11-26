//! Where pictures were painted, for the self-tests (`DECIBELL_TEST_PROBES=1`, as the shell's
//! probes): attachment id → its tile's window bounds. Off, nothing is recorded.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{Bounds, Pixels};

thread_local! {
    static TILES: RefCell<HashMap<i64, Bounds<Pixels>>> = RefCell::new(HashMap::new());
}

pub fn record(attachment_id: i64, bounds: Bounds<Pixels>) {
    if decibell_shell::probes::enabled() {
        TILES.with(|t| t.borrow_mut().insert(attachment_id, bounds));
    }
}

/// Where attachment `id`'s tile was painted last.
pub fn tile(attachment_id: i64) -> Option<Bounds<Pixels>> {
    TILES.with(|t| t.borrow().get(&attachment_id).copied())
}
