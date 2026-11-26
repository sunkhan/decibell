//! Server-side member search (`tabs/useMemberSearch.ts`; CLAUDE.md "Server-settings search runs
//! on the community server"): every member whose name or nickname contains the query and who
//! matches the role filter, online or not, by username, 100 per page. Its own entity, never the
//! roster: results don't reach the member sidebar. Roster deltas keep the loaded pages live: a
//! changed member is replaced, dropped once they stop matching, and slots in when they start to
//! (if they sort inside what's loaded).
//!
//! Timers exist only for their reason: the 250 ms debounce while typed text waits to be
//! searched, the 8 s "no answer" limit while a request is out.

use std::cell::Cell;
use std::collections::HashSet;
use std::time::Duration;

use decibell_core::events::CoreEvent;
use decibell_model::stores::community::Member;
use decibell_model::{Model, ServerId};
use gpui::{Context, SharedString, Subscription, Task};

use crate::requests::{self, Request};

/// 0 = any role, > 0 = holds that role, -1 = holds no role (wire contract).
pub type RoleFilter = i64;

/// Typing settles for this long before a search goes out. The community rate-limits list
/// fetches to 20 burst / 4 per s per session.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
pub const PAGE: i32 = 100;
/// A community that predates MEMBER_SEARCH_REQ ignores it; say so rather than spin forever.
pub const NO_ANSWER_AFTER: Duration = Duration::from_millis(8000);
pub const NO_ANSWER: &str = "The server didn't answer. It may be running an older version that can't search members yet.";

thread_local! {
    /// Seqs are unique across every open search (several can be live at once: the Members tab,
    /// a role's holders, the add-members picker), and each one only takes answers to its own
    /// latest request.
    static NEXT_SEQ: Cell<u32> = const { Cell::new(1) };
}

fn next_seq() -> u32 {
    NEXT_SEQ.with(|s| {
        let seq = s.get();
        s.set(seq.wrapping_add(1).max(1));
        seq
    })
}

pub struct MemberSearch {
    pub server: ServerId,
    /// What was typed.
    query: String,
    /// What was searched (typed, trimmed, after the debounce).
    applied: String,
    role: RoleFilter,
    pub members: Vec<Member>,
    pub total: i64,
    pub has_more: bool,
    next_after: SharedString,
    /// A request is in flight.
    pub loading: bool,
    /// The current query has answered at least once.
    pub loaded: bool,
    pub error: Option<SharedString>,
    seq: u32,
    answered: u32,
    /// Role ids in the last role list seen (none yet: `None`).
    known_roles: Option<HashSet<i64>>,
    _debounce: Option<Task<()>>,
    _no_answer: Option<Task<()>>,
    _answers: Subscription,
}

/// The renderer's `matches`: the role filter, then the query in the name or the nickname
/// (case-insensitive).
pub fn matches(m: &Member, query: &str, role: RoleFilter) -> bool {
    if role == -1 && !m.role_ids.is_empty() {
        return false;
    }
    if role > 0 && !m.role_ids.contains(&role) {
        return false;
    }
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    m.username.to_lowercase().contains(&q) || m.nickname.to_lowercase().contains(&q)
}

impl MemberSearch {
    pub fn new(server: ServerId, query: &str, role: RoleFilter, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let answers = cx.subscribe(&model.answers, |this: &mut Self, _, event: &CoreEvent, cx| this.on_event(event, cx));
        let mut this = MemberSearch {
            server,
            query: query.to_string(),
            applied: query.trim().to_string(),
            role,
            members: Vec::new(),
            total: 0,
            has_more: false,
            next_after: SharedString::default(),
            loading: true,
            loaded: false,
            error: None,
            seq: 0,
            answered: 0,
            known_roles: None,
            _debounce: None,
            _no_answer: None,
            _answers: answers,
        };
        this.send(String::new(), cx);
        this
    }

    /// The typed text hasn't been searched yet.
    pub fn settling(&self) -> bool {
        self.query.trim() != self.applied
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn role(&self) -> RoleFilter {
        self.role
    }

    /// Debounce the text; the search goes out once typing has paused.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query.to_string();
        let trimmed = query.trim().to_string();
        if trimmed == self.applied {
            self._debounce = None;
            cx.notify();
            return;
        }
        self._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                this._debounce = None;
                this.applied = trimmed;
                this.restart(cx);
            });
        }));
        cx.notify();
    }

    /// Search what was typed now, without the debounce (the screens: their clock doesn't run).
    pub fn apply_now(&mut self, cx: &mut Context<Self>) {
        self._debounce = None;
        self.applied = self.query.trim().to_string();
        self.restart(cx);
    }

    /// A role change applies at once.
    pub fn set_role(&mut self, role: RoleFilter, cx: &mut Context<Self>) {
        if self.role != role {
            self.role = role;
            self.restart(cx);
        }
    }

    /// A new search keeps the old rows on screen until its answer lands.
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.send(String::new(), cx);
        cx.notify();
    }

    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.has_more || self.loading {
            return;
        }
        self.loading = true;
        let after = self.next_after.to_string();
        self.send(after, cx);
        cx.notify();
    }

    fn send(&mut self, after: String, cx: &mut Context<Self>) {
        if after.is_empty() {
            self.seq = next_seq();
        }
        let seq = self.seq;
        self.answered = 0;
        let request = Request::SearchMembers {
            server: self.server,
            query: self.applied.clone(),
            role_id: self.role,
            after,
            limit: PAGE,
            seq,
        };
        let weak = cx.weak_entity();
        requests::send(request, cx, move |result, cx| {
            if let Err(e) = result {
                let _ = weak.update(cx, |this, cx| {
                    if seq == this.seq {
                        this.loading = false;
                        this.loaded = true;
                        this.error = Some(e.js_string());
                        cx.notify();
                    }
                });
            }
        });
        self._no_answer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NO_ANSWER_AFTER).await;
            let _ = this.update(cx, |this, cx| {
                this._no_answer = None;
                if seq == this.seq && this.answered != seq && this.loading {
                    this.loading = false;
                    this.loaded = true;
                    this.error = Some(NO_ANSWER.into());
                    cx.notify();
                }
            });
        }));
    }

    fn on_event(&mut self, event: &CoreEvent, cx: &mut Context<Self>) {
        let server = self.server;
        let ours = |id: &str| ServerId::from_wire(id) == server;
        match event {
            CoreEvent::MemberSearchReceived(p) if ours(&p.server_id) && p.seq == self.seq => {
                self.answered = p.seq;
                self._no_answer = None;
                self.loading = false;
                self.loaded = true;
                if !p.success {
                    self.error = Some(if p.message.is_empty() { "Couldn't search members.".into() } else { p.message.clone().into() });
                } else {
                    let page = p.members.iter().map(Member::from);
                    if p.first_page {
                        self.members = page.collect();
                    } else {
                        self.members.extend(page);
                    }
                    self.total = p.total_matches;
                    self.has_more = p.has_more;
                    self.next_after = p.next_after.clone().into();
                    self.error = None;
                }
                cx.notify();
            }
            CoreEvent::MemberUpsert(p) if ours(&p.server_id) => {
                if !self.loaded {
                    return;
                }
                let member = Member::from(&p.member);
                let fits = matches(&member, &self.applied, self.role);
                if let Some(index) = self.members.iter().position(|m| m.uid == member.uid) {
                    if fits {
                        self.members[index] = member;
                    } else {
                        self.members.remove(index);
                        self.total = (self.total - 1).max(0);
                    }
                    cx.notify();
                    return;
                }
                if !fits {
                    return;
                }
                // Only inside the loaded range, so paging leaves no gap (pages run in name order:
                // the server's cursor is the member's name).
                if self.has_more
                    && let Some(last) = self.members.last()
                    && member.username > last.username
                {
                    return;
                }
                let at = self.members.iter().position(|m| m.username > member.username).unwrap_or(self.members.len());
                self.members.insert(at, member);
                self.total += 1;
                cx.notify();
            }
            CoreEvent::MemberRemove(p) if ours(&p.server_id) => {
                if let Some(index) = self.members.iter().position(|m| m.uid == p.uid) {
                    self.members.remove(index);
                    self.total = (self.total - 1).max(0);
                    cx.notify();
                }
            }
            // A deleted role is announced only by the role list (no upsert per holder): drop its
            // id from the loaded rows. Under "no role" a member who just lost their last one now
            // matches but isn't loaded, so that search starts over.
            CoreEvent::RoleListReceived(p) if ours(&p.server_id) && p.success && !p.roles.is_empty() => {
                let known: HashSet<i64> = p.roles.iter().map(|r| r.id).collect();
                let before = self.known_roles.replace(known.clone());
                let mut changed = false;
                for m in &mut self.members {
                    if !m.role_ids.iter().all(|id| known.contains(id)) {
                        m.role_ids = m.role_ids.iter().copied().filter(|id| known.contains(id)).collect();
                        changed = true;
                    }
                }
                if changed {
                    cx.notify();
                }
                let deleted = before.is_some_and(|b| b.iter().any(|id| !known.contains(id)));
                if deleted && self.role == -1 {
                    self.send(String::new(), cx);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, nick: &str, roles: &[i64]) -> Member {
        Member::from(&decibell_core::events::MemberInfoPayload {
            username: name.into(),
            uid: 1,
            joined_at: 0,
            nickname: nick.into(),
            is_owner: false,
            is_online: true,
            role_ids: roles.to_vec(),
            timed_out_until: 0,
        })
    }

    #[test]
    fn matching_is_the_renderers() {
        let m = member("Alice", "Ally", &[4]);
        assert!(matches(&m, "", 0));
        assert!(matches(&m, "ali", 0), "case-insensitive in the name");
        assert!(matches(&m, "LLY", 0), "and in the nickname");
        assert!(!matches(&m, "bob", 0));
        assert!(matches(&m, "", 4));
        assert!(!matches(&m, "", 5));
        assert!(!matches(&m, "", -1), "has a role");
        assert!(matches(&member("bob", "", &[]), "", -1));
    }
}
