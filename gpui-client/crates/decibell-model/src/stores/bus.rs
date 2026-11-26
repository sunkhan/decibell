//! Answers no store keeps: what a renderer view picked up with its own `listen()`
//! (Server Settings' Overview / Audit Log / Storage tabs, the member search, the
//! channel-wipe dialog, a save bar waiting for a refusal).
//!
//! The router emits those events here after the stores have applied them; a view
//! subscribes for as long as it is open:
//! ```ignore
//! self._answers = cx.subscribe(&model.answers, |this, _, event: &CoreEvent, cx| {
//!     if let CoreEvent::StorageInfoReceived(p) = event { this.apply(p, cx) }
//! });
//! ```
//! Only the variants `events::route` sends here arrive (docs/model.md §6 marks them
//! "answers"); high-rate events never do.

use decibell_core::events::CoreEvent;
use gpui::EventEmitter;

#[derive(Default)]
pub struct Answers;

impl EventEmitter<CoreEvent> for Answers {}
