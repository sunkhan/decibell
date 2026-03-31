use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use super::capture::{CaptureConfig, CaptureOutput, CaptureSource, CaptureSourceType, RawFrame};

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
        thumbnail: None,
    }])
}

/// Start capturing via XDG Desktop Portal + PipeWire.
/// This triggers the portal picker dialog (blocking until user selects),
/// then starts a PipeWire capture thread that feeds frames through the channel.
pub async fn start_capture(
    _source_id: &str,
    config: &CaptureConfig,
) -> Result<CaptureOutput, String> {
    let config = config.clone();

    // Run portal dialog + PipeWire setup on a blocking thread
    // (portal dialog blocks until user picks a source)
    let result = tokio::task::spawn_blocking(move || -> Result<_, String> {
        // Step 1: Portal D-Bus interaction (shows picker dialog)
        let (pw_fd, node_id, dbus_conn) = portal_screencast_session()?;

        // Step 2: Start PipeWire capture on a dedicated thread
        // The D-Bus connection is moved into the thread to keep the portal
        // session alive for the duration of the capture.
        let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(4);

        // When target is 0x0 ("source" quality), the actual dimensions aren't
        // known until PipeWire negotiates. Use a oneshot to communicate them back.
        let (dim_tx, dim_rx) = std::sync::mpsc::sync_channel::<(u32, u32)>(1);

        let needs_resolution = config.target_width == 0 || config.target_height == 0;
        let known_dims = (config.target_width, config.target_height);

        std::thread::Builder::new()
            .name("decibell-capture".to_string())
            .spawn(move || {
                if let Err(e) = pipewire_capture_loop(pw_fd, node_id, tx, config, dbus_conn, dim_tx) {
                    eprintln!("[capture] Capture loop error: {}", e);
                }
            })
            .map_err(|e| format!("Spawn capture thread: {}", e))?;

        // Wait for resolved dimensions from PipeWire format negotiation
        let (width, height) = if needs_resolution {
            dim_rx.recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| "Timeout waiting for PipeWire format negotiation".to_string())?
        } else {
            known_dims
        };

        Ok(CaptureOutput { receiver: rx, width, height })
    })
    .await
    .map_err(|e| format!("Join error: {}", e))??;

    Ok(result)
}

// ─── XDG Desktop Portal D-Bus interaction ───────────────────────────────────

/// Unwrap a zbus OwnedValue to an OwnedObjectPath.
/// Handles Value::ObjectPath, Value::Str, and nested Value::Value wrapping.
fn value_to_object_path(val: &zbus::zvariant::OwnedValue) -> Result<zbus::zvariant::OwnedObjectPath, String> {
    use zbus::zvariant::{OwnedObjectPath, Value};
    fn extract(v: &Value<'_>) -> Result<OwnedObjectPath, String> {
        match v {
            Value::ObjectPath(p) => Ok(OwnedObjectPath::from(p.clone())),
            Value::Str(s) => s.as_str().try_into()
                .map_err(|e: zbus::zvariant::Error| format!("{}", e)),
            Value::Value(inner) => extract(inner),
            other => Err(format!("unexpected type: {:?}", other)),
        }
    }
    extract(<&Value>::from(val))
}

/// Run the full portal screencast session (blocking).
/// Returns (PipeWire fd, node_id, D-Bus connection) on success.
/// IMPORTANT: The D-Bus connection must be kept alive for the duration of
/// the capture — the portal cleans up the screencast session when the
/// requesting client disconnects from D-Bus.
fn portal_screencast_session() -> Result<(OwnedFd, u32, zbus::blocking::Connection), String> {
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
    let session: OwnedObjectPath = value_to_object_path(session_val)
        .map_err(|e| format!("Parse session_handle: {}", e))?;

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

    Ok((fd.into(), node_id, conn))
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

    // Unwrap nested Value::Value wrappers
    fn unwrap_value<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
        match v {
            Value::Value(inner) => unwrap_value(inner),
            other => other,
        }
    }

    // streams is a(ua{sv}) — array of (node_id, properties) tuples
    let val = unwrap_value(<&Value>::from(streams_val));
    match val {
        Value::Array(arr) => {
            let first = arr.first().ok_or("Empty streams array")?;
            let first = unwrap_value(first);
            match first {
                Value::Structure(s) => {
                    let fields = s.fields();
                    let first_field = fields.first().ok_or("Empty struct")?;
                    let first_field = unwrap_value(first_field);
                    if let Value::U32(node_id) = first_field {
                        Ok(*node_id)
                    } else {
                        Err(format!("Expected u32 node_id, got: {:?}", first_field))
                    }
                }
                _ => Err(format!("Unexpected stream element type: {:?}", first)),
            }
        }
        _ => Err(format!("streams is not an array: {:?}", val)),
    }
}

// ─── Direct PipeWire capture ─────────────────────────────────────────────────
//
// Uses pipewire-rs to connect directly to the PipeWire node, negotiating
// SHM (shared memory) buffers to avoid NVIDIA DMA-BUF block-linear tiling
// issues that cause horizontal shifts with GStreamer's videoconvert.

use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;

struct CaptureData {
    format: spa::param::video::VideoInfoRaw,
    tx: SyncSender<RawFrame>,
    target_width: u32,
    target_height: u32,
    frame_count: u64,
    start: Instant,
    quit_mainloop: pw::main_loop::MainLoopWeak,
    /// Oneshot sender for resolved dimensions (used when target is 0x0 "source" quality).
    dim_tx: Option<std::sync::mpsc::SyncSender<(u32, u32)>>,
}

fn pipewire_capture_loop(
    fd: OwnedFd,
    node_id: u32,
    tx: SyncSender<RawFrame>,
    config: CaptureConfig,
    _dbus_conn: zbus::blocking::Connection, // kept alive to preserve portal session
    dim_tx: std::sync::mpsc::SyncSender<(u32, u32)>,
) -> Result<(), String> {
    // 0 means "source resolution" — will be resolved in param_changed once
    // PipeWire negotiates the actual format.
    let target_width = config.target_width;
    let target_height = config.target_height;

    eprintln!(
        "[capture] Starting PipeWire capture (node={}, target={}x{}@{}fps)",
        node_id, target_width, target_height, config.target_fps
    );

    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| format!("PW MainLoop: {:?}", e))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| format!("PW Context: {:?}", e))?;
    let core = context.connect_fd_rc(fd, None)
        .map_err(|e| format!("PW connect_fd: {:?}", e))?;

    let data = CaptureData {
        format: Default::default(),
        tx,
        target_width,
        target_height,
        frame_count: 0,
        start: Instant::now(),
        quit_mainloop: mainloop.downgrade(),
        dim_tx: Some(dim_tx),
    };

    let stream = pw::stream::StreamRc::new(
        core,
        "decibell-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|e| format!("PW Stream: {:?}", e))?;

    let mainloop_weak = mainloop.downgrade();

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_stream, _data, old, new| {
            eprintln!("[capture] PipeWire stream: {:?} -> {:?}", old, new);
            if let pw::stream::StreamState::Error(msg) = &new {
                eprintln!("[capture] Stream error: {}", msg);
                if let Some(ml) = mainloop_weak.upgrade() {
                    ml.quit();
                }
            }
        })
        .param_changed(|_stream, data, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }

            let (media_type, media_subtype) =
                match spa::param::format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(_) => return,
                };

            if media_type != spa::param::format::MediaType::Video
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }

            data.format
                .parse(param)
                .expect("Failed to parse VideoInfoRaw");

            let w = data.format.size().width;
            let h = data.format.size().height;
            let fmt = data.format.format();

            // Resolve "source" resolution (0x0) to the actual capture dimensions
            if data.target_width == 0 {
                data.target_width = w;
            }
            if data.target_height == 0 {
                data.target_height = h;
            }

            // Notify start_capture() of the resolved dimensions (for encoder init)
            if let Some(dim_tx) = data.dim_tx.take() {
                let _ = dim_tx.send((data.target_width, data.target_height));
            }

            eprintln!(
                "[capture] Negotiated format: {:?} {}x{} -> {}x{} @ {}/{}",
                fmt, w, h,
                data.target_width, data.target_height,
                data.format.framerate().num,
                data.format.framerate().denom,
            );

            // Don't set buffer params — let PipeWire negotiate automatically.
            // We handle stride correctly in the process callback using chunk metadata.
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }

            let d = &mut datas[0];

            // Extract chunk metadata before taking mutable borrow for data
            let chunk_size = d.chunk().size() as usize;
            let stride = d.chunk().stride() as usize;
            let chunk_offset = d.chunk().offset() as usize;
            let buf_type = d.type_();

            if chunk_size == 0 || stride == 0 {
                return;
            }

            let Some(raw_data) = d.data() else { return };
            let raw_data = &raw_data[chunk_offset..][..chunk_size];

            let src_w = data.format.size().width as usize;
            let src_h = data.format.size().height as usize;
            let fmt = data.format.format();

            data.frame_count += 1;
            if data.frame_count <= 3 || data.frame_count % 120 == 0 {
                eprintln!(
                    "[capture] Frame {} ({:?} {}x{}, stride={}, chunk={} bytes, buf_type={:?}, {:.1}s)",
                    data.frame_count, fmt, src_w, src_h, stride, chunk_size,
                    buf_type, data.start.elapsed().as_secs_f64()
                );
            }

            // Send raw pixel data — color conversion happens on the encoder thread
            // to keep the PipeWire callback fast and avoid blocking the compositor.
            let is_bgra = fmt == spa::param::video::VideoFormat::BGRA
                || fmt == spa::param::video::VideoFormat::BGRx;
            let is_rgba = fmt == spa::param::video::VideoFormat::RGBA
                || fmt == spa::param::video::VideoFormat::RGBx;

            if !is_bgra && !is_rgba {
                if data.frame_count <= 3 {
                    eprintln!("[capture] Unsupported format {:?}, skipping", fmt);
                }
                return;
            }

            let pixel_format = if is_bgra {
                super::capture::PixelFormat::BGRA
            } else {
                super::capture::PixelFormat::RGBA
            };

            let frame = RawFrame {
                data: raw_data.to_vec(),
                width: src_w as u32,
                height: src_h as u32,
                stride,
                pixel_format,
                timestamp_us: data.start.elapsed().as_micros() as u64,
            };

            match data.tx.try_send(frame) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    eprintln!("[capture] Frame channel closed, stopping");
                    if let Some(ml) = data.quit_mainloop.upgrade() {
                        ml.quit();
                    }
                }
            }
        })
        .register()
        .map_err(|e| format!("PW listener: {:?}", e))?;

    // Negotiate BGRA format (preferred for screen capture)
    let format_obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::RGBx
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: target_width,
                height: target_height,
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1,
            },
            spa::utils::Rectangle {
                width: 7680,
                height: 4320,
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: config.target_fps, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 1000, denom: 1 }
        ),
    );

    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(format_obj),
    )
    .unwrap()
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )
    .map_err(|e| format!("PW stream connect: {:?}", e))?;

    eprintln!("[capture] PipeWire stream connected, running main loop");
    mainloop.run();
    eprintln!("[capture] PipeWire main loop exited");

    Ok(())
}

