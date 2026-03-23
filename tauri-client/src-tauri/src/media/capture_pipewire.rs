use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use super::capture::{CaptureConfig, CaptureSource, CaptureSourceType, RawFrame};

/// On Linux, screen/window selection is handled by the XDG Desktop Portal.
/// We return a single placeholder entry; the real picker dialog appears
/// when capture starts (triggered by the portal).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    Ok(vec![CaptureSource {
        id: "portal".to_string(),
        name: "Screen (Portal Picker)".to_string(),
        source_type: CaptureSourceType::Screen,
        width: 0,
        height: 0,
    }])
}

/// Start capturing via XDG Desktop Portal + PipeWire.
/// This triggers the portal picker dialog (blocking until user selects),
/// then starts a PipeWire capture thread that feeds frames through the channel.
pub async fn start_capture(
    _source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let config = config.clone();

    // Run portal dialog + PipeWire setup on a blocking thread
    // (portal dialog blocks until user picks a source)
    let rx = tokio::task::spawn_blocking(move || -> Result<_, String> {
        // Step 1: Portal D-Bus interaction (shows picker dialog)
        let (pw_fd, node_id) = portal_screencast_session()?;

        // Step 2: Start PipeWire capture on a dedicated thread
        let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(4);

        std::thread::Builder::new()
            .name("decibell-capture".to_string())
            .spawn(move || {
                if let Err(e) = pipewire_capture_loop(pw_fd, node_id, tx, config) {
                    log::error!("[capture] PipeWire: {}", e);
                }
            })
            .map_err(|e| format!("Spawn capture thread: {}", e))?;

        Ok(rx)
    })
    .await
    .map_err(|e| format!("Join error: {}", e))??;

    Ok(rx)
}

// ─── XDG Desktop Portal D-Bus interaction ───────────────────────────────────

/// Run the full portal screencast session (blocking).
/// Returns (PipeWire fd, node_id) on success.
fn portal_screencast_session() -> Result<(OwnedFd, u32), String> {
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, Value};

    let conn = Connection::session().map_err(|e| format!("D-Bus session: {}", e))?;

    let sender = conn
        .unique_name()
        .ok_or("No D-Bus unique name")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let pid = std::process::id();

    let sc = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast",
    )
    .map_err(|e| format!("ScreenCast proxy: {}", e))?;

    // ── CreateSession ──
    let create_token = format!("u{}_c", pid);
    let session_token = format!("u{}_s", pid);

    let (code, results) = portal_request(
        &conn,
        &sc,
        &sender,
        &create_token,
        "CreateSession",
        &(HashMap::from([
            ("handle_token", Value::from(create_token.as_str())),
            (
                "session_handle_token",
                Value::from(session_token.as_str()),
            ),
        ]),),
    )?;
    if code != 0 {
        return Err("Failed to create portal session".into());
    }

    let session_val = results
        .get("session_handle")
        .ok_or("No session_handle in response")?;
    // The value is an object path wrapped in OwnedValue
    let session: OwnedObjectPath = session_val
        .clone()
        .try_into()
        .map_err(|e: zbus::zvariant::Error| format!("Parse session_handle: {}", e))?;

    // ── SelectSources (shows portal picker dialog) ──
    let select_token = format!("u{}_sel", pid);

    let (code, _) = portal_request(
        &conn,
        &sc,
        &sender,
        &select_token,
        "SelectSources",
        &(
            &session,
            HashMap::from([
                ("handle_token", Value::from(select_token.as_str())),
                ("types", Value::from(3u32)), // MONITOR | WINDOW
                ("multiple", Value::from(false)),
            ]),
        ),
    )?;
    if code != 0 {
        return Err("Screen selection was cancelled".into());
    }

    // ── Start ──
    let start_token = format!("u{}_st", pid);

    let (code, results) = portal_request(
        &conn,
        &sc,
        &sender,
        &start_token,
        "Start",
        &(
            &session,
            "",
            HashMap::from([("handle_token", Value::from(start_token.as_str()))]),
        ),
    )?;
    if code != 0 {
        return Err("Stream start was rejected".into());
    }

    // Extract PipeWire node_id from streams: a(ua{sv})
    let node_id = extract_node_id(&results)?;

    // ── OpenPipeWireRemote ──
    let reply = sc
        .call_method("OpenPipeWireRemote", &(&session, HashMap::<&str, Value>::new()))
        .map_err(|e| format!("OpenPipeWireRemote: {}", e))?;

    let fd: zbus::zvariant::OwnedFd = reply
        .body()
        .deserialize()
        .map_err(|e| format!("Parse PipeWire fd: {}", e))?;

    Ok((fd.into(), node_id))
}

/// Make a portal method call and wait for the Response signal.
/// The XDG portal uses a request/response pattern: each method returns a
/// request object path, and the actual result arrives via a Response signal.
fn portal_request(
    conn: &zbus::blocking::Connection,
    sc: &zbus::blocking::Proxy,
    sender: &str,
    token: &str,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> Result<(u32, HashMap<String, zbus::zvariant::OwnedValue>), String> {
    let request_path = format!(
        "/org/freedesktop/portal/desktop/request/{}/{}",
        sender, token
    );

    // Subscribe to Response signal BEFORE making the call (avoid race)
    let req_proxy = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(|e| format!("{} request proxy: {}", method, e))?;

    let mut signals = req_proxy
        .receive_signal("Response")
        .map_err(|e| format!("{} signal subscribe: {}", method, e))?;

    // Make the actual portal call
    sc.call_method(method, body)
        .map_err(|e| format!("{}: {}", method, e))?;

    // Wait for response (blocks until portal dialog completes for SelectSources)
    let signal = signals.next().ok_or_else(|| format!("{}: signal stream ended", method))?;

    let body = signal.body();
    let (code, results): (u32, HashMap<String, zbus::zvariant::OwnedValue>) = body
        .deserialize()
        .map_err(|e| format!("{} parse response: {}", method, e))?;

    Ok((code, results))
}

/// Extract the PipeWire node_id from the Start response's "streams" field.
/// streams is D-Bus type a(ua{sv}) — array of (node_id, properties).
fn extract_node_id(
    results: &HashMap<String, zbus::zvariant::OwnedValue>,
) -> Result<u32, String> {
    use zbus::zvariant::Value;

    let streams_val = results.get("streams").ok_or("No 'streams' in response")?;

    // streams is a(ua{sv}) — array of (node_id, properties) tuples
    // OwnedValue derefs to Value
    let val: &Value = streams_val;
    match val {
        Value::Array(arr) => {
            let first = arr.first().ok_or("Empty streams array")?;
            match first {
                Value::Structure(s) => {
                    let fields = s.fields();
                    if let Some(Value::U32(node_id)) = fields.first() {
                        Ok(*node_id)
                    } else {
                        Err(format!("Unexpected stream structure: {:?}", fields))
                    }
                }
                _ => Err(format!("Unexpected stream element type: {:?}", first)),
            }
        }
        _ => Err(format!("streams is not an array: {:?}", val)),
    }
}

// ─── PipeWire frame capture ─────────────────────────────────────────────────

fn pipewire_capture_loop(
    fd: OwnedFd,
    node_id: u32,
    tx: SyncSender<RawFrame>,
    _config: CaptureConfig,
) -> Result<(), String> {
    use pipewire::main_loop::MainLoopBox;
    use pipewire::context::ContextBox;
    use pipewire::stream::StreamBox;

    pipewire::init();

    let mainloop =
        MainLoopBox::new(None).map_err(|e| format!("PW MainLoop: {:?}", e))?;
    let context = ContextBox::new(mainloop.loop_(), None)
        .map_err(|e| format!("PW Context: {:?}", e))?;
    let core = context
        .connect_fd(fd, None)
        .map_err(|e| format!("PW connect_fd: {:?}", e))?;

    let props = pipewire::properties::properties! {
        *pipewire::keys::MEDIA_TYPE => "Video",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
    };
    let stream = StreamBox::new(&core, "decibell-capture", props)
        .map_err(|e| format!("PW Stream: {:?}", e))?;

    // Quit flag — checked in process callback to stop mainloop when pipeline drops
    let quit_flag = std::rc::Rc::new(std::cell::Cell::new(false));

    struct CaptureData {
        tx: SyncSender<RawFrame>,
        start: Instant,
        quit: std::rc::Rc<std::cell::Cell<bool>>,
    }

    let user_data = CaptureData {
        tx,
        start: Instant::now(),
        quit: quit_flag.clone(),
    };

    let _listener = stream
        .add_local_listener_with_user_data(user_data)
        .param_changed(|_stream: &pipewire::stream::Stream, _data: &mut CaptureData, _id: u32, _param: Option<&pipewire::spa::pod::Pod>| {
            // Format negotiation callback — we extract dimensions from
            // the buffer chunk stride/size instead of parsing the SPA pod.
        })
        .process(|stream: &pipewire::stream::Stream, data: &mut CaptureData| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if let Some(d) = datas.first_mut() {
                    let stride = d.chunk().stride() as u32;
                    let size = d.chunk().size();

                    if stride >= 4 && size > 0 {
                        // Infer dimensions: BGRx/BGRA = 4 bytes per pixel
                        let width = stride / 4;
                        let height = size / stride;

                        if let Some(frame_bytes) = d.data() {
                            let usable = (size as usize).min(frame_bytes.len());
                            if usable >= (width * height * 4) as usize {
                                let frame = RawFrame {
                                    data: frame_bytes[..usable].to_vec(),
                                    width,
                                    height,
                                    timestamp_us: data.start.elapsed().as_micros() as u64,
                                };

                                match data.tx.try_send(frame) {
                                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                                        // Video pipeline stopped — signal quit
                                        data.quit.set(true);
                                    }
                                    _ => {} // OK or Full (drop frame if buffer full)
                                }
                            }
                        }
                    }
                }
            }
        })
        .register()
        .map_err(|e| format!("PW listener: {:?}", e))?;

    // Connect stream to the portal's PipeWire node
    stream
        .connect(
            pipewire::spa::utils::Direction::Input,
            Some(node_id),
            pipewire::stream::StreamFlags::AUTOCONNECT
                | pipewire::stream::StreamFlags::MAP_BUFFERS,
            &mut [],
        )
        .map_err(|e| format!("PW stream connect: {:?}", e))?;

    // Run mainloop — iterate manually so we can check quit_flag
    let loop_ref = mainloop.loop_();
    while !quit_flag.get() {
        loop_ref.iterate(std::time::Duration::from_millis(50));
    }

    log::info!("[capture] PipeWire capture stopped");
    Ok(())
}
