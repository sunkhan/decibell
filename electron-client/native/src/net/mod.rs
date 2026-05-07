// Wire-protocol layer. Mirrors `tauri-client/src-tauri/src/net/`.
//
// proto, framing, tls, connection are all dependency-pure: zero
// references to Tauri or to AppState — they came over verbatim. central
// and community embed business logic (route_packets emits app events,
// reconnect mutates AppState) and are ported in PR3 trimmed to just
// the auth flow; later PRs grow each route_packets to handle their
// new packet types.

pub mod central;
pub mod community;
pub mod connection;
pub mod framing;
pub mod proto;
pub mod tls;
