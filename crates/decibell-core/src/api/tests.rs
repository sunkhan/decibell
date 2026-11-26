//! The api from a client's side: calls made from a thread outside any tokio
//! runtime (a native client's UI thread) on a non-tokio executor, and the
//! error texts and kinds a few refused calls answer with (the Electron
//! renderer shows and matches these texts; they must not change).

use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Wake};
use std::time::{Duration, Instant};

use super::{channels, community, devices, presence, servers, streaming, CoreError, ErrorKind};

/// A minimal executor that is not tokio's: polls on the calling thread and
/// parks between wakes. Fails the test instead of hanging.
fn block_on<F: Future>(fut: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Unpark(std::thread::current())).into();
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
            return out;
        }
        let now = Instant::now();
        assert!(now < deadline, "an api call hung on a non-tokio executor");
        std::thread::park_timeout(deadline - now);
    }
}

/// The runtime handed to `rt::init`, as `lifecycle::init` would: one for the
/// whole test binary (tests under `#[tokio::test]` keep their own, `rt` uses
/// the current runtime first).
fn core_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        crate::rt::init(rt.handle().clone());
        rt
    })
}

/// A loopback port nothing listens on.
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
}

/// Called from a plain thread (no runtime context) and awaited on a
/// non-tokio executor, the api neither panics (tokio timers / sockets need a
/// runtime context: `rt::run` gives the body one) nor hangs.
#[test]
fn api_calls_work_from_a_thread_outside_any_runtime() {
    core_runtime();
    crate::state::init();
    let port = closed_port();
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err(), "the test thread must be outside a runtime");

        // What every async api body relies on: a timer and a socket, run
        // through `rt::run` from here.
        let refused = block_on(crate::rt::run(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .map(|_| ())
                .map_err(|e| CoreError::other(e.to_string()))
        }));
        assert!(refused.is_err(), "nothing listens on {port}");
        // spawn / spawn_blocking from outside a runtime, awaited here.
        assert_eq!(block_on(crate::rt::spawn(async { 7 })).unwrap(), 7);
        assert_eq!(block_on(crate::rt::spawn_blocking(|| 8)).unwrap(), 8);

        // Sync api: no runtime needed at all.
        let parsed = servers::parse_invite_link(servers::ParseInviteLinkArgs {
            url: "decibell://invite/127.0.0.1:8082/abc".into(),
        })
        .expect("a link");
        assert_eq!((parsed.host.as_str(), parsed.port, parsed.code.as_str()), ("127.0.0.1", 8082, "ABC"));

        // Async api answering from state alone.
        let caps = block_on(streaming::get_caps()).expect("caps");
        assert!(caps.encode.is_empty() && caps.decode.is_empty());
        let e = block_on(channels::request_channel_history(channels::RequestChannelHistoryArgs {
            server_id: "9".into(),
            channel_id: "general".into(),
            before_id: 0,
            limit: 50,
            around_id: None,
            after_id: None,
        }))
        .unwrap_err();
        assert_eq!((e.message(), e.kind()), ("Not connected to community 9", ErrorKind::NotConnected));
        let e = block_on(devices::list_devices()).err().expect("refused");
        assert_eq!((e.message(), e.kind()), ("Not signed in", ErrorKind::NotSignedIn));

        // Async api that opens a socket: connecting a community dials it
        // (TcpStream::connect) before anything else; nothing listens.
        crate::state::shared().blocking_lock().token = Some("t".into());
        let e = block_on(servers::connect_to_community(servers::ConnectToCommunityArgs {
            server_id: "9".into(),
            host: "127.0.0.1".into(),
            port: port as i32,
        }))
        .unwrap_err();
        crate::state::shared().blocking_lock().token = None;
        assert!(
            e.message().starts_with(&format!("TCP connect to 127.0.0.1:{port} failed")),
            "{}",
            e.message()
        );
    })
    .join()
    .expect("no panic outside a runtime");
}

/// Inside a runtime (the addon: every command runs on napi's), the body runs
/// inline on it, as the commands always did.
#[tokio::test]
async fn inside_a_runtime_the_body_runs_inline() {
    let here = std::thread::current().id();
    let ran_on = crate::rt::run(async move { Ok(std::thread::current().id()) }).await.unwrap();
    assert_eq!(ran_on, here);
}

#[tokio::test]
async fn argument_errors_keep_their_text() {
    crate::state::init();
    let e = presence::set_status(presence::SetStatusArgs { status: "bogus".into() }).await.unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Unknown status: bogus", ErrorKind::InvalidArgument));

    let e = community::voice_mod(community::VoiceModArgs {
        server_id: "1".into(),
        username: None,
        uid: 2,
        action: "launch".into(),
        channel_id: None,
    })
    .await
    .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Unknown voice_mod action 'launch'", ErrorKind::InvalidArgument));

    let e = servers::redeem_invite(servers::RedeemInviteArgs {
        server_id: "1".into(),
        host: "h".into(),
        port: 8082,
        invite_code: "  ".into(),
    })
    .await
    .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Invite code is empty", ErrorKind::InvalidArgument));
    let e = servers::connect_to_community(servers::ConnectToCommunityArgs {
        server_id: "1".into(),
        host: "h".into(),
        port: 70000,
    })
    .await
    .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Invalid port", ErrorKind::InvalidArgument));
    let e = servers::request_drop_membership(servers::DropMembershipArgs { server_id: "1.5".into() })
        .await
        .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Invalid server_id: 1.5", ErrorKind::InvalidArgument));

    let e = super::call::send_call_signal(super::call::SendCallSignalArgs {
        call_id: "c".into(),
        to: "bob".into(),
        to_uid: 2,
        kind: "WAVE".into(),
        pub_key: None,
        candidates: None,
        stream: None,
    })
    .await
    .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Unknown call signal kind WAVE", ErrorKind::InvalidArgument));

    let e = super::hotkeys::hotkeys_inject(super::hotkeys::HotkeysInjectArgs {
        action: "explode".into(),
        pressed: true,
        source: "cli".into(),
    })
    .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Unknown hotkey action: explode", ErrorKind::InvalidArgument));
}

#[tokio::test]
async fn calls_without_a_connection_say_so() {
    crate::state::init();
    let e = servers::request_server_list().await.unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Not connected to central server", ErrorKind::NotConnected));
    let e = servers::disconnect_from_community(servers::DisconnectFromCommunityArgs { server_id: "4".into() })
        .await
        .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Not connected to community 4", ErrorKind::NotConnected));
    let e = servers::update_server_picture("4".to_string(), vec![1u8, 2, 3]).await.unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Not connected to community server 4", ErrorKind::NotConnected));
    let e = super::voice::set_voice_threshold(super::voice::SetVoiceThresholdArgs { threshold_db: -40.0 })
        .await
        .unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Not in a voice channel", ErrorKind::Other));
    let e = super::call::call_watch_stream(super::call::CallWatchStreamArgs { watch: true }).await.unwrap_err();
    assert_eq!((e.message(), e.kind()), ("Not in a call", ErrorKind::Other));
}
