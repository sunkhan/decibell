//! The suggestion menu's rows (`SearchBox.tsx`'s `rows` and `defaultActive`): what the menu lists
//! for the token being typed, or the run row, the filters and the inside-words option.

use chrono::Days;
use decibell_model::chat::search::{Day, HasKind, SearchQuery};
use decibell_model::ChannelId;
use gpui::SharedString;

use crate::tokens::{ChannelRef, MemberRef, Op, TypedToken, has_kind_for, has_name, js_trim};

/// One row of the menu (`Row`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// "Search for …" / "Search with these filters".
    Run(SharedString),
    Op(Op),
    Inside,
    Member(MemberRef),
    Channel(ChannelRef),
    Has(HasKind),
    Day { day: Day, label: &'static str },
}

/// The rows for the box's state.
pub fn build_rows(
    token: Option<&TypedToken>,
    draft: &SearchQuery,
    members: &[MemberRef],
    channels: &[ChannelRef],
    channel: Option<&ChannelId>,
    today: Day,
) -> Vec<Row> {
    if let Some(token) = token {
        let v = token.value.to_lowercase();
        return match token.op {
            Op::From => {
                let q = v.strip_prefix('@').unwrap_or(&v);
                members
                    .iter()
                    .filter(|m| {
                        q.is_empty() || m.name.to_lowercase().contains(q) || (!m.nickname.is_empty() && m.nickname.to_lowercase().contains(q))
                    })
                    .take(8)
                    .map(|m| Row::Member(m.clone()))
                    .collect()
            }
            Op::In => {
                let q = v.strip_prefix('#').unwrap_or(&v);
                let mut list: Vec<&ChannelRef> = channels.iter().filter(|c| q.is_empty() || c.name.to_lowercase().contains(q)).collect();
                // The channel you're in first (a stable sort, as JavaScript's).
                list.sort_by_key(|c| Some(&c.id) != channel);
                list.into_iter().take(10).map(|c| Row::Channel(c.clone())).collect()
            }
            Op::Has => HasKind::ALL
                .into_iter()
                .filter(|k| v.is_empty() || has_name(*k).starts_with(v.as_str()) || has_kind_for(&v) == Some(*k))
                .map(Row::Has)
                .collect(),
            _ => vec![
                Row::Day { day: today, label: "Today" },
                Row::Day { day: today.checked_sub_days(Days::new(1)).unwrap_or(today), label: "Yesterday" },
            ],
        };
    }
    let mut out = Vec::with_capacity(8);
    let text = js_trim(&draft.text);
    if !text.is_empty() || !draft.filters.is_empty() {
        out.push(Row::Run(text.to_string().into()));
    }
    out.extend(Op::ALL.into_iter().map(Row::Op));
    out.push(Row::Inside);
    out
}

/// `defaultActive`: a picker preselects its best match and typed words preselect "Search for…";
/// the plain filter list waits for the arrow keys (Enter in an empty box doesn't insert a filter).
/// Dates use the calendar's own day.
pub fn default_active(token: Option<&TypedToken>, rows: &[Row]) -> i32 {
    match token {
        Some(token) if token.op.is_date() => -1,
        Some(_) => 0,
        None if matches!(rows.first(), Some(Row::Run(_))) => 0,
        None => -1,
    }
}

/// The arrow keys over the rows (`setActive` in `onKeyDown`).
pub fn step_active(active: i32, rows: usize, down: bool) -> i32 {
    if rows == 0 {
        return active;
    }
    let n = rows as i32;
    if active < 0 {
        return if down { 0 } else { n - 1 };
    }
    (active + if down { 1 } else { -1 } + n) % n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::trailing_token;

    fn members() -> Vec<MemberRef> {
        ["alice", "bob", "carol", "dave", "erin", "perf1", "perf2", "perf3", "perf4", "aurora"]
            .iter()
            .enumerate()
            .map(|(i, n)| MemberRef { uid: i as i64 + 1, name: (*n).into(), nickname: if *n == "bob" { "Bobby".into() } else { "".into() } })
            .collect()
    }

    fn channels() -> Vec<ChannelRef> {
        ["welcome", "general", "clips", "off-topic"]
            .iter()
            .map(|n| ChannelRef { id: (*n).into(), name: (*n).into(), encrypted: false })
            .collect()
    }

    #[test]
    fn rows_per_token() {
        let today = Day::from_ymd_opt(2026, 11, 26).unwrap();
        let q = |text: &str| SearchQuery { text: text.to_string().into(), filters: vec![] };
        let rows = |text: &str| {
            let draft = q(text);
            let token = trailing_token(&draft.text);
            build_rows(token.as_ref(), &draft, &members(), &channels(), Some(&ChannelId::from("general")), today)
        };
        assert_eq!(rows("").len(), 7, "the filters and the option");
        assert_eq!(rows("  ").first(), Some(&Row::Op(Op::From)));
        assert_eq!(rows("raid ").first(), Some(&Row::Run("raid".into())));
        assert_eq!(rows("from:").len(), 8);
        assert_eq!(rows("from:@BOBB"), vec![Row::Member(members()[1].clone())]);
        assert_eq!(rows("from:perf").len(), 4);
        let r#in = rows("in:");
        assert_eq!(r#in.first(), Some(&Row::Channel(channels()[1].clone())), "the current channel first");
        assert_eq!(r#in.len(), 4);
        assert_eq!(rows("in:#o").len(), 2, "welcome, off-topic");
        assert_eq!(rows("has:"), HasKind::ALL.iter().map(|k| Row::Has(*k)).collect::<Vec<_>>());
        assert_eq!(rows("has:pic"), vec![]);
        assert_eq!(rows("has:pictures"), vec![Row::Has(HasKind::Image)]);
        assert_eq!(rows("has:i"), vec![Row::Has(HasKind::Image)]);
        assert_eq!(rows("before:").len(), 2);
        let draft = q("x before:");
        let token = trailing_token(&draft.text);
        assert_eq!(default_active(token.as_ref(), &[]), -1);
        assert_eq!(default_active(None, &rows("")), -1);
        assert_eq!(default_active(None, &rows("x")), 0);
        assert_eq!(step_active(-1, 7, false), 6);
        assert_eq!(step_active(6, 7, true), 0);
    }
}
