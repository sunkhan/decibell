//! The MLS group behind one voice-channel session (OpenMLS, RFC 9420).
//! Design: docs/superpowers/specs/2026-09-03-mls-voice-channel-encryption-design.md.
//!
//! One `Driver` task per voice session owns the OpenMLS provider and group
//! and reacts to four inputs — the roster (VOICE_PRESENCE_UPDATE), the
//! server's GroupInfo / commit replies, other members' commit broadcasts,
//! and a 500 ms tick — through a command channel. Every epoch change
//! exports the media keys into the shared `KeyRing` the pipelines read.
//!
//!   join ─► GROUP_INFO_REQ ─► none ─► MlsGroup::new ─► GROUP_CREATE_REQ
//!                          └► some ─► external commit ─► COMMIT_REQ
//!   COMMIT_RES ok ─► merge pending ─► verify leaves ─► keys
//!   COMMIT_BROADCAST ─► process ─► merge ─► verify leaves ─► keys
//!   any failure that leaves us behind ─► resync: rejoin by external commit
//!   roster − members ─► the elected member (lowest verified name) removes them
//!
//! Identity binding: after every merge each leaf's (identity, signature key)
//! is checked against central's key directory via the E2EE session. An
//! unverified leaf quarantines the epoch (nothing is sealed) and the elected
//! member commits its removal — a server can put anyone on the roster, but
//! not a fake identity in the group.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use tokio::sync::{mpsc, Mutex};

use super::identity::IdentityKeys;
use super::session as e2ee;
use crate::events;
use crate::media::frame_crypto::{EpochKeys, SharedKeyRing};
use crate::net::connection::build_packet;
use crate::net::proto::{packet, MlsCommitReq, MlsGroupCreateReq, MlsGroupInfoReq};
use crate::state::AppState;

const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
const EXPORT_LABEL: &str = "decibell-media-v1";
/// A member missing from the roster is removed after this long (a quick
/// reconnect rejoins with a fresh leaf anyway).
const GHOST_GRACE: Duration = Duration::from_secs(3);
const TICK: Duration = Duration::from_millis(500);
const MAX_JOIN_ATTEMPTS: u32 = 6;
const REPLY_TIMEOUT: Duration = Duration::from_secs(6);

pub enum GroupCmd {
    /// The channel roster from VOICE_PRESENCE_UPDATE.
    Presence(Vec<String>),
    GroupInfoRes { exists: bool, epoch: u64, group_info: Vec<u8> },
    CommitRes { success: bool, message: String, epoch: u64 },
    CommitBroadcast { epoch: u64, sender: String, commit: Vec<u8> },
    /// A stream is starting: nothing to do for keys (the per-stream salt
    /// keeps nonces fresh) — kept as a hook.
    Shutdown,
}

/// What `join_voice_channel` holds; dropping it stops the driver.
pub struct GroupHandle {
    pub tx: mpsc::UnboundedSender<GroupCmd>,
    pub ring: SharedKeyRing,
    pub server_id: String,
    pub channel_id: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for GroupHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(GroupCmd::Shutdown);
        self.task.abort();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    FetchingInfo,
    Creating,
    Joining,
    Operational,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    None,
    Create,
    Join,
    /// A commit we built while operational (remove / update).
    Own,
}

struct Driver {
    state: Arc<Mutex<AppState>>,
    server_id: String,
    channel_id: String,
    username: String,
    jwt: String,
    write_tx: mpsc::Sender<Vec<u8>>,
    ring: SharedKeyRing,
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    credential: CredentialWithKey,
    own_sign_pub: Vec<u8>,
    group: Option<MlsGroup>,
    phase: Phase,
    pending: Pending,
    /// Epoch our pending commit creates (for logging / sanity).
    pending_epoch: u64,
    sent_at: Instant,
    join_attempts: u32,
    roster: HashSet<String>,
    /// username → signature key verified against central this session.
    verified: HashMap<String, Vec<u8>>,
    /// Names we could not verify in the current epoch.
    unverified: Vec<String>,
    ghost_since: HashMap<String, Instant>,
    last_state: String,
}

pub fn start(
    state: Arc<Mutex<AppState>>,
    server_id: String,
    channel_id: String,
    username: String,
    identity: &IdentityKeys,
    jwt: String,
    write_tx: mpsc::Sender<Vec<u8>>,
    ring: SharedKeyRing,
) -> Result<GroupHandle, String> {
    let provider = OpenMlsRustCrypto::default();
    let sign_pub = identity.sign_public()?.to_vec();
    let signer = SignatureKeyPair::from_raw(SignatureScheme::ED25519, identity.sign_priv.clone(), sign_pub.clone());
    signer
        .store(provider.storage())
        .map_err(|e| format!("store signer: {e:?}"))?;
    let credential: Credential = BasicCredential::new(username.as_bytes().to_vec()).into();
    let credential = CredentialWithKey { credential, signature_key: signer.to_public_vec().into() };
    let (tx, rx) = mpsc::unbounded_channel();
    let driver = Driver {
        state,
        server_id: server_id.clone(),
        channel_id: channel_id.clone(),
        username,
        jwt,
        write_tx,
        ring: ring.clone(),
        provider,
        signer,
        credential,
        own_sign_pub: sign_pub,
        group: None,
        phase: Phase::FetchingInfo,
        pending: Pending::None,
        pending_epoch: 0,
        sent_at: Instant::now(),
        join_attempts: 0,
        roster: HashSet::new(),
        verified: HashMap::new(),
        unverified: Vec::new(),
        ghost_since: HashMap::new(),
        last_state: String::new(),
    };
    let task = tokio::spawn(driver.run(rx));
    Ok(GroupHandle { tx, ring, server_id, channel_id, task })
}

fn create_config() -> MlsGroupCreateConfig {
    MlsGroupCreateConfig::builder()
        .ciphersuite(CIPHERSUITE)
        .use_ratchet_tree_extension(true)
        .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
        .build()
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
        .build()
}

fn identity_of(credential: &Credential) -> Option<String> {
    let basic = BasicCredential::try_from(credential.clone()).ok()?;
    String::from_utf8(basic.identity().to_vec()).ok()
}

impl Driver {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<GroupCmd>) {
        self.request_group_info().await;
        let mut tick = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    match cmd {
                        None | Some(GroupCmd::Shutdown) => break,
                        Some(GroupCmd::Presence(roster)) => self.on_presence(roster).await,
                        Some(GroupCmd::GroupInfoRes { exists, epoch, group_info }) => {
                            self.on_group_info(exists, epoch, group_info).await
                        }
                        Some(GroupCmd::CommitRes { success, message, epoch }) => {
                            self.on_commit_res(success, message, epoch).await
                        }
                        Some(GroupCmd::CommitBroadcast { epoch, sender, commit }) => {
                            self.on_commit_broadcast(epoch, sender, commit).await
                        }
                    }
                }
                _ = tick.tick() => self.on_tick().await,
            }
        }
        log::info!("[mls] {} driver stopped", self.channel_id);
    }

    // ── Outbound ────────────────────────────────────────────────

    async fn send(&self, ty: packet::Type, payload: packet::Payload) {
        let data = build_packet(ty, payload, Some(&self.jwt));
        if tokio::time::timeout(Duration::from_secs(5), self.write_tx.send(data)).await.is_err() {
            log::warn!("[mls] {} send timed out", self.channel_id);
        }
    }

    async fn request_group_info(&mut self) {
        self.phase = Phase::FetchingInfo;
        self.pending = Pending::None;
        self.sent_at = Instant::now();
        self.emit("joining");
        self.send(
            packet::Type::MlsGroupInfoReq,
            packet::Payload::MlsGroupInfoReq(MlsGroupInfoReq { channel_id: self.channel_id.clone() }),
        )
        .await;
    }

    fn emit(&mut self, state: &str) {
        let epoch = self.group.as_ref().map(|g| g.epoch().as_u64()).unwrap_or(0);
        let members = self.group.as_ref().map(|g| g.members().count() as u32).unwrap_or(0);
        let key = format!("{state}:{epoch}:{members}:{}", self.unverified.join(","));
        if key == self.last_state {
            return;
        }
        self.last_state = key;
        events::emit_voice_e2ee_state(events::VoiceE2eeStatePayload {
            server_id: self.server_id.clone(),
            channel_id: self.channel_id.clone(),
            state: state.to_string(),
            epoch,
            members,
            unverified: self.unverified.clone(),
        });
    }

    // ── Group info / create / join ──────────────────────────────

    async fn on_group_info(&mut self, exists: bool, epoch: u64, group_info: Vec<u8>) {
        if self.phase != Phase::FetchingInfo {
            return;
        }
        if !exists {
            match MlsGroup::new(&self.provider, &self.signer, &create_config(), self.credential.clone()) {
                Ok(group) => {
                    let gi = match group
                        .export_group_info(self.provider.crypto(), &self.signer, true)
                        .map_err(|e| format!("{e:?}"))
                        .and_then(|m| m.to_bytes().map_err(|e| format!("{e:?}")))
                    {
                        Ok(b) => b,
                        Err(e) => {
                            log::error!("[mls] export group info: {e}");
                            return;
                        }
                    };
                    self.group = Some(group);
                    self.phase = Phase::Creating;
                    self.pending = Pending::Create;
                    self.pending_epoch = 0;
                    self.sent_at = Instant::now();
                    self.send(
                        packet::Type::MlsGroupCreateReq,
                        packet::Payload::MlsGroupCreateReq(MlsGroupCreateReq {
                            channel_id: self.channel_id.clone(),
                            group_info: gi,
                        }),
                    )
                    .await;
                }
                Err(e) => log::error!("[mls] create group: {e:?}"),
            }
            return;
        }
        // Join the existing group by external commit.
        let vgi = match MlsMessageIn::tls_deserialize_exact(&group_info)
            .ok()
            .and_then(|m| match m.extract() {
                MlsMessageBodyIn::GroupInfo(v) => Some(v),
                _ => None,
            })
        {
            Some(v) => v,
            None => {
                log::warn!("[mls] {} malformed GroupInfo from server", self.channel_id);
                self.retry_join_later();
                return;
            }
        };
        let built = MlsGroup::external_commit_builder()
            .with_config(join_config())
            .build_group(&self.provider, vgi, self.credential.clone())
            .map_err(|e| format!("{e:?}"))
            .and_then(|b| b.load_psks(self.provider.storage()).map_err(|e| format!("{e:?}")))
            .and_then(|b| {
                b.build(self.provider.rand(), self.provider.crypto(), &self.signer, |_| true)
                    .map_err(|e| format!("{e:?}"))
            })
            .and_then(|b| b.finalize(&self.provider).map_err(|e| format!("{e:?}")));
        let (group, bundle) = match built {
            Ok(x) => x,
            Err(e) => {
                log::warn!("[mls] {} external commit failed: {e}", self.channel_id);
                self.retry_join_later();
                return;
            }
        };
        let commit = match bundle.commit().to_bytes() {
            Ok(b) => b,
            Err(e) => {
                log::error!("[mls] serialize commit: {e:?}");
                return;
            }
        };
        let gi = bundle
            .group_info()
            .and_then(|g| MlsMessageOut::from(g.clone()).to_bytes().ok())
            .unwrap_or_default();
        self.group = Some(group);
        self.phase = Phase::Joining;
        self.pending = Pending::Join;
        self.pending_epoch = epoch + 1;
        self.sent_at = Instant::now();
        self.join_attempts += 1;
        self.send(
            packet::Type::MlsCommitReq,
            packet::Payload::MlsCommitReq(MlsCommitReq {
                channel_id: self.channel_id.clone(),
                epoch: epoch + 1,
                commit,
                group_info: gi,
            }),
        )
        .await;
    }

    fn retry_join_later(&mut self) {
        self.group = None;
        self.phase = Phase::FetchingInfo;
        self.pending = Pending::None;
        // The tick re-requests after a short backoff.
        self.sent_at = Instant::now() - REPLY_TIMEOUT + Duration::from_millis(300 * (self.join_attempts as u64 + 1));
        self.join_attempts += 1;
    }

    async fn on_commit_res(&mut self, success: bool, message: String, epoch: u64) {
        match self.pending {
            Pending::Create => {
                self.pending = Pending::None;
                if success {
                    log::info!("[mls] {} created group (epoch 0)", self.channel_id);
                    self.phase = Phase::Operational;
                    self.join_attempts = 0;
                    self.after_merge().await;
                } else {
                    // Someone created it first — join theirs.
                    log::info!("[mls] {} create refused ({message}); joining instead", self.channel_id);
                    self.group = None;
                    self.request_group_info().await;
                }
            }
            Pending::Join => {
                self.pending = Pending::None;
                if success {
                    let merged = self
                        .group
                        .as_mut()
                        .map(|g| g.merge_pending_commit(&self.provider).map_err(|e| format!("{e:?}")))
                        .unwrap_or(Err("no group".into()));
                    match merged {
                        Ok(()) => {
                            log::info!("[mls] {} joined at epoch {epoch}", self.channel_id);
                            self.phase = Phase::Operational;
                            self.join_attempts = 0;
                            self.after_merge().await;
                        }
                        Err(e) => {
                            log::warn!("[mls] {} merge after join failed: {e}", self.channel_id);
                            self.resync().await;
                        }
                    }
                } else {
                    log::info!("[mls] {} join refused ({message}, server epoch {epoch}); retrying", self.channel_id);
                    if self.join_attempts >= MAX_JOIN_ATTEMPTS {
                        log::error!("[mls] {} giving up joining the group", self.channel_id);
                        self.emit("failed");
                        return;
                    }
                    self.group = None;
                    self.request_group_info().await;
                }
            }
            Pending::Own => {
                self.pending = Pending::None;
                let Some(group) = self.group.as_mut() else { return };
                if success {
                    if let Err(e) = group.merge_pending_commit(&self.provider) {
                        log::warn!("[mls] {} merge own commit failed: {e:?}", self.channel_id);
                        self.resync().await;
                        return;
                    }
                    self.after_merge().await;
                } else {
                    // Lost the race for this epoch: drop ours; the winner's
                    // broadcast follows (or already did — see on_commit_broadcast).
                    let _ = group.clear_pending_commit(self.provider.storage());
                    log::info!("[mls] {} own commit refused ({message}, server epoch {epoch})", self.channel_id);
                    if group.epoch().as_u64() + 1 < epoch {
                        // We're more than one epoch behind: rejoin.
                        self.resync().await;
                    }
                }
            }
            Pending::None => {}
        }
    }

    async fn on_commit_broadcast(&mut self, epoch: u64, sender: String, commit: Vec<u8>) {
        if self.phase != Phase::Operational {
            // Joining/creating: our own commit will be refused and we
            // re-fetch; nothing to apply yet.
            return;
        }
        let Some(group) = self.group.as_mut() else { return };
        if self.pending == Pending::Own {
            // Someone else's commit won this epoch; ours is dead.
            let _ = group.clear_pending_commit(self.provider.storage());
            self.pending = Pending::None;
        }
        let our_epoch = group.epoch().as_u64();
        if epoch <= our_epoch {
            return; // stale / duplicate
        }
        if epoch != our_epoch + 1 {
            log::warn!("[mls] {} missed an epoch (have {our_epoch}, got {epoch}) — resync", self.channel_id);
            self.resync().await;
            return;
        }
        let processed = MlsMessageIn::tls_deserialize_exact(&commit)
            .map_err(|e| format!("{e:?}"))
            .and_then(|m| m.try_into_protocol_message().map_err(|e| format!("{e:?}")))
            .and_then(|pm| group.process_message(&self.provider, pm).map_err(|e| format!("{e:?}")));
        match processed {
            Ok(p) => match p.into_content() {
                ProcessedMessageContent::StagedCommitMessage(staged) => {
                    if let Err(e) = group.merge_staged_commit(&self.provider, *staged) {
                        log::warn!("[mls] {} merge commit from {sender} failed: {e:?}", self.channel_id);
                        self.resync().await;
                        return;
                    }
                    if !group.is_active() {
                        log::info!("[mls] {} we were removed from the group — rejoining", self.channel_id);
                        self.resync().await;
                        return;
                    }
                    self.after_merge().await;
                }
                _ => log::debug!("[mls] {} ignoring non-commit message from {sender}", self.channel_id),
            },
            Err(e) => {
                log::warn!("[mls] {} can't process commit from {sender}: {e} — resync", self.channel_id);
                self.resync().await;
            }
        }
    }

    /// Discard the group and rejoin by external commit.
    async fn resync(&mut self) {
        self.group = None;
        self.unverified.clear();
        self.emit("resyncing");
        self.request_group_info().await;
    }

    // ── After every merge: verify leaves, export keys ───────────

    async fn after_merge(&mut self) {
        let Some(group) = self.group.as_ref() else { return };
        let members: Vec<(String, Vec<u8>, LeafNodeIndex)> = group
            .members()
            .filter_map(|m| identity_of(&m.credential).map(|id| (id, m.signature_key.clone(), m.index)))
            .collect();
        // Verify identities against central (cached per session).
        let mut unverified: Vec<String> = Vec::new();
        for (name, key, _) in &members {
            if *name == self.username {
                if *key != self.own_sign_pub {
                    unverified.push(name.clone());
                }
                continue;
            }
            if self.verified.get(name) == Some(key) {
                continue;
            }
            let ok = match e2ee::resolve_peer_current(&self.state, name).await {
                Ok(Some(b)) => b.sign_pub == *key,
                Ok(None) => false,
                Err(e) => {
                    log::warn!("[mls] key lookup for {name} failed: {e}");
                    false
                }
            };
            if ok {
                self.verified.insert(name.clone(), key.clone());
            } else {
                unverified.push(name.clone());
            }
        }
        // Some names may have been dropped by a resync of the peer; also
        // count duplicates of the same identity as suspicious.
        let Some(group) = self.group.as_ref() else { return };
        self.unverified = unverified.clone();
        if !unverified.is_empty() {
            log::warn!("[mls] {} unverified leaves: {:?} — quarantining epoch {}", self.channel_id, unverified, group.epoch().as_u64());
            let q = self.ring.load().quarantine();
            self.ring.store(Arc::new(q));
            self.emit("quarantine");
            if self.is_elected(&members) {
                let idx: Vec<LeafNodeIndex> = members
                    .iter()
                    .filter(|(n, _, _)| unverified.contains(n))
                    .map(|(_, _, i)| *i)
                    .collect();
                self.commit_remove(idx).await;
            }
            return;
        }
        let exporter = match group.export_secret(self.provider.crypto(), EXPORT_LABEL, self.channel_id.as_bytes(), 32) {
            Ok(s) => s,
            Err(e) => {
                log::error!("[mls] {} export secret: {e:?}", self.channel_id);
                return;
            }
        };
        let names: Vec<String> = members.iter().map(|(n, _, _)| n.clone()).collect();
        let keys = EpochKeys::derive(group.epoch().as_u64(), &exporter, &names);
        let next = self.ring.load().rotated(keys);
        self.ring.store(Arc::new(next));
        log::info!("[mls] {} epoch {} keys ready ({} members)", self.channel_id, group.epoch().as_u64(), names.len());
        self.emit("ready");
        self.reconcile_roster().await;
    }

    /// The member who commits removals: the lowest verified name that is
    /// both in the group and on the roster.
    fn is_elected(&self, members: &[(String, Vec<u8>, LeafNodeIndex)]) -> bool {
        let mut candidates: Vec<&String> = members
            .iter()
            .map(|(n, _, _)| n)
            .filter(|n| **n == self.username || self.verified.contains_key(*n))
            .filter(|n| self.roster.is_empty() || self.roster.contains(*n))
            .collect();
        candidates.sort();
        candidates.first().map(|n| **n == self.username).unwrap_or(false)
    }

    async fn commit_remove(&mut self, leaves: Vec<LeafNodeIndex>) {
        if leaves.is_empty() || self.pending != Pending::None || self.phase != Phase::Operational {
            return;
        }
        let Some(group) = self.group.as_mut() else { return };
        let (commit, _welcome, gi) = match group.remove_members(&self.provider, &self.signer, &leaves) {
            Ok(x) => x,
            Err(e) => {
                log::warn!("[mls] {} remove_members: {e:?}", self.channel_id);
                return;
            }
        };
        let commit = match commit.to_bytes() {
            Ok(b) => b,
            Err(_) => return,
        };
        let gi = gi.and_then(|g| MlsMessageOut::from(g).to_bytes().ok()).unwrap_or_default();
        let new_epoch = group.epoch().as_u64() + 1;
        self.pending = Pending::Own;
        self.pending_epoch = new_epoch;
        self.sent_at = Instant::now();
        log::info!("[mls] {} removing {} leaf/leaves → epoch {new_epoch}", self.channel_id, leaves.len());
        self.send(
            packet::Type::MlsCommitReq,
            packet::Payload::MlsCommitReq(MlsCommitReq {
                channel_id: self.channel_id.clone(),
                epoch: new_epoch,
                commit,
                group_info: gi,
            }),
        )
        .await;
    }

    // ── Roster ──────────────────────────────────────────────────

    async fn on_presence(&mut self, roster: Vec<String>) {
        self.roster = roster.into_iter().collect();
        if self.phase == Phase::Operational {
            self.reconcile_roster().await;
        }
    }

    /// Members of the group that the server no longer lists in the channel
    /// are ghosts (crashed / disconnected without a commit). After a grace
    /// period the elected member removes them.
    async fn reconcile_roster(&mut self) {
        if self.phase != Phase::Operational || self.roster.is_empty() {
            return;
        }
        let Some(group) = self.group.as_ref() else { return };
        let members: Vec<(String, Vec<u8>, LeafNodeIndex)> = group
            .members()
            .filter_map(|m| identity_of(&m.credential).map(|id| (id, m.signature_key.clone(), m.index)))
            .collect();
        if !members.iter().any(|(n, _, _)| *n == self.username) {
            self.resync().await;
            return;
        }
        let now = Instant::now();
        let mut due: Vec<LeafNodeIndex> = Vec::new();
        for (name, _, idx) in &members {
            if self.roster.contains(name) {
                self.ghost_since.remove(name);
                continue;
            }
            let since = *self.ghost_since.entry(name.clone()).or_insert(now);
            if now.duration_since(since) >= GHOST_GRACE {
                due.push(*idx);
            }
        }
        self.ghost_since.retain(|n, _| members.iter().any(|(m, _, _)| m == n));
        if !due.is_empty() && self.is_elected(&members) {
            self.commit_remove(due).await;
        }
    }

    async fn on_tick(&mut self) {
        match self.phase {
            Phase::FetchingInfo => {
                if self.sent_at.elapsed() >= REPLY_TIMEOUT {
                    if self.join_attempts >= MAX_JOIN_ATTEMPTS {
                        self.emit("failed");
                        return;
                    }
                    self.join_attempts += 1;
                    self.request_group_info().await;
                }
            }
            Phase::Creating | Phase::Joining => {
                if self.sent_at.elapsed() >= REPLY_TIMEOUT {
                    log::warn!("[mls] {} no reply to our commit — re-fetching", self.channel_id);
                    self.group = None;
                    self.request_group_info().await;
                }
            }
            Phase::Operational => {
                if self.pending == Pending::Own && self.sent_at.elapsed() >= REPLY_TIMEOUT {
                    if let Some(g) = self.group.as_mut() {
                        let _ = g.clear_pending_commit(self.provider.storage());
                    }
                    self.pending = Pending::None;
                }
                self.reconcile_roster().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Two and three members through a simulated delivery service: create,
    //! external join, commit sequencing, removal, and that every member
    //! exports the same media secret per epoch. Exercises exactly the
    //! OpenMLS calls the driver makes, without the network.
    use super::*;

    struct Peer {
        provider: OpenMlsRustCrypto,
        signer: SignatureKeyPair,
        cred: CredentialWithKey,
        group: Option<MlsGroup>,
    }

    fn peer(name: &str) -> Peer {
        let id = IdentityKeys::generate(1).unwrap();
        let provider = OpenMlsRustCrypto::default();
        let pubk = id.sign_public().unwrap().to_vec();
        let signer = SignatureKeyPair::from_raw(SignatureScheme::ED25519, id.sign_priv.clone(), pubk);
        signer.store(provider.storage()).unwrap();
        let credential: Credential = BasicCredential::new(name.as_bytes().to_vec()).into();
        let cred = CredentialWithKey { credential, signature_key: signer.to_public_vec().into() };
        Peer { provider, signer, cred, group: None }
    }

    /// The delivery service: current epoch + GroupInfo bytes.
    struct Ds {
        epoch: u64,
        info: Vec<u8>,
    }

    fn export(p: &Peer) -> Vec<u8> {
        p.group.as_ref().unwrap().export_secret(p.provider.crypto(), EXPORT_LABEL, b"voice-lounge", 32).unwrap()
    }

    fn join(p: &mut Peer, ds: &mut Ds) -> Vec<u8> {
        let vgi = match MlsMessageIn::tls_deserialize_exact(&ds.info).unwrap().extract() {
            MlsMessageBodyIn::GroupInfo(v) => v,
            _ => panic!("not a GroupInfo"),
        };
        let (group, bundle) = MlsGroup::external_commit_builder()
            .with_config(join_config())
            .build_group(&p.provider, vgi, p.cred.clone())
            .unwrap()
            .load_psks(p.provider.storage())
            .unwrap()
            .build(p.provider.rand(), p.provider.crypto(), &p.signer, |_| true)
            .unwrap()
            .finalize(&p.provider)
            .unwrap();
        let commit = bundle.commit().to_bytes().unwrap();
        ds.epoch += 1;
        ds.info = MlsMessageOut::from(bundle.group_info().unwrap().clone()).to_bytes().unwrap();
        p.group = Some(group);
        p.group.as_mut().unwrap().merge_pending_commit(&p.provider).unwrap();
        commit
    }

    fn apply(p: &mut Peer, commit: &[u8]) {
        let g = p.group.as_mut().unwrap();
        let pm = MlsMessageIn::tls_deserialize_exact(commit).unwrap().try_into_protocol_message().unwrap();
        match g.process_message(&p.provider, pm).unwrap().into_content() {
            ProcessedMessageContent::StagedCommitMessage(sc) => g.merge_staged_commit(&p.provider, *sc).unwrap(),
            _ => panic!("expected a commit"),
        }
    }

    #[test]
    fn create_join_remove_agree_on_keys() {
        let mut alice = peer("alice");
        let g = MlsGroup::new(&alice.provider, &alice.signer, &create_config(), alice.cred.clone()).unwrap();
        let info = g.export_group_info(alice.provider.crypto(), &alice.signer, true).unwrap().to_bytes().unwrap();
        alice.group = Some(g);
        let mut ds = Ds { epoch: 0, info };

        // bob joins externally; alice applies the broadcast
        let mut bob = peer("bob");
        let c1 = join(&mut bob, &mut ds);
        apply(&mut alice, &c1);
        assert_eq!(ds.epoch, 1);
        assert_eq!(alice.group.as_ref().unwrap().epoch().as_u64(), 1);
        assert_eq!(export(&alice), export(&bob));

        // carol joins; both apply
        let mut carol = peer("carol");
        let c2 = join(&mut carol, &mut ds);
        apply(&mut alice, &c2);
        apply(&mut bob, &c2);
        assert_eq!(export(&alice), export(&carol));
        assert_eq!(export(&bob), export(&carol));
        let names: Vec<String> = alice
            .group
            .as_ref()
            .unwrap()
            .members()
            .filter_map(|m| identity_of(&m.credential))
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"carol".to_string()));
        // leaf signature keys are the identity keys
        for m in alice.group.as_ref().unwrap().members() {
            let who = identity_of(&m.credential).unwrap();
            let expect = match who.as_str() {
                "alice" => alice.signer.to_public_vec(),
                "bob" => bob.signer.to_public_vec(),
                _ => carol.signer.to_public_vec(),
            };
            assert_eq!(m.signature_key, expect);
        }

        // alice removes bob (a ghost); carol applies; bob is out
        let bob_idx = alice
            .group
            .as_ref()
            .unwrap()
            .members()
            .find(|m| identity_of(&m.credential).as_deref() == Some("bob"))
            .unwrap()
            .index;
        let (commit, _, gi) = alice
            .group
            .as_mut()
            .unwrap()
            .remove_members(&alice.provider, &alice.signer, &[bob_idx])
            .unwrap();
        assert!(gi.is_some(), "commit bundle carries a GroupInfo with the ratchet tree");
        ds.epoch += 1;
        ds.info = MlsMessageOut::from(gi.unwrap()).to_bytes().unwrap();
        alice.group.as_mut().unwrap().merge_pending_commit(&alice.provider).unwrap();
        let bytes = commit.to_bytes().unwrap();
        apply(&mut carol, &bytes);
        apply(&mut bob, &bytes);
        assert!(!bob.group.as_ref().unwrap().is_active());
        assert_eq!(export(&alice), export(&carol));
        assert_eq!(alice.group.as_ref().unwrap().members().count(), 2);

        // a rejoin after removal works against the new GroupInfo
        let mut bob2 = peer("bob");
        let c4 = join(&mut bob2, &mut ds);
        apply(&mut alice, &c4);
        apply(&mut carol, &c4);
        assert_eq!(export(&alice), export(&bob2));
        assert_eq!(alice.group.as_ref().unwrap().epoch().as_u64(), 4);
    }

    #[test]
    fn stale_group_info_is_rejected_by_the_group() {
        // A joiner working from an old GroupInfo produces a commit the
        // members can't apply — the DS's epoch rule is what stops it, but
        // even without it the merge fails rather than desyncing silently.
        let mut alice = peer("alice");
        let g = MlsGroup::new(&alice.provider, &alice.signer, &create_config(), alice.cred.clone()).unwrap();
        let old_info = g.export_group_info(alice.provider.crypto(), &alice.signer, true).unwrap().to_bytes().unwrap();
        alice.group = Some(g);
        let mut ds = Ds { epoch: 0, info: old_info.clone() };
        let mut bob = peer("bob");
        let c1 = join(&mut bob, &mut ds);
        apply(&mut alice, &c1);
        // carol uses the stale epoch-0 info
        let mut carol = peer("carol");
        let mut stale = Ds { epoch: 0, info: old_info };
        let c_stale = join(&mut carol, &mut stale);
        let g = alice.group.as_mut().unwrap();
        let pm = MlsMessageIn::tls_deserialize_exact(&c_stale).unwrap().try_into_protocol_message().unwrap();
        assert!(g.process_message(&alice.provider, pm).is_err());
    }
}
