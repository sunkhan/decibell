//! Per-user playback gain (`features/voice/userGain.ts`): the dB ↔ linear mapping, the
//! "locally muted" override, and reading them from `Settings` (`user_volumes_by_uid`,
//! `local_muted_uids`: decimal uids, the core's `config.json`). The renderer mirrors these
//! into `voiceStore.userVolumes` / `localMutedUsers`; here `Settings` is their only home.

use decibell_core::config::AppSettings;

use crate::ids::Uid;

pub const MIN_DB: f64 = -40.0;
pub const MAX_DB: f64 = 15.0;
pub const DEFAULT_DB: f64 = 0.0;

/// dB → linear gain; the −40 dB floor is silence.
pub fn db_to_gain(db: f64) -> f64 {
    if db <= MIN_DB { 0.0 } else { 10f64.powf(db / 20.0) }
}

pub fn format_db(db: f64) -> String {
    if db <= MIN_DB {
        "Muted".into()
    } else if db == 0.0 {
        "0 dB".into()
    } else {
        format!("{}{:.1} dB", if db > 0.0 { "+" } else { "" }, db)
    }
}

pub fn db_to_percent(db: f64) -> String {
    if db <= MIN_DB { "0%".into() } else { format!("{}%", (db_to_gain(db) * 100.0).round()) }
}

/// A saved key → uid: a positive decimal written canonically (no "007", no "+7").
fn as_uid(key: &str) -> Option<Uid> {
    let n: Uid = key.parse().ok()?;
    (n > 0 && n.to_string() == key).then_some(n)
}

/// The saved volume of `uid`, dB.
pub fn user_db(s: &AppSettings, uid: Uid) -> Option<f64> {
    s.user_volumes_by_uid.iter().find(|(k, _)| as_uid(k) == Some(uid)).map(|(_, db)| *db)
}

pub fn locally_muted(s: &AppSettings, uid: Uid) -> bool {
    s.local_muted_uids.iter().any(|k| as_uid(k) == Some(uid))
}

/// The gain `uid` should play at: 0 when locally muted, else their saved dB.
pub fn effective_gain(s: &AppSettings, uid: Uid) -> f64 {
    if locally_muted(s, uid) { 0.0 } else { db_to_gain(user_db(s, uid).unwrap_or(DEFAULT_DB)) }
}

/// Whether a saved volume or local mute exists for `uid` (`applySavedUserGains` replays
/// only those).
pub fn has_saved_gain(s: &AppSettings, uid: Uid) -> bool {
    user_db(s, uid).is_some() || locally_muted(s, uid)
}

pub fn set_user_db(s: &mut AppSettings, uid: Uid, db: f64) {
    s.user_volumes_by_uid.retain(|k, _| as_uid(k) != Some(uid));
    s.user_volumes_by_uid.insert(uid.to_string(), db.clamp(MIN_DB, MAX_DB));
}

/// Flip `uid`'s local mute; returns the new state.
pub fn toggle_local_mute(s: &mut AppSettings, uid: Uid) -> bool {
    if locally_muted(s, uid) {
        s.local_muted_uids.retain(|k| as_uid(k) != Some(uid));
        false
    } else {
        s.local_muted_uids.push(uid.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_and_their_settings() {
        assert_eq!(db_to_gain(-40.0), 0.0);
        assert_eq!(db_to_gain(0.0), 1.0);
        assert!((db_to_gain(6.0) - 1.995).abs() < 0.01);
        assert_eq!(format_db(-40.0), "Muted");
        assert_eq!(format_db(3.0), "+3.0 dB");
        assert_eq!(format_db(-6.5), "-6.5 dB");
        assert_eq!(db_to_percent(0.0), "100%");

        let mut s = AppSettings::default();
        s.user_volumes_by_uid.insert("007".into(), 5.0);
        assert_eq!(user_db(&s, 7), None, "non-canonical keys are ignored");
        set_user_db(&mut s, 7, 99.0);
        assert_eq!(user_db(&s, 7), Some(MAX_DB));
        assert!(toggle_local_mute(&mut s, 7));
        assert_eq!(effective_gain(&s, 7), 0.0);
        assert!(!toggle_local_mute(&mut s, 7));
        assert!((effective_gain(&s, 7) - db_to_gain(MAX_DB)).abs() < 1e-9);
        assert!(has_saved_gain(&s, 7) && !has_saved_gain(&s, 8));
    }
}
