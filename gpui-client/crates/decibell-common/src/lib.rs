//! Model-aware widgets every feature uses (`gpui-client/CONVENTIONS.md`, "Shell"):
//!
//! - [`Watch`]: keeps a cached view in step with exactly the stores it reads (docs/model.md §4).
//! - [`UserAvatar`]: picture or letter, the presence interest while drawn, the status dot.
//! - [`display_name`] / [`user_name`]: the names people see (nicknames, renames, "Unknown user").
//! - [`StatusMessageText`], [`presence_of`], [`status_message_of`]: statuses as the renderer reads them.
//! - [`open_user_menu`] / [`open_profile_beside`]: the user menu and profile popup requests.

pub mod open;
pub mod person;
pub mod watch;

pub use open::{BoundsCell, open_profile_at, open_profile_beside, open_user_menu};
pub use person::{
    AvatarDot, NicknameWatch, StatusMessageText, UNKNOWN_USER, UserAvatar, display_name, dot_presence, person_name_color,
    presence_of, role_color, status_line, status_message_of, user_name, want_member,
};
pub use watch::Watch;
