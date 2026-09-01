//! Application-level filter for share-audio capture.
//!
//! The Go Live dialog lets the streamer pick *which* applications a
//! stream carries: a whitelist (`Selected`), a blacklist (`AllExcept`)
//! or everything (`All`). The ticked set is one set of stable app
//! identities shared by both list modes; the mode only reinterprets it.
//!
//! Identity is a platform-local, lowercase, extension-less program name
//! (`chrome`, `spotify`, `firefox`) — never a PID — so the filter can be
//! persisted in the settings blob and survive app restarts. Each capture
//! backend resolves identities to live PIDs / PipeWire nodes itself.
//!
//! Everything in this file is cfg-free and unit-tested on the Linux dev
//! box, including the planning logic the Windows process-loopback mixer
//! runs (`plan_clients`, `dedup_tree_roots`, `diff_pids`): one WASAPI
//! process-loopback client covers exactly one process *tree*, so the
//! whitelist becomes N include-clients and the blacklist becomes a
//! dynamic include-set of everything that is not blocked.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamAudioMode {
    /// Whitelist: only ticked apps are streamed.
    Selected,
    /// Blacklist: everything except ticked apps.
    AllExcept,
    /// Everything (minus Decibell itself) — the pre-picker behaviour.
    #[default]
    All,
}

impl StreamAudioMode {
    /// Wire/config spelling: `selected` | `all_except` | `all`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "selected" => Some(Self::Selected),
            "all_except" => Some(Self::AllExcept),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::AllExcept => "all_except",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StreamAudioFilter {
    pub mode: StreamAudioMode,
    /// Normalised app identities (see [`normalize_identity`]).
    pub apps: BTreeSet<String>,
}

impl StreamAudioFilter {
    /// Lenient constructor for napi args / config: an unknown or absent
    /// mode falls back to `All`, identities are normalised, empties are
    /// dropped. Never fails — a bad filter must never block a stream.
    pub fn from_args(mode: Option<&str>, apps: Option<&[String]>) -> Self {
        let mode = mode.and_then(StreamAudioMode::parse).unwrap_or_default();
        let apps = apps
            .map(|list| {
                list.iter()
                    .map(|s| normalize_identity(s))
                    .filter(|s| !s.is_empty())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        Self { mode, apps }
    }

    /// Should audio from the app with this identity reach the stream?
    pub fn allows(&self, identity: &str) -> bool {
        match self.mode {
            StreamAudioMode::All => true,
            StreamAudioMode::Selected => self.apps.contains(&normalize_identity(identity)),
            StreamAudioMode::AllExcept => !self.apps.contains(&normalize_identity(identity)),
        }
    }

    /// True when the filter lets everything through — `All`, or
    /// `AllExcept` with nothing ticked. Backends use this to keep the
    /// cheap "capture everything minus self" path.
    pub fn is_pass_through(&self) -> bool {
        match self.mode {
            StreamAudioMode::All => true,
            StreamAudioMode::AllExcept => self.apps.is_empty(),
            StreamAudioMode::Selected => false,
        }
    }
}

/// Canonical identity spelling: trimmed, lowercase, without a trailing
/// `.exe`. Applied to everything that enters a filter and to every
/// identity a backend derives, so the two sides always compare equal.
pub fn normalize_identity(raw: &str) -> String {
    let mut s = raw.trim().to_lowercase();
    if let Some(stem) = s.strip_suffix(".exe") {
        s = stem.to_string();
    }
    s
}

/// Identity from a full executable path (either separator style):
/// `C:\Program Files\Spotify\Spotify.exe` → `spotify`,
/// `/usr/lib/firefox/firefox` → `firefox`. None for an empty basename.
pub fn identity_from_exe_path(path: &str) -> Option<String> {
    let base = path.rsplit(['\\', '/']).next().unwrap_or("");
    let id = normalize_identity(base);
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Handle to a running platform capture, owned by `AudioStreamEngine`.
/// Dropping it tears the capture down completely (threads joined, any
/// temporary audio routing removed).
pub trait StreamAudioCapture: Send {
    /// Replace the live filter. Must return quickly — the caller holds
    /// the AppState lock; do the work on the capture's own thread.
    fn set_filter(&self, filter: StreamAudioFilter);
}

// ───────────────────────── session planning (Windows) ─────────────────────────

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
/// One audio session as enumerated from the OS: a process that has an
/// audio render stream open (playing or paused).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSessionInfo {
    pub pid: u32,
    pub identity: String,
    /// Currently rendering (WASAPI `AudioSessionStateActive`).
    pub active: bool,
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
/// Which process-loopback clients the mixer should be running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientPlan {
    /// One client in EXCLUDE mode on our own process tree.
    ExcludeSelf,
    /// One INCLUDE-tree client per PID. Empty = stream silence.
    Include {
        pids: BTreeSet<u32>,
        /// Allowed roots that were dropped because a *blocked* process
        /// lives inside their tree (blacklist leak-safety). Logged by
        /// the caller so the user can see why an app went quiet.
        suppressed: Vec<u32>,
    },
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
/// Walk `parent_of` upwards from `pid`; true if `ancestor` is reached.
/// Bounded and cycle-safe (PID reuse can make a snapshot inconsistent).
fn has_ancestor(parent_of: &BTreeMap<u32, u32>, pid: u32, ancestor: u32) -> bool {
    let mut cur = pid;
    let mut seen = BTreeSet::new();
    while let Some(&p) = parent_of.get(&cur) {
        if p == ancestor {
            return true;
        }
        if p == 0 || p == cur || !seen.insert(p) || seen.len() > 128 {
            return false;
        }
        cur = p;
    }
    false
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
/// Drop every PID that has another member of the set as an ancestor —
/// an INCLUDE_TARGET_PROCESS_TREE client on the ancestor already covers
/// it, and a second client would double the audio.
pub fn dedup_tree_roots(pids: &BTreeSet<u32>, parent_of: &BTreeMap<u32, u32>) -> BTreeSet<u32> {
    pids.iter()
        .copied()
        .filter(|&pid| !pids.iter().any(|&other| other != pid && has_ancestor(parent_of, pid, other)))
        .collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
/// Decide the client set for a filter against a session snapshot.
///
/// * pass-through filter → `ExcludeSelf` (cheapest; also catches a
///   process that started playing between two snapshots).
/// * otherwise include every allowed session's PID, never PID 0, never
///   anything in our own tree or carrying our own identity, deduped to
///   tree roots. In `AllExcept` mode a root whose tree contains a
///   *blocked* PID is suppressed as well: the API can't include a parent
///   without its children, and a blacklist prefers silence over a leak.
pub fn plan_clients(
    filter: &StreamAudioFilter,
    sessions: &[AudioSessionInfo],
    parent_of: &BTreeMap<u32, u32>,
    self_pid: u32,
    self_identity: &str,
) -> ClientPlan {
    if filter.is_pass_through() {
        return ClientPlan::ExcludeSelf;
    }
    let self_identity = normalize_identity(self_identity);
    let is_self = |s: &AudioSessionInfo| {
        s.pid == self_pid
            || has_ancestor(parent_of, s.pid, self_pid)
            || (!self_identity.is_empty() && normalize_identity(&s.identity) == self_identity)
    };

    let mut allowed = BTreeSet::new();
    let mut blocked = BTreeSet::new();
    for s in sessions {
        if s.pid == 0 || is_self(s) {
            continue;
        }
        if filter.allows(&s.identity) {
            allowed.insert(s.pid);
        } else {
            blocked.insert(s.pid);
        }
    }

    let roots = dedup_tree_roots(&allowed, parent_of);
    let mut suppressed = Vec::new();
    let pids = if filter.mode == StreamAudioMode::AllExcept {
        roots
            .into_iter()
            .filter(|&root| {
                let leaks = blocked.iter().any(|&b| has_ancestor(parent_of, b, root));
                if leaks {
                    suppressed.push(root);
                }
                !leaks
            })
            .collect()
    } else {
        roots
    };
    ClientPlan::Include { pids, suppressed }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
/// `(to_add, to_remove)` to move the running client set to `wanted`.
pub fn diff_pids(current: &BTreeSet<u32>, wanted: &BTreeSet<u32>) -> (Vec<u32>, Vec<u32>) {
    let add = wanted.difference(current).copied().collect();
    let remove = current.difference(wanted).copied().collect();
    (add, remove)
}

// ───────────────────────────── list shape ─────────────────────────────

/// One row of the picker: an application, possibly spanning several
/// processes / nodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub pids: Vec<u32>,
    pub active: bool,
}

/// Group sessions by identity into picker rows, sorted by display name
/// (case-insensitive) then identity. `name_of` maps an identity to the
/// label to show; `active` is OR-ed across the group.
pub fn group_sessions(
    sessions: &[AudioSessionInfo],
    name_of: impl Fn(&str) -> String,
) -> Vec<AppEntry> {
    let mut by_id: BTreeMap<String, AppEntry> = BTreeMap::new();
    for s in sessions {
        let id = normalize_identity(&s.identity);
        if id.is_empty() {
            continue;
        }
        let e = by_id.entry(id.clone()).or_insert_with(|| AppEntry {
            name: name_of(&id),
            id,
            pids: Vec::new(),
            active: false,
        });
        if s.pid != 0 && !e.pids.contains(&s.pid) {
            e.pids.push(s.pid);
        }
        e.active |= s.active;
    }
    let mut out: Vec<AppEntry> = by_id.into_values().collect();
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn filter(mode: StreamAudioMode, ids: &[&str]) -> StreamAudioFilter {
        StreamAudioFilter { mode, apps: set(ids) }
    }

    fn sess(pid: u32, id: &str, active: bool) -> AudioSessionInfo {
        AudioSessionInfo { pid, identity: id.to_string(), active }
    }

    fn pids(list: &[u32]) -> BTreeSet<u32> {
        list.iter().copied().collect()
    }

    #[test]
    fn mode_parse_round_trips_and_rejects_unknown() {
        for m in [StreamAudioMode::Selected, StreamAudioMode::AllExcept, StreamAudioMode::All] {
            assert_eq!(StreamAudioMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(StreamAudioMode::parse(" all "), Some(StreamAudioMode::All));
        assert_eq!(StreamAudioMode::parse("whitelist"), None);
        assert_eq!(StreamAudioMode::default(), StreamAudioMode::All);
    }

    #[test]
    fn mode_serde_uses_snake_case() {
        let json = serde_json::to_string(&StreamAudioMode::AllExcept).unwrap();
        assert_eq!(json, "\"all_except\"");
        let back: StreamAudioMode = serde_json::from_str("\"selected\"").unwrap();
        assert_eq!(back, StreamAudioMode::Selected);
    }

    #[test]
    fn allows_per_mode() {
        let sel = filter(StreamAudioMode::Selected, &["spotify"]);
        assert!(sel.allows("spotify"));
        assert!(sel.allows("Spotify.exe"));
        assert!(!sel.allows("chrome"));

        let exc = filter(StreamAudioMode::AllExcept, &["spotify"]);
        assert!(!exc.allows("spotify"));
        assert!(exc.allows("chrome"));

        let all = filter(StreamAudioMode::All, &["spotify"]);
        assert!(all.allows("spotify"));
        assert!(all.allows("anything"));
    }

    #[test]
    fn pass_through_rules() {
        assert!(filter(StreamAudioMode::All, &["x"]).is_pass_through());
        assert!(filter(StreamAudioMode::AllExcept, &[]).is_pass_through());
        assert!(!filter(StreamAudioMode::AllExcept, &["x"]).is_pass_through());
        assert!(!filter(StreamAudioMode::Selected, &[]).is_pass_through());
    }

    #[test]
    fn normalisation() {
        assert_eq!(normalize_identity("  Chrome.EXE "), "chrome");
        assert_eq!(normalize_identity("firefox"), "firefox");
        assert_eq!(normalize_identity(".exe"), "");
        assert_eq!(
            identity_from_exe_path(r"C:\Program Files\Spotify\Spotify.exe"),
            Some("spotify".into())
        );
        assert_eq!(identity_from_exe_path("/usr/lib/firefox/firefox"), Some("firefox".into()));
        assert_eq!(identity_from_exe_path("C:\\dir\\"), None);
        assert_eq!(identity_from_exe_path(""), None);
    }

    #[test]
    fn from_args_is_lenient_and_dedups() {
        let apps = vec!["Spotify.exe".to_string(), " spotify ".to_string(), "".to_string(), "Chrome".to_string()];
        let f = StreamAudioFilter::from_args(Some("all_except"), Some(&apps));
        assert_eq!(f.mode, StreamAudioMode::AllExcept);
        assert_eq!(f.apps, set(&["chrome", "spotify"]));

        let f = StreamAudioFilter::from_args(Some("bogus"), None);
        assert_eq!(f, StreamAudioFilter::default());
        assert_eq!(StreamAudioFilter::from_args(None, None).mode, StreamAudioMode::All);
    }

    #[test]
    fn plan_pass_through_is_exclude_self() {
        let sessions = [sess(10, "chrome", true)];
        let f = filter(StreamAudioMode::All, &[]);
        assert_eq!(plan_clients(&f, &sessions, &BTreeMap::new(), 1, "decibell"), ClientPlan::ExcludeSelf);
        let f = filter(StreamAudioMode::AllExcept, &[]);
        assert_eq!(plan_clients(&f, &sessions, &BTreeMap::new(), 1, "decibell"), ClientPlan::ExcludeSelf);
    }

    #[test]
    fn plan_selected_includes_only_ticked_and_never_self() {
        let sessions = [
            sess(10, "chrome", true),
            sess(20, "spotify", false),
            sess(30, "decibell", true), // our own identity, different pid
            sess(0, "spotify", true),   // pid 0 never planned
        ];
        let f = filter(StreamAudioMode::Selected, &["spotify", "decibell"]);
        let plan = plan_clients(&f, &sessions, &BTreeMap::new(), 1, "Decibell.exe");
        assert_eq!(plan, ClientPlan::Include { pids: pids(&[20]), suppressed: vec![] });

        let f = filter(StreamAudioMode::Selected, &[]);
        let plan = plan_clients(&f, &sessions, &BTreeMap::new(), 1, "decibell");
        assert_eq!(plan, ClientPlan::Include { pids: pids(&[]), suppressed: vec![] });
    }

    #[test]
    fn plan_all_except_excludes_ticked_and_own_tree() {
        // 1 = us, 2 = our audio child, 10 = chrome, 20 = spotify
        let parent_of: BTreeMap<u32, u32> = [(2, 1), (10, 5), (20, 5)].into_iter().collect();
        let sessions = [
            sess(2, "electron", true),
            sess(10, "chrome", true),
            sess(20, "spotify", true),
        ];
        let f = filter(StreamAudioMode::AllExcept, &["spotify"]);
        let plan = plan_clients(&f, &sessions, &parent_of, 1, "decibell");
        assert_eq!(plan, ClientPlan::Include { pids: pids(&[10]), suppressed: vec![] });
    }

    #[test]
    fn plan_all_except_suppresses_parent_of_blocked_child() {
        // steam (100) launched game (200); blacklist the game → steam must go too.
        let parent_of: BTreeMap<u32, u32> = [(200, 100)].into_iter().collect();
        let sessions = [sess(100, "steam", true), sess(200, "game", true), sess(300, "chrome", true)];
        let f = filter(StreamAudioMode::AllExcept, &["game"]);
        let plan = plan_clients(&f, &sessions, &parent_of, 1, "decibell");
        assert_eq!(plan, ClientPlan::Include { pids: pids(&[300]), suppressed: vec![100] });

        // Whitelist mode does NOT suppress: the user ticked steam, hearing
        // the game too is the lesser evil vs silencing steam.
        let f = filter(StreamAudioMode::Selected, &["steam"]);
        let plan = plan_clients(&f, &sessions, &parent_of, 1, "decibell");
        assert_eq!(plan, ClientPlan::Include { pids: pids(&[100]), suppressed: vec![] });
    }

    #[test]
    fn dedup_keeps_only_tree_roots_and_survives_cycles() {
        let parent_of: BTreeMap<u32, u32> = [(20, 10), (30, 20), (40, 4), (50, 60), (60, 50)].into_iter().collect();
        assert_eq!(dedup_tree_roots(&pids(&[10, 20, 30, 40]), &parent_of), pids(&[10, 40]));
        assert_eq!(dedup_tree_roots(&pids(&[20, 30]), &parent_of), pids(&[20]));
        // 50 ⇄ 60 cycle: each is the other's "ancestor" — both drop, no hang.
        let out = dedup_tree_roots(&pids(&[50, 60]), &parent_of);
        assert!(out.is_empty());
        assert_eq!(dedup_tree_roots(&pids(&[]), &parent_of), pids(&[]));
    }

    #[test]
    fn diff_pids_splits_add_and_remove() {
        let (add, remove) = diff_pids(&pids(&[1, 2, 3]), &pids(&[2, 3, 4]));
        assert_eq!(add, vec![4]);
        assert_eq!(remove, vec![1]);
        let (add, remove) = diff_pids(&pids(&[]), &pids(&[]));
        assert!(add.is_empty() && remove.is_empty());
    }

    #[test]
    fn group_sessions_merges_pids_and_ors_active() {
        let sessions = [
            sess(10, "chrome", false),
            sess(11, "Chrome.exe", true),
            sess(20, "spotify", false),
            sess(0, "", true),
        ];
        let rows = group_sessions(&sessions, |id| format!("App {id}"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], AppEntry { id: "chrome".into(), name: "App chrome".into(), pids: vec![10, 11], active: true });
        assert_eq!(rows[1], AppEntry { id: "spotify".into(), name: "App spotify".into(), pids: vec![20], active: false });
    }

    #[test]
    fn group_sessions_sorts_by_name_case_insensitively() {
        let sessions = [sess(1, "zeta", false), sess(2, "alpha", false), sess(3, "Beta", false)];
        let rows = group_sessions(&sessions, |id| match id {
            "zeta" => "aardvark".to_string(),
            other => other.to_string(),
        });
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["zeta", "alpha", "beta"]);
    }
}
