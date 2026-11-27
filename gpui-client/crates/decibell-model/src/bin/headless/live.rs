//! The M3 steps of `decibell-headless`: voice, statuses, friends, E2EE, devices, calls,
//! and their waits and dumps (docs/model.md §8 and "M3").

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use decibell_model::ids::Uid;
use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
use decibell_model::live::friends::FriendAction;
use decibell_model::live::presence::{OwnStatus, StatusMessage};
use decibell_model::live::voice::VideoCodec;
use decibell_model::live::{Cue, voice::session::VoiceE2eePhase};
use decibell_model::{Model, actions};
use gpui::{App, AsyncApp, SharedString};
use serde_json::{Value, json};

pub const USAGE: &str = "\
Live steps (voice, people, calls):
  voice-join <channel>            join a voice channel of the open community
  voice-leave
  mute | unmute | deafen | undeafen
  status <online|away|dnd|invisible>
  device-idle <on|off>            this device's idle flag, as auto-away reports it (the account
                                  shows Away only while every device is idle)
  status-message <text> <seconds> our custom status (\"\" clears; 0 s = never clears)
  friends                         ask for the friend list
  friend-add <name> | friend-accept <name> | friend-decline <name> | friend-remove <name>
  e2ee-status                     pull the E2EE status
  e2ee-ready <passphrase>         set encryption up (not set up) or unlock it (locked)
  e2ee-reset <passphrase>         Reset keys: a new identity under a new passphrase (the account's
                                  other devices lock; a pinned peer sees a key change)
  audio-devices <sink>            the voice / call devices: <sink> for output, its monitor for
                                  input (a test's null sink: nothing heard, no real microphone)
  peer-info <user>                one line `DUMP peer-info <json>` (keys, safety number)
  devices                         load the account's device list
  device-sign-out <id | @file>    sign one device out (the id, or a file holding it)
  friends-only <on|off>           Settings → Privacy's friends-only DMs (the account's)
  join <community>                join a public community of central's directory (as Browse's Join)
  audio-devices <input> <output>  the voice / call devices (a test's null sink and its monitor)
  leave <community>               leave one of our communities
  call <user> | answer | decline | hangup
  watch <user>[,<user>...]        register presence interest (as a view showing them); a user is a
                                  name, or #<uid> (a throwaway account another client made)
  watch-roster <community> <n>    interest in the first n held members of a community
  codecs                          probe / read the codec capabilities and ship them
  wait voice <channel> <user>     shown in that voice channel of the open community
  wait voice-gone <channel> <user>
  wait voice-state <user> <muted|unmuted|deafened>   in our session's participants
  wait stream <user> <h264|h265|av1|any> [unlocked]   their stream is live in our voice channel
                                  (in that codec; `unlocked`: announced with no enforced codec)
  wait stream-gone <user>         no stream of theirs in our voice channel
  wait mls <joining|ready|...>    the connected channel's MLS state
  wait e2ee <unavailable|not_set_up|locked|ready>
  wait presence <user> <online|away|dnd|offline>
  wait avatar <user> <version|@file|any|->   their picture's version as the avatars store knows it
                                  (@file: the version a file holds, once it exists; - = no picture)
  wait message <user> <text|->    their custom status (- = none)
  wait friend <user> <status|none>   (a|b: either)
  wait call <idle|outgoing|incoming|connecting|active>   (a|b: either)
  wait incoming-ended             another device of ours took / declined the ringing call
  wait devices                    the device list is loaded
  wait name <user> <name>         the users store names that user so (a rename reached us)";

#[derive(Debug, Clone)]
pub enum LiveStep {
    VoiceJoin(String),
    VoiceLeave,
    Mute(bool),
    Deafen(bool),
    Status(String),
    Idle(bool),
    StatusMessage(String, u32),
    Friends,
    Friend(&'static str, String),
    E2eeStatus,
    E2eeReady(String),
    E2eeReset(String),
    AudioDevices(String),
    PeerInfo(String),
    Devices,
    DeviceSignOut(String),
    Call(String),
    Answer,
    Decline,
    Hangup,
    Watch(Vec<String>),
    WatchRoster(String, usize),
    Codecs,
    FriendsOnly(bool),
    Join(String),
    Leave(String),
    AudioDevices(String, String),
}

#[derive(Debug, Clone)]
pub enum LiveWait {
    Voice { channel: String, user: String, present: bool },
    VoiceState(String, String),
    Stream { user: String, codec: String, unlocked: bool },
    StreamGone(String),
    Mls(String),
    E2ee(String),
    Presence(String, String),
    Message(String, String),
    Friend(String, String),
    Call(String),
    IncomingEnded,
    Devices,
    Avatar(String, String),
    Name(String, String),
}

type Args = std::vec::IntoIter<String>;

fn next(what: &str, it: &mut Args) -> Result<String, String> {
    it.next().ok_or(format!("{what}: missing argument"))
}

/// A live step, or none when `arg` is not one.
pub fn parse(arg: &str, it: &mut Args) -> Result<Option<LiveStep>, String> {
    Ok(Some(match arg {
        "voice-join" => LiveStep::VoiceJoin(next(arg, it)?),
        "voice-leave" => LiveStep::VoiceLeave,
        "mute" => LiveStep::Mute(true),
        "unmute" => LiveStep::Mute(false),
        "deafen" => LiveStep::Deafen(true),
        "undeafen" => LiveStep::Deafen(false),
        "status" => LiveStep::Status(next(arg, it)?),
        "device-idle" => match next(arg, it)?.as_str() {
            "on" => LiveStep::Idle(true),
            "off" => LiveStep::Idle(false),
            other => return Err(format!("device-idle: on or off, not {other:?}")),
        },
        "status-message" => {
            let text = next(arg, it)?;
            let secs = next(arg, it)?.parse().map_err(|_| "status-message: seconds")?;
            LiveStep::StatusMessage(text, secs)
        }
        "friends" => LiveStep::Friends,
        "friend-add" => LiveStep::Friend("add", next(arg, it)?),
        "friend-accept" => LiveStep::Friend("accept", next(arg, it)?),
        "friend-decline" => LiveStep::Friend("decline", next(arg, it)?),
        "friend-remove" => LiveStep::Friend("remove", next(arg, it)?),
        "e2ee-status" => LiveStep::E2eeStatus,
        "e2ee-ready" => LiveStep::E2eeReady(next(arg, it)?),
        "e2ee-reset" => LiveStep::E2eeReset(next(arg, it)?),
        "audio-devices" => LiveStep::AudioDevices(next(arg, it)?),
        "peer-info" => LiveStep::PeerInfo(next(arg, it)?),
        "devices" => LiveStep::Devices,
        "device-sign-out" => LiveStep::DeviceSignOut(next(arg, it)?),
        "call" => LiveStep::Call(next(arg, it)?),
        "answer" => LiveStep::Answer,
        "decline" => LiveStep::Decline,
        "hangup" => LiveStep::Hangup,
        "watch" => LiveStep::Watch(next(arg, it)?.split(',').map(str::to_string).collect()),
        "watch-roster" => {
            let community = next(arg, it)?;
            let n = next(arg, it)?.parse().map_err(|_| "watch-roster: count")?;
            LiveStep::WatchRoster(community, n)
        }
        "codecs" => LiveStep::Codecs,
        "friends-only" => LiveStep::FriendsOnly(match next(arg, it)?.as_str() {
            "on" => true,
            "off" => false,
            other => return Err(format!("friends-only: on or off, not {other:?}")),
        }),
        "join" => LiveStep::Join(next(arg, it)?),
        "leave" => LiveStep::Leave(next(arg, it)?),
        "audio-devices" => LiveStep::AudioDevices(next(arg, it)?, next(arg, it)?),
        _ => return Ok(None),
    }))
}

/// A live `wait` condition, or none.
pub fn parse_wait(what: &str, it: &mut Args) -> Result<Option<LiveWait>, String> {
    let w = |s: &str| format!("wait {s}");
    Ok(Some(match what {
        "voice" | "voice-gone" => LiveWait::Voice {
            channel: next(&w(what), it)?,
            user: next(&w(what), it)?,
            present: what == "voice",
        },
        "voice-state" => LiveWait::VoiceState(next(&w(what), it)?, next(&w(what), it)?),
        "stream" => {
            let user = next(&w(what), it)?;
            let codec = next(&w(what), it)?;
            // An optional last word.
            let unlocked = it.as_slice().first().is_some_and(|a| a == "unlocked");
            if unlocked {
                it.next();
            }
            LiveWait::Stream { user, codec, unlocked }
        }
        "stream-gone" => LiveWait::StreamGone(next(&w(what), it)?),
        "mls" => LiveWait::Mls(next(&w(what), it)?),
        "e2ee" => LiveWait::E2ee(next(&w(what), it)?),
        "presence" => LiveWait::Presence(next(&w(what), it)?, next(&w(what), it)?),
        "message" => LiveWait::Message(next(&w(what), it)?, next(&w(what), it)?),
        "friend" => LiveWait::Friend(next(&w(what), it)?, next(&w(what), it)?),
        "call" => LiveWait::Call(next(&w(what), it)?),
        "incoming-ended" => LiveWait::IncomingEnded,
        "devices" => LiveWait::Devices,
        "avatar" => LiveWait::Avatar(next(&w(what), it)?, next(&w(what), it)?),
        "name" => LiveWait::Name(next(&w(what), it)?, next(&w(what), it)?),
        _ => return Ok(None),
    }))
}

thread_local! {
    static CUES: RefCell<Vec<Cue>> = const { RefCell::new(Vec::new()) };
}

/// Record the cues (the sounds the app would play) for `dump cues`.
pub fn install(model: &Model, cx: &mut App) {
    cx.subscribe(&model.live.cues, |_, cue: &Cue, _| CUES.with(|c| c.borrow_mut().push(*cue))).detach();
}

/// A user by name: the users store, else the stack's accounts (their uids are fixed); `#<uid>`
/// names one by uid.
fn uid_of(model: &Model, name: &str, cx: &App) -> Option<Uid> {
    if let Some(uid) = name.strip_prefix('#') {
        return uid.parse().ok();
    }
    if let Some((uid, _)) = model.users.read(cx).iter().find(|(_, n)| n.as_ref() == name) {
        return Some(uid);
    }
    let dev_dir = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/decibell-dev")))?;
    let text = std::fs::read_to_string(dev_dir.join("run/accounts.json")).ok()?;
    let accounts: Value = serde_json::from_str(&text).ok()?;
    accounts["accounts"].as_array()?.iter().find(|a| a["name"] == name)?["uid"].as_i64()
}

fn need_uid(model: &Model, name: &str, cx: &App) -> Result<Uid, String> {
    uid_of(model, name, cx).ok_or(format!("no user named {name:?}"))
}

pub async fn run(step: &LiveStep, model: &Model, timeout: Duration, cx: &mut AsyncApp) -> Result<(), String> {
    match step {
        LiveStep::VoiceJoin(name) => {
            let target = cx.update(|cx| {
                let server = model.nav.read(cx).active_server.ok_or("voice-join: no community is open")?;
                let community = model.community(server, cx).ok_or("voice-join: that community has not answered")?;
                let id = community
                    .channels
                    .read(cx)
                    .list()
                    .iter()
                    .find(|c| &*c.name == name.as_str())
                    .map(|c| c.id.clone())
                    .ok_or(format!("voice-join: no channel named {name:?}"))?;
                Ok::<_, String>((server, id))
            })?;
            let joined = cx.update(|cx| actions::voice::join_voice_channel(target.0, target.1, cx));
            joined.await.map_err(|e| format!("voice-join: {e}"))?;
            let connected = cx.update(|cx| model.live.voice.session.read(cx).connected().is_some());
            if !connected {
                return Err("voice-join: not connected (encryption keys not ready?)".into());
            }
        }
        LiveStep::VoiceLeave => cx.update(actions::voice::leave_voice_channel),
        LiveStep::Mute(on) => {
            let on = *on;
            cx.update(|cx| actions::voice::set_mute(on, cx))
        }
        LiveStep::Deafen(on) => {
            let on = *on;
            cx.update(|cx| actions::voice::set_deafen(on, cx))
        }
        LiveStep::Status(s) => {
            let status = match s.as_str() {
                "online" => OwnStatus::Online,
                "away" => OwnStatus::Away,
                "dnd" => OwnStatus::Dnd,
                "invisible" => OwnStatus::Invisible,
                other => return Err(format!("status: unknown {other:?}")),
            };
            cx.update(|cx| actions::presence::choose_status(status, cx));
        }
        LiveStep::Idle(idle) => {
            let idle = *idle;
            cx.update(|cx| actions::presence::report_idle(idle, cx));
        }
        LiveStep::StatusMessage(text, secs) => {
            let message = StatusMessage::of(text, "");
            let secs = *secs;
            cx.update(|cx| actions::presence::save_status_message(message, secs, cx));
        }
        LiveStep::Friends => cx.update(actions::friends::request_friend_list),
        LiveStep::Friend(kind, name) => {
            if *kind == "add" {
                let (tx, rx) = futures::channel::oneshot::channel();
                cx.update(|cx| actions::friends::add_friend(name.clone(), cx, move |r, _| drop(tx.send(r))));
                rx.await.map_err(|_| "friend-add: dropped")?.map_err(|e| format!("friend-add: {e}"))?;
            } else {
                let action = match *kind {
                    "accept" => FriendAction::Accept,
                    "decline" => FriendAction::Reject,
                    _ => FriendAction::Remove,
                };
                cx.update(|cx| {
                    let uid = need_uid(model, name, cx)?;
                    actions::friends::act(action, uid, name.clone().into(), cx);
                    Ok::<_, String>(())
                })?;
            }
        }
        LiveStep::E2eeStatus => cx.update(actions::e2ee::pull_status),
        LiveStep::E2eeReady(passphrase) => {
            let started = Instant::now();
            loop {
                let status = cx.update(|cx| model.live.e2ee.read(cx).status);
                match status {
                    E2eeStatus::Ready => break,
                    E2eeStatus::NotSetUp | E2eeStatus::Locked => {
                        let busy = cx.update(|cx| model.live.e2ee.read(cx).passphrase.as_ref().is_some_and(|f| f.busy));
                        if let Some(err) = cx.update(|cx| model.live.e2ee.read(cx).passphrase.as_ref().and_then(|f| f.error.clone())) {
                            return Err(format!("e2ee-ready: {err}"));
                        }
                        if !busy {
                            let mode = if status == E2eeStatus::Locked { PassphraseMode::Unlock } else { PassphraseMode::Setup };
                            cx.update(|cx| {
                                model.live.e2ee.update(cx, |e, cx| e.open(mode, cx));
                                actions::e2ee::submit_passphrase(passphrase.clone(), passphrase.clone(), cx);
                            });
                        }
                    }
                    E2eeStatus::Unavailable => {}
                }
                if started.elapsed() > timeout {
                    return Err(format!("e2ee-ready: still {status:?}"));
                }
                cx.background_executor().timer(Duration::from_millis(50)).await;
            }
        }
        LiveStep::E2eeReset(passphrase) => {
            let before = cx.update(|cx| model.live.e2ee.read(cx).key_id);
            cx.update(|cx| {
                model.live.e2ee.update(cx, |e, cx| e.open(PassphraseMode::Reset, cx));
                actions::e2ee::submit_passphrase(passphrase.clone(), passphrase.clone(), cx);
            });
            let started = Instant::now();
            loop {
                let (status, key_id, flow) = cx.update(|cx| {
                    let e = model.live.e2ee.read(cx);
                    (e.status, e.key_id, e.passphrase.clone())
                });
                if let Some(err) = flow.as_ref().and_then(|f| f.error.clone()) {
                    return Err(format!("e2ee-reset: {err}"));
                }
                if flow.is_none() && status == E2eeStatus::Ready && key_id != before {
                    crate::say(format!("E2EE-RESET key {key_id}"));
                    break;
                }
                if started.elapsed() > timeout {
                    return Err(format!("e2ee-reset: still {status:?}, key {key_id}"));
                }
                cx.background_executor().timer(Duration::from_millis(50)).await;
            }
        }
        LiveStep::AudioDevices(sink) => cx.update(|cx| {
            actions::voice::set_input_device(Some(format!("{sink}.monitor")), cx);
            actions::voice::set_output_device(Some(sink.clone()), cx);
        }),
        LiveStep::PeerInfo(name) => {
            let uid = cx.update(|cx| need_uid(model, name, cx))?;
            let (tx, rx) = futures::channel::oneshot::channel();
            cx.update(|cx| actions::e2ee::peer_info(uid, cx, move |r, _| drop(tx.send(r))));
            let info = rx.await.map_err(|_| "peer-info: dropped")?.map_err(|e| format!("peer-info: {e}"))?;
            crate::say(format!(
                "DUMP peer-info {}",
                json!({ "uid": uid, "hasKeys": info.has_keys, "keyId": info.key_id, "fingerprint": info.fingerprint,
                        "safetyNumber": info.safety_number, "pinned": info.pinned, "changedAt": info.changed_at })
            ));
        }
        LiveStep::Devices => cx.update(|cx| actions::devices::watch(true, cx)),
        LiveStep::DeviceSignOut(arg) => {
            let text = match arg.strip_prefix('@') {
                Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
                None => arg.clone(),
            };
            let id: i64 = text.trim().parse().map_err(|_| format!("device-sign-out: bad id {text:?}"))?;
            cx.update(|cx| actions::devices::sign_out(vec![id], false, cx));
        }
        LiveStep::Call(name) => {
            let uid = cx.update(|cx| need_uid(model, name, cx))?;
            cx.update(|cx| actions::calls::start_call(uid, cx));
        }
        LiveStep::Answer => cx.update(actions::calls::accept_call),
        LiveStep::Decline => cx.update(actions::calls::decline_call),
        LiveStep::Hangup => cx.update(|cx| actions::calls::end_call(Some("Call ended"), true, cx)),
        LiveStep::Watch(names) => {
            let uids = cx.update(|cx| names.iter().map(|n| need_uid(model, n, cx)).collect::<Result<Vec<_>, _>>())?;
            cx.update(|cx| actions::presence::retain(uids, cx));
        }
        LiveStep::WatchRoster(community, n) => {
            let uids: Vec<Uid> = cx.update(|cx| {
                let id = model.servers.read(cx).servers().iter().find(|s| &*s.name == community.as_str()).map(|s| s.id);
                let c = id.and_then(|id| model.community(id, cx)).ok_or(format!("watch-roster: no {community:?}"))?;
                let mut uids: Vec<Uid> = c.roster.read(cx).members().keys().copied().filter(|u| *u > 0).collect();
                uids.sort_unstable();
                uids.truncate(*n);
                Ok::<_, String>(uids)
            })?;
            crate::say(format!("WATCHING {}", uids.len()));
            cx.update(|cx| actions::presence::retain(uids, cx));
        }
        LiveStep::Codecs => cx.update(|cx| actions::voice::init_codec_caps(false, cx)),
        LiveStep::FriendsOnly(on) => {
            let on = *on;
            cx.update(|cx| actions::dm::set_friends_only_dms(on, cx));
        }
        LiveStep::Join(name) => {
            // The directory arrives after the sign-in (`request_server_list`).
            let started = Instant::now();
            let server = loop {
                let found = cx.update(|cx| {
                    model.servers.read(cx).discoverable().iter().find(|s| &*s.name == name.as_str()).map(|s| (s.id, s.host.to_string(), s.port))
                });
                if let Some(found) = found {
                    break found;
                }
                if started.elapsed() > timeout {
                    return Err(format!("join: {name:?} is not in central's directory"));
                }
                cx.background_executor().timer(Duration::from_millis(50)).await;
            };
            let (tx, rx) = futures::channel::oneshot::channel();
            cx.update(|cx| actions::servers::connect(server.0, server.1, server.2, cx, move |r, _| drop(tx.send(r))));
            rx.await.map_err(|_| "join: no answer".to_string())?.map_err(|e| format!("join {name:?}: {e}"))?;
        }
        LiveStep::AudioDevices(input, output) => {
            let (input, output) = (input.clone(), output.clone());
            cx.update(|cx| {
                actions::voice::set_input_device(Some(input), cx);
                actions::voice::set_output_device(Some(output), cx);
            });
        }
        LiveStep::Leave(name) => {
            let id = cx.update(|cx| model.servers.read(cx).servers().iter().find(|s| &*s.name == name.as_str()).map(|s| s.id));
            let id = id.ok_or(format!("leave: {name:?} is not one of our communities"))?;
            cx.update(|cx| actions::servers::leave(id, cx));
        }
    }
    Ok(())
}

pub fn holds(wait: &LiveWait, model: &Model, cx: &App) -> bool {
    let me = model.auth.read(cx).uid;
    match wait {
        LiveWait::Voice { channel, user, present } => {
            let Some(server) = model.nav.read(cx).active_server else { return false };
            let Some(community) = model.community(server, cx) else { return false };
            let Some(id) = community.channels.read(cx).list().iter().find(|c| &*c.name == channel.as_str()).map(|c| c.id.clone())
            else {
                return false;
            };
            let Some(uid) = uid_of(model, user, cx) else { return false };
            model.live.voice.rosters.read(cx).roster(&(server, id)).contains(&uid) == *present
        }
        LiveWait::VoiceState(user, state) => {
            let Some(uid) = uid_of(model, user, cx) else { return false };
            model.live.voice.participants.read(cx).list().iter().any(|p| {
                p.uid == uid
                    && match state.as_str() {
                        "muted" => p.state.muted,
                        "unmuted" => !p.state.muted,
                        "deafened" => p.state.deafened,
                        "undeafened" => !p.state.deafened,
                        _ => false,
                    }
            })
        }
        LiveWait::Stream { user, codec, unlocked } => {
            let Some(uid) = uid_of(model, user, cx) else { return false };
            let session = model.live.voice.session.read(cx);
            let streams = model.live.voice.streams.read(cx).session(session.connected().as_ref(), session.call_peer.is_some());
            streams.iter().any(|s| {
                s.owner_uid == uid
                    && match codec.as_str() {
                        "h264" => matches!(s.current_codec, VideoCodec::H264Hw | VideoCodec::H264Sw),
                        "h265" => s.current_codec == VideoCodec::H265,
                        "av1" => s.current_codec == VideoCodec::Av1,
                        _ => true,
                    }
                    && (!*unlocked || s.enforced_codec == VideoCodec::Unknown)
            })
        }
        LiveWait::StreamGone(user) => {
            let Some(uid) = uid_of(model, user, cx) else { return false };
            let session = model.live.voice.session.read(cx);
            let streams = model.live.voice.streams.read(cx).session(session.connected().as_ref(), session.call_peer.is_some());
            !streams.iter().any(|s| s.owner_uid == uid)
        }
        LiveWait::Mls(phase) => model.live.voice.session.read(cx).e2ee.as_ref().is_some_and(|e| e.phase == VoiceE2eePhase::parse(phase)),
        LiveWait::E2ee(s) => E2eeStatus::parse(s) == model.live.e2ee.read(cx).status,
        LiveWait::Presence(user, s) => uid_of(model, user, cx).is_some_and(|uid| {
            let shown = model.live.presence.display(me, uid, false, cx);
            serde_json::to_value(shown).ok().and_then(|v| v.as_str().map(str::to_string)).as_deref() == Some(s.as_str())
        }),
        LiveWait::Message(user, text) => uid_of(model, user, cx).is_some_and(|uid| {
            let m = model.live.presence.message(me, uid, cx).map(|m| m.text.to_string());
            if text == "-" { m.is_none() } else { m.as_deref() == Some(text.as_str()) }
        }),
        LiveWait::Friend(user, s) => uid_of(model, user, cx).is_some_and(|uid| {
            let f = model.live.friends.read(cx).get(uid).map(|f| f.status);
            let shown = match f {
                None => Some("none".to_string()),
                Some(st) => serde_json::to_value(st).ok().and_then(|v| v.as_str().map(str::to_string)),
            };
            s.split('|').any(|want| shown.as_deref() == Some(want))
        }),
        LiveWait::Call(s) => {
            let status = serde_json::to_value(model.live.calls.read(cx).status).ok().and_then(|v| v.as_str().map(str::to_string));
            s.split('|').any(|want| status.as_deref() == Some(want))
        }
        LiveWait::IncomingEnded => model.live.calls.read(cx).incoming_ended.is_some(),
        LiveWait::Devices => model.live.devices.read(cx).list.is_some(),
        LiveWait::Name(user, name) => {
            uid_of(model, user, cx).is_some_and(|uid| model.name_of(uid, &SharedString::default(), cx).as_ref() == name.as_str())
        }
        LiveWait::Avatar(user, want) => uid_of(model, user, cx).is_some_and(|uid| {
            let avatars = model.live.presence.avatars.read(cx);
            let version = avatars.get(uid).map(|e| e.version.to_string()).unwrap_or_default();
            match want.as_str() {
                "-" => version.is_empty(),
                "any" => !version.is_empty(),
                file if file.starts_with('@') => {
                    std::fs::read_to_string(&file[1..]).is_ok_and(|v| !v.trim().is_empty() && v.trim() == version)
                }
                exact => version == exact,
            }
        }),
    }
}

/// `dump codecs`: the core's encoder probe, run now (`probe_native_encoders`: what Go Live
/// offers on this machine, each codec with its backend, FFmpeg encoder and limits;
/// `DECIBELL_ENCODER` leaves one backend), and the codec caps the model holds (the `codecs`
/// step loads them: `encoders` with each one's backend, label and limits). One line
/// `DUMP codecs <json>`.
pub async fn dump_codecs(model: &Model, cx: &mut AsyncApp) -> Result<(), String> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let encoders: Vec<Value> = decibell_core::api::streaming::probe_native_encoders()
        .await
        .map_err(|e| format!("dump codecs: the encoder probe failed: {e}"))?
        .iter()
        .map(|c| {
            json!({
                "codec": VideoCodec::from_wire(c.codec).human_name(), "backend": c.backend, "label": c.label,
                "encoder": c.encoder_name, "hardware": c.hardware, "maxWidth": c.max_width,
                "maxHeight": c.max_height, "maxFps": c.max_fps, "maxPixelRate": c.max_pixel_rate,
            })
        })
        .collect();
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let encoders: Vec<Value> = Vec::new();
    let caps = cx.update(|cx| {
        let c = model.live.voice.codecs.read(cx);
        json!({ "encode": c.encode, "decode": c.decode, "encoders": c.encoders, "loaded": c.loaded })
    });
    let forced = std::env::var(decibell_core::media::encoder_backend::FORCE_ENV).ok();
    crate::say(format!("DUMP codecs {}", json!({ "forced": forced, "encoders": encoders, "caps": caps })));
    Ok(())
}

/// A live `dump`, or none when `what` is not one.
pub fn dump(what: &str, model: &Model, cx: &App) -> Option<Value> {
    let s = |v: &SharedString| v.to_string();
    let live = &model.live;
    Some(match what {
        "voice" => {
            let session = live.voice.session.read(cx);
            let rosters = live.voice.rosters.read(cx);
            let mut channels: Vec<Value> = rosters
                .keys()
                .map(|k| {
                    let c = rosters.channel(k).unwrap();
                    json!({ "server": k.0, "channel": s(&k.1), "roster": c.roster,
                            "states": c.roster.iter().map(|u| json!([u, c.state(*u)])).collect::<Vec<_>>() })
                })
                .collect();
            channels.sort_by_key(|v| v.to_string());
            let streams: Vec<Value> = live
                .voice
                .streams
                .read(cx)
                .all()
                .map(|(uid, l)| json!({ "owner": uid, "server": l.server, "channel": s(&l.channel), "info": l.info }))
                .collect();
            json!({
                "server": session.server, "channel": session.channel.as_ref().map(s), "callPeer": session.call_peer,
                "muted": session.muted, "deafened": session.deafened, "error": session.error.as_ref().map(s),
                "e2ee": session.e2ee,
                "participants": live.voice.participants.read(cx).list(),
                "speaking": live.voice.speaking.read(cx).speakers().collect::<Vec<_>>(),
                "latencyMs": live.voice.stats.read(cx).latency_ms,
                "statsSamples": live.voice.stats.read(cx).samples().len(),
                "pendingMoves": rosters.has_pending_moves(),
                "channels": channels, "streams": streams,
                "watching": live.voice.watching.read(cx).list(),
                "ownStream": live.voice.own_stream.read(cx).phase,
                "codecs": { "encode": live.voice.codecs.read(cx).encode, "decode": live.voice.codecs.read(cx).decode,
                            "loaded": live.voice.codecs.read(cx).loaded },
            })
        }
        "presence" => {
            let p = live.presence.statuses.read(cx);
            let mut known: Vec<Uid> = p.known().collect();
            known.sort_unstable();
            let me = model.auth.read(cx).uid;
            json!({
                "known": known.iter().map(|u| json!([u, live.presence.display(me, *u, false, cx),
                    live.presence.message(me, *u, cx).map(|m| m.text.to_string())])).collect::<Vec<_>>(),
                "watched": live.presence.interest.read(cx).watched(),
                "avatarsLoaded": live.presence.avatars.read(cx).loaded(),
            })
        }
        "own" => {
            let o = live.presence.own.read(cx);
            json!({ "supported": o.supported, "status": o.status, "display": o.display(), "idle": o.idle,
                    "accountIdle": o.account_idle, "message": o.message, "clears": o.clears_at.is_some() })
        }
        "friends" => {
            let f = live.friends.read(cx);
            json!({ "friends": f.list(), "error": f.last_action_error.as_ref().map(s), "waiting": f.waiting_for_list() })
        }
        "e2ee" => {
            let e = live.e2ee.read(cx);
            json!({ "supported": e.supported, "status": e.status, "keyId": e.key_id, "fingerprint": s(&e.fingerprint),
                    "prompt": e.passphrase.as_ref().map(|f| json!({"mode": f.mode, "busy": f.busy, "error": f.error.as_ref().map(s)})) })
        }
        "call" => {
            let c = live.calls.read(cx);
            json!({
                "status": c.status, "callId": c.call_id.as_ref().map(s), "peer": c.peer, "isCaller": c.is_caller,
                "ringingAcked": c.ringing_acked, "path": c.path, "verified": c.verified,
                "endReason": c.end_reason.as_ref().map(s), "incomingEnded": c.incoming_ended,
                "signaling": c.call_signaling, "ringArmed": c.ring_armed(),
            })
        }
        "transfers" => {
            let t = live.transfers.read(cx);
            json!({ "downloads": t.downloads().len(), "uploads": t.uploads().len(), "progress": t.progress(),
                    "attention": t.attention, "panelOpen": t.panel_open })
        }
        "devices" => {
            let d = live.devices.read(cx);
            json!({ "revision": d.revision, "list": d.list, "error": d.error.as_ref().map(s), "busy": d.busy })
        }
        "hotkeys" => {
            let h = live.hotkeys.read(cx);
            json!({ "status": h.status, "recording": h.recording, "fallback": h.fallback_armed() })
        }
        "cues" => {
            let cues: Vec<String> = CUES.with(|c| c.borrow().iter().map(|c| format!("{c:?}")).collect());
            json!(cues)
        }
        _ => return None,
    })
}
