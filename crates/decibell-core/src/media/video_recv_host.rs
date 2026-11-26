//! When the video receive thread (`decibell-video-recv`) runs.
//!
//! It reassembles and decrypts watched streams, answers the relay's bind
//! challenges on the media socket and pings it, and hands a streamer its
//! watchers' keyframe requests. In a community voice session none of that
//! has a use until the user watches a stream or streams, so the thread runs
//! only while the watch set (`WATCHED_STREAMS`) is non-empty or a
//! `VideoEngine` streams on this session's media socket (it holds a
//! [`VideoRecvDemand`]); a session with neither has no such thread. A P2P
//! call keeps it for the whole call: the punched media path lives on its
//! PINGs (and the peer's, reflected), or the NAT mapping expires and a screen
//! share later in the call can't get through.
//!
//! A started thread pings the relay at once. [`VideoRecvHost::bound`] lets
//! `watch_stream` wait (bounded) for the echo, i.e. for the relay to have
//! bound this endpoint, before it asks the server for the stream: the first
//! keyframe the server requests for a new watcher then reaches us instead of
//! an endpoint that isn't bound yet (or is stale after a NAT rebinding).
//!
//! Every change goes through [`VideoRecvHost::reconcile`] under the host's
//! lock and reads the watch set fresh, so concurrent watches, stream starts /
//! stops and a session ending leave the thread running exactly when wanted.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Instant;

use super::frame_crypto::SharedKeyRing;
use super::media_socket::MediaSocket;
use super::pipeline::VoiceEvent;
use super::udp_recv::Waker;

/// What a thread is started with.
pub(crate) struct VideoRecvParams {
    pub socket: Arc<MediaSocket>,
    pub sender_id: String,
    pub ring: Option<SharedKeyRing>,
    /// Presentation timestamps run from here across restarts of the thread.
    pub ts_epoch: Instant,
    /// P2P call: run for the whole session.
    pub always: bool,
}

/// One running (or stopping) thread.
struct Run {
    handle: JoinHandle<()>,
    stop: Arc<AtomicBool>,
    waker: Arc<Waker>,
    bound: tokio::sync::watch::Receiver<bool>,
}

impl Run {
    fn signal_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.waker.wake();
    }
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

struct State {
    /// VideoEngines streaming on this session's media socket.
    streaming: usize,
    /// The session ended: never start again.
    shut_down: bool,
    /// The voice event bridge's sender; dropped at shutdown so the bridge
    /// (which ends when every sender is gone) can end.
    event_tx: Option<mpsc::Sender<VoiceEvent>>,
    running: Option<Run>,
}

pub(crate) struct VideoRecvHost {
    params: VideoRecvParams,
    /// Whether anything is watched (the global set; tests inject their own).
    watching: Box<dyn Fn() -> bool + Send + Sync>,
    state: Mutex<State>,
}

/// Hosts of live sessions, for watch-set changes to reach.
static HOSTS: Mutex<Vec<Weak<VideoRecvHost>>> = Mutex::new(Vec::new());

/// The watch set changed: start or stop each live session's thread. Called by
/// the watch-set writers, serialised by their lock.
pub(crate) fn reconcile_all() {
    let hosts: Vec<Arc<VideoRecvHost>> = {
        let mut list = HOSTS.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|w| w.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    };
    for host in hosts {
        host.reconcile();
    }
}

impl VideoRecvHost {
    /// A host for one voice session, registered for watch-set changes, its
    /// thread started if it is wanted already.
    pub(crate) fn start(
        params: VideoRecvParams,
        event_tx: mpsc::Sender<VoiceEvent>,
        watching: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Arc<VideoRecvHost> {
        let host = Arc::new(VideoRecvHost {
            params,
            watching,
            state: Mutex::new(State { streaming: 0, shut_down: false, event_tx: Some(event_tx), running: None }),
        });
        HOSTS.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(&host));
        host.reconcile();
        host
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start the thread when it is wanted and isn't running, stop it when it
    /// runs and isn't wanted.
    pub(crate) fn reconcile(&self) {
        let mut st = self.lock();
        let wanted = !st.shut_down && (self.params.always || st.streaming > 0 || (self.watching)());
        let live = st.running.as_ref().is_some_and(|r| !r.stopping());
        if wanted && !live {
            // A thread told to stop exits at once (it was woken); join it
            // before another reads the socket.
            if let Some(old) = st.running.take() {
                let _ = old.handle.join();
            }
            st.running = self.spawn(&st);
        } else if !wanted && live {
            if let Some(r) = st.running.as_ref() {
                r.signal_stop();
            }
        }
    }

    fn spawn(&self, st: &State) -> Option<Run> {
        let event_tx = st.event_tx.clone()?;
        let waker = match Waker::new() {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("[video-recv] waker: {}", e);
                let _ = event_tx.send(VoiceEvent::Error(format!(
                    "Video receive couldn't start ({}) — screen share playback is unavailable",
                    e
                )));
                return None;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (bound_tx, bound) = tokio::sync::watch::channel(false);
        let p = &self.params;
        let (socket, sender_id, ring, ts_epoch) = (p.socket.clone(), p.sender_id.clone(), p.ring.clone(), p.ts_epoch);
        let (stop_t, waker_t) = (stop.clone(), waker.clone());
        let spawned = std::thread::Builder::new().name("decibell-video-recv".to_string()).spawn(move || {
            // Guard the receive loop like the audio/encoder threads. It
            // processes fully untrusted network input: a panic here
            // (crafted packet, future invariant change) would unwind and
            // silently kill video for every watched stream with no error
            // and no restart. Catch it and surface an actionable message.
            let panic_tx = event_tx.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::run_video_recv_thread(socket, sender_id, event_tx, stop_t, waker_t, ring, ts_epoch, bound_tx);
            }));
            if let Err(e) = outcome {
                let msg = e
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| e.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown error".to_string());
                log::error!("[video-recv] thread panicked: {}", msg);
                let _ = panic_tx.send(VoiceEvent::Error(format!(
                    "Video receive crashed ({}) — screen share playback stopped",
                    msg
                )));
            }
        });
        match spawned {
            Ok(handle) => Some(Run { handle, stop, waker, bound }),
            Err(e) => {
                log::error!("[video-recv] spawn: {}", e);
                None
            }
        }
    }

    /// A stream started on this session's media socket.
    pub(crate) fn demand(self: &Arc<Self>) -> VideoRecvDemand {
        self.lock().streaming += 1;
        self.reconcile();
        VideoRecvDemand(self.clone())
    }

    /// The running thread's "the relay has bound our media endpoint" flag
    /// (true once our PING came back), or None when no thread runs.
    pub(crate) fn bound(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        let st = self.lock();
        st.running.as_ref().filter(|r| !r.stopping()).map(|r| r.bound.clone())
    }

    /// Whether a thread is running (not told to stop).
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.lock().running.as_ref().is_some_and(|r| !r.stopping() && !r.handle.is_finished())
    }

    /// The session ended: stop the thread for good and join it, and let go
    /// of the event sender.
    pub(crate) fn shut_down(&self) {
        let run = {
            let mut st = self.lock();
            st.shut_down = true;
            st.event_tx = None;
            st.running.take()
        };
        if let Some(r) = run {
            r.signal_stop();
            let _ = r.handle.join();
        }
    }
}

/// Keeps the video receive thread of one session running while a stream is
/// sent on its media socket (held by the `VideoEngine`).
pub struct VideoRecvDemand(Arc<VideoRecvHost>);

impl Drop for VideoRecvDemand {
    fn drop(&mut self) {
        {
            let mut st = self.0.lock();
            st.streaming = st.streaming.saturating_sub(1);
        }
        self.0.reconcile();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::time::Duration;

    /// A host over a Plain socket to a fake relay, with its own "anything
    /// watched" switch (the global set is shared by every test).
    fn host(always: bool) -> (Arc<VideoRecvHost>, UdpSocket, Arc<AtomicBool>, mpsc::Receiver<VoiceEvent>) {
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(relay.local_addr().unwrap()).unwrap();
        relay.connect(sock.local_addr().unwrap()).unwrap();
        let watching = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::channel();
        let w = watching.clone();
        let host = VideoRecvHost::start(
            VideoRecvParams {
                socket: Arc::new(MediaSocket::plain(sock, "7")),
                sender_id: "7".into(),
                ring: None,
                ts_epoch: Instant::now(),
                always,
            },
            event_tx,
            Box::new(move || w.load(Ordering::Relaxed)),
        );
        (host, relay, watching, event_rx)
    }

    /// The relay's next datagram: our PING (type 5).
    fn pinged(relay: &UdpSocket) -> bool {
        let mut b = [0u8; 2048];
        matches!(relay.recv(&mut b), Ok(n) if n > 0 && b[0] == 5)
    }

    fn drain(relay: &UdpSocket) {
        relay.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let mut b = [0u8; 2048];
        while relay.recv(&mut b).is_ok() {}
        relay.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    }

    /// No thread (and nothing on the media socket) until something needs
    /// it; a watch or a stream starts it, the last of them stops it, and a
    /// session that ended never starts it again.
    #[test]
    fn runs_only_while_watched_or_streaming() {
        let (host, relay, watching, _events) = host(false);
        assert!(!host.is_running());
        assert!(host.bound().is_none());
        assert!(!pinged(&relay), "an idle session pings nothing");

        watching.store(true, Ordering::Relaxed);
        host.reconcile();
        assert!(host.is_running());
        assert!(pinged(&relay), "a started thread pings the relay at once");
        let mut bound = host.bound().unwrap();
        assert!(!*bound.borrow());
        // The relay echoes (it has bound us): the flag watch_stream waits on.
        relay.send(&super::super::packet::UdpAudioPacket::new_ping("7", 1).to_bytes()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), bound.wait_for(|b| *b)).await })
            .expect("bound after the echo")
            .unwrap();

        watching.store(false, Ordering::Relaxed);
        host.reconcile();
        assert!(!host.is_running());
        drain(&relay);
        assert!(!pinged(&relay));

        // A stream keeps it (two, the last one's drop stops it); a watch
        // coming and going meanwhile changes nothing.
        let a = host.demand();
        assert!(host.is_running());
        let b = host.demand();
        watching.store(true, Ordering::Relaxed);
        host.reconcile();
        watching.store(false, Ordering::Relaxed);
        host.reconcile();
        assert!(host.is_running());
        drop(a);
        assert!(host.is_running());
        drop(b);
        assert!(!host.is_running());

        // Ended: neither a watch nor a stream starts it again.
        host.shut_down();
        watching.store(true, Ordering::Relaxed);
        host.reconcile();
        let c = host.demand();
        assert!(!host.is_running());
        drop(c);
    }

    /// A P2P call's media path lives on its PINGs: the thread runs from the
    /// start to the end of the session, watched or not.
    #[test]
    fn a_call_keeps_it_for_the_whole_session() {
        let (host, relay, _watching, _events) = host(true);
        assert!(host.is_running());
        assert!(pinged(&relay));
        host.shut_down();
        assert!(!host.is_running());
    }

    /// Starts and stops racing from many threads (watches, streams) end in
    /// the state the last of them asked for, with at most one thread.
    #[test]
    fn concurrent_changes_settle_on_the_wanted_state() {
        let (host, _relay, watching, _events) = host(false);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let (host, watching) = (host.clone(), watching.clone());
                std::thread::spawn(move || {
                    for i in 0..30 {
                        if t % 2 == 0 {
                            let d = host.demand();
                            if i % 3 == 0 {
                                std::thread::yield_now();
                            }
                            drop(d);
                        } else {
                            watching.store(i % 2 == 0, Ordering::Relaxed);
                            host.reconcile();
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        watching.store(false, Ordering::Relaxed);
        host.reconcile();
        assert!(!host.is_running());
        let d = host.demand();
        assert!(host.is_running());
        drop(d);
        assert!(!host.is_running());
        host.shut_down();
    }
}
