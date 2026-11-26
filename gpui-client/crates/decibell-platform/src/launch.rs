//! What a start of the app asks of it beyond opening (`electron/main/index.ts`, `hotkeys.ts`): the
//! `decibell://` links on its command line and a `--hotkey=<action>` from a desktop without a
//! shortcut service. A second start hands this to the running instance ([`crate::instance`]); the
//! first applies its own once the window is up ([`crate::apply`]).

use serde::{Deserialize, Serialize};

/// The command line's requests, validated as Electron's main process validates them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    /// `decibell://` links that passed [`is_valid_deep_link`]; the invite grammar
    /// (`decibell_model::invite_link`) parses them when they are opened.
    pub links: Vec<String>,
    /// `<action>`, `<action>:down` or `<action>:up` ([`parse_hotkey`]).
    pub hotkey: Option<String>,
    /// The desktop's activation token for this start (Wayland's `XDG_ACTIVATION_TOKEN`): with it
    /// the running instance may take the focus, without it the compositor only marks it as
    /// wanting attention.
    pub activation_token: Option<String>,
}

impl Launch {
    /// From the raw arguments: every argument that starts with `decibell://` (Electron's
    /// `findDeepLinkInArgv` keeps the first; a start carries one), and `--hotkey`'s value.
    /// Invalid ones are dropped with a warning, as main drops them.
    pub fn new(links: &[String], hotkey: Option<&str>) -> Launch {
        let mut kept = Vec::new();
        for link in links {
            if is_valid_deep_link(link) {
                kept.push(link.clone());
            } else {
                log::warn!("[deep-link] rejected malformed url");
            }
        }
        let hotkey = hotkey.and_then(|value| {
            let parsed = parse_hotkey(value);
            if parsed.is_none() {
                log::warn!("[hotkeys] ignored --hotkey={value:?}: not an action");
            }
            parsed
        });
        let activation_token = std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| !t.is_empty());
        Launch { links: kept, hotkey, activation_token }
    }

    /// Nothing but an activation (a plain second start).
    pub fn is_plain(&self) -> bool {
        self.links.is_empty() && self.hotkey.is_none()
    }
}

/// Whether an argument looks like one of our links at all (Electron: `a.startsWith("decibell://")`;
/// the scheme is case-insensitive here, as URL schemes are).
pub fn is_link_argument(arg: &str) -> bool {
    arg.get(.."decibell://".len()).is_some_and(|scheme| scheme.eq_ignore_ascii_case("decibell://"))
}

/// Electron's `isValidDeepLink`: a link can be triggered by any web page or any local process, so
/// only a bounded, control-character- and whitespace-free `decibell:` URL goes on. (The invite
/// grammar and the join confirmation still stand between it and joining anything.)
pub fn is_valid_deep_link(url: &str) -> bool {
    if url.len() > 2048 || !is_link_argument(url) {
        return false;
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    // `new URL(url)` parses: an authority-form URL with a non-empty rest.
    url.len() > "decibell://".len()
}

/// The actions a `--hotkey` may name (`hotkeys.ts`' `ACTIONS`).
const ACTIONS: [&str; 7] = [
    "toggle_mute",
    "toggle_deafen",
    "push_to_talk",
    "push_to_mute",
    "leave_voice",
    "answer_call",
    "decline_call",
];
const HOLD_ACTIONS: [&str; 2] = ["push_to_talk", "push_to_mute"];

/// `hotkeys.ts`' `parseHotkeyArg` on one value: `toggle_mute`, `push_to_talk:down` / `:up`; `:up`
/// only for a hold action, a bare hold action meaning down.
pub fn parse_hotkey(value: &str) -> Option<String> {
    let (action, edge) = value.split_once(':').map_or((value, None), |(a, e)| (a, Some(e)));
    if !ACTIONS.contains(&action) {
        return None;
    }
    match edge {
        None | Some("down") => {}
        Some("up") if HOLD_ACTIONS.contains(&action) => {}
        _ => return None,
    }
    Some(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_links_are_bounded_and_clean() {
        assert!(is_valid_deep_link("decibell://invite/ABCD234567"));
        assert!(is_valid_deep_link("DECIBELL://invite/ABCD234567"));
        assert!(is_valid_deep_link("decibell://invite/127.0.0.11:8082/ABCD234567"));
        assert!(!is_valid_deep_link("decibell://"));
        assert!(!is_valid_deep_link("https://example.com"));
        assert!(!is_valid_deep_link("decibell://invite/AB CD"));
        assert!(!is_valid_deep_link("decibell://invite/AB\nCD"));
        assert!(!is_valid_deep_link("decibell://invite/AB\u{7f}CD"));
        assert!(!is_valid_deep_link(&format!("decibell://invite/{}", "A".repeat(2040))));
    }

    #[test]
    fn hotkeys_follow_main_s_rules() {
        assert_eq!(parse_hotkey("toggle_mute").as_deref(), Some("toggle_mute"));
        assert_eq!(parse_hotkey("push_to_talk").as_deref(), Some("push_to_talk"));
        assert_eq!(parse_hotkey("push_to_talk:down").as_deref(), Some("push_to_talk:down"));
        assert_eq!(parse_hotkey("push_to_mute:up").as_deref(), Some("push_to_mute:up"));
        assert_eq!(parse_hotkey("toggle_mute:down").as_deref(), Some("toggle_mute:down"));
        assert_eq!(parse_hotkey("toggle_mute:up"), None);
        assert_eq!(parse_hotkey("push_to_talk:sideways"), None);
        assert_eq!(parse_hotkey("format_disk"), None);
        assert_eq!(parse_hotkey(""), None);
    }

    /// The model's `inject_cli` (what the running instance calls) accepts exactly what passes here.
    #[test]
    fn the_model_agrees() {
        for value in [
            "toggle_mute",
            "toggle_deafen",
            "push_to_talk",
            "push_to_talk:down",
            "push_to_talk:up",
            "push_to_mute:up",
            "leave_voice",
            "answer_call",
            "decline_call",
            "decline_call:up",
            "toggle_mute:sideways",
            "nope",
        ] {
            // Without a running core `inject_cli` only parses.
            assert_eq!(
                parse_hotkey(value).is_some(),
                decibell_model::actions::hotkeys::inject_cli(value),
                "{value}"
            );
        }
    }

    #[test]
    fn launch_keeps_the_valid_parts() {
        let launch = Launch::new(
            &["decibell://invite/ABCD234567".into(), "decibell://bad link".into()],
            Some("toggle_deafen"),
        );
        assert_eq!(launch.links, vec!["decibell://invite/ABCD234567".to_string()]);
        assert_eq!(launch.hotkey.as_deref(), Some("toggle_deafen"));
        assert!(!launch.is_plain());
        assert!(Launch::new(&[], Some("nope")).is_plain());
    }
}
