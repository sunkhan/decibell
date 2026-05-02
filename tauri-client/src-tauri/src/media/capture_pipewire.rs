use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use std::os::fd::FromRawFd;
use super::capture::{CaptureConfig, CaptureOutput, CaptureSource, CaptureSourceType, DmaBufFrame, RawFrame};

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
        // Queue depth 2: one frame in flight in the encoder, one ready to
        // hand off, drop newer SHM frames if the encoder lags. At 1080p
        // BGRA that's ~16MB cap; at 1440p ~22MB. The send is try_send so
        // the capture thread never blocks on a slow consumer.
        let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(2);
        // gpu_tx is preserved for symmetry with the existing pipeline
        // wiring but stays empty on Linux now that we don't advertise
        // DMA-BUF — depth=1 is plenty for an unused channel.
        let (gpu_tx, gpu_rx) = std::sync::mpsc::sync_channel::<DmaBufFrame>(1);

        // When target is 0x0 ("source" quality), the actual dimensions aren't
        // known until PipeWire negotiates. Use a oneshot to communicate them back.
        let (dim_tx, dim_rx) = std::sync::mpsc::sync_channel::<(u32, u32)>(1);

        let needs_resolution = config.target_width == 0 || config.target_height == 0;
        let known_dims = (config.target_width, config.target_height);

        std::thread::Builder::new()
            .name("decibell-capture".to_string())
            .spawn(move || {
                if let Err(e) = pipewire_capture_loop(pw_fd, node_id, tx, gpu_tx, config, dbus_conn, dim_tx) {
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

        Ok(CaptureOutput { receiver: rx, width, height, gpu_receiver: Some(gpu_rx) })
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
                ("types", Value::from(3u32)),       // MONITOR | WINDOW
                ("multiple", Value::from(false)),
                // cursor_mode is a bitmask of *supported* cursor modes. 1 = Hidden.
                // Setting it explicitly avoids producers that treat the absence of
                // the option as "client doesn't support this negotiation at all".
                ("cursor_mode", Value::from(1u32)),
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
    gpu_tx: SyncSender<DmaBufFrame>,
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    frame_count: u64,
    last_capture_us: u64,
    start: Instant,
    quit_mainloop: pw::main_loop::MainLoopWeak,
    /// Oneshot sender for resolved dimensions (used when target is 0x0 "source" quality).
    dim_tx: Option<std::sync::mpsc::SyncSender<(u32, u32)>>,
    diag_process_calls: u64,
    diag_no_buffer: u64,
    diag_rate_limited: u64,
    diag_empty_datas: u64,
    diag_zero_chunk: u64,
}

/// Build an EnumFormat param describing what we accept from the producer.
///
/// When `with_modifier` is true, the param includes a `VideoModifier`
/// property with flags `MANDATORY | DONT_FIXATE`, listing `DRM_FORMAT_MOD_INVALID`
/// (the "any modifier" wildcard) and `DRM_FORMAT_MOD_LINEAR`. This is the
/// handshake GNOME/mutter's xdg-desktop-portal requires to hand us DMA-BUF
/// buffers. Without it, mutter has no compatible offer and the stream
/// errors with "no more input formats".
///
/// When `with_modifier` is false, VideoModifier is omitted, which signals
/// plain SHM to the producer — the historical path that KDE's portal serves.
fn build_format_pod(
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    with_modifier: bool,
) -> Vec<u8> {
    use pw::spa::pod::{ChoiceValue, Property, PropertyFlags, Value};
    use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags};

    let size_pref = spa::utils::Rectangle {
        // "Source" quality passes 0x0. We used to set the preferred size
        // to the 8K range max (7680x4320), reasoning that PipeWire would
        // walk down to the actual monitor size — but mutter on NVIDIA
        // Wayland rejects format negotiation outright when the preferred
        // is wildly larger than what it can satisfy. 1920x1080 is a much
        // more common landing zone; the size range stays 1x1..7680x4320
        // so larger monitors can still be picked, just without that being
        // the preference.
        width: if target_width > 0 { target_width } else { 1920 },
        height: if target_height > 0 { target_height } else { 1080 },
    };

    let mut obj = spa::pod::object!(
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
            size_pref,
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 7680, height: 4320 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: target_fps, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 1000, denom: 1 }
        ),
    );

    if with_modifier {
        // Default LINEAR (mmap-readable, what we want), MOD_INVALID as
        // alternative so mutter on NVIDIA Wayland — which can't allocate
        // LINEAR but can allocate "whatever native tiled layout" via
        // MOD_INVALID — has something it can satisfy and won't reject the
        // whole format with "no more input formats".
        //
        // process() checks the producer-fixated modifier on each frame:
        // LINEAR goes through the mmap path normally, MOD_INVALID gets
        // dropped with a clear "incompatible setup" log rather than
        // encoded as garbage. Better to surface a real error than to
        // silently stream a black feed.
        obj.properties.push(Property {
            key: spa::param::format::FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY,
            value: Value::Choice(ChoiceValue::Long(Choice::<i64>(
                ChoiceFlags::empty(),
                ChoiceEnum::<i64>::Enum {
                    default: 0_i64, // DRM_FORMAT_MOD_LINEAR — preferred
                    alternatives: vec![
                        0_i64,
                        super::gpu_interop::DRM_FORMAT_MOD_INVALID as i64,
                    ],
                },
            ))),
        });
    }

    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner()
}

/// Build a `ParamMeta` pod requesting `SPA_META_Header` metadata on every
/// buffer. Some producers (mutter on certain configs, and Niri's screencast
/// implementation) treat this as the signal that the consumer is ready to
/// receive data — without it, buffers are allocated into the pool but never
/// actually enqueued, and `process()` never fires.
fn build_param_meta_header() -> Vec<u8> {
    use pw::spa::pod::{Object, Property, PropertyFlags, Value};

    const SPA_META_HEADER_SIZE: i32 = 32;

    let obj = Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamMeta.as_raw(),
        id: pw::spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            Property {
                key: pw::spa::sys::SPA_PARAM_META_type,
                flags: PropertyFlags::empty(),
                value: Value::Id(pw::spa::utils::Id(pw::spa::sys::SPA_META_Header)),
            },
            Property {
                key: pw::spa::sys::SPA_PARAM_META_size,
                flags: PropertyFlags::empty(),
                value: Value::Int(SPA_META_HEADER_SIZE),
            },
        ],
    };

    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner()
}

/// Parse the producer's Format pod and extract the `VideoModifier` Choice
/// alternatives. When the producer sends `FIXATION_REQUIRED`, this is the
/// actual set of modifiers it's offering — we must pick one of these to
/// respond with. `VideoInfoRaw::parse` only gives the "default" (first) entry.
fn extract_modifier_alternatives(param: &Pod) -> Option<Vec<u64>> {
    use pw::spa::pod::{
        deserialize::PodDeserializer, ChoiceValue, Value,
    };
    use pw::spa::utils::ChoiceEnum;

    let bytes = param.as_bytes();
    let (_, value) = PodDeserializer::deserialize_from::<Value>(bytes).ok()?;

    let obj = match value {
        Value::Object(o) => o,
        _ => return None,
    };

    let key_modifier = spa::param::format::FormatProperties::VideoModifier.as_raw();
    for prop in &obj.properties {
        if prop.key != key_modifier {
            continue;
        }
        match &prop.value {
            Value::Long(v) => return Some(vec![*v as u64]),
            Value::Choice(ChoiceValue::Long(choice)) => {
                return match &choice.1 {
                    ChoiceEnum::None(v) => Some(vec![*v as u64]),
                    ChoiceEnum::Enum { alternatives, .. } => {
                        Some(alternatives.iter().map(|&x| x as u64).collect())
                    }
                    _ => None,
                };
            }
            _ => return None,
        }
    }
    None
}

/// Pick a modifier to fixate on. Preference order:
///   1. `DRM_FORMAT_MOD_INVALID` (wildcard — producer uses its native layout).
///   2. Any non-LINEAR modifier (native GPU layout is what the compositor
///      can actually export; LINEAR often fails allocation on NVIDIA).
///   3. LINEAR as a last resort.
///   4. The producer-default if the list was empty (VideoInfoRaw fallback).
fn pick_modifier(alternatives: &[u64], default: u64) -> u64 {
    const MOD_INVALID: u64 = 0x00ffffff_ffffffff;
    const MOD_LINEAR: u64 = 0;

    if alternatives.is_empty() {
        return default;
    }
    if alternatives.iter().any(|&m| m == MOD_INVALID) {
        return MOD_INVALID;
    }
    if let Some(&m) = alternatives.iter().find(|&&m| m != MOD_LINEAR) {
        return m;
    }
    alternatives[0]
}

/// Build a concrete `Format` pod (not `EnumFormat`) with every property
/// fixated to a single value. This is the response mutter / xdg-desktop-portal-
/// gnome expects after we offered a DONT_FIXATE modifier choice: the producer
/// replied with a narrowed modifier list, we pick one, and acknowledge with
/// this concrete Format + ParamBuffers so it can finally allocate buffers.
fn build_fixated_format_pod(
    width: u32,
    height: u32,
    framerate: spa::utils::Fraction,
    fmt: spa::param::video::VideoFormat,
    modifier: u64,
) -> Vec<u8> {
    use pw::spa::pod::{Object, Property, PropertyFlags, Value};

    // Ordering matters — VideoModifier is required to come right after
    // VideoFormat per the spa format-object convention.
    let obj = Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::Format.as_raw(),
        properties: vec![
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
                Id,
                fmt
            ),
            // VideoModifier as a plain Long (no Choice), flagged MANDATORY —
            // the "fixated" signal producers wait for to proceed to allocation.
            Property {
                key: spa::param::format::FormatProperties::VideoModifier.as_raw(),
                flags: PropertyFlags::MANDATORY,
                value: Value::Long(modifier as i64),
            },
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoSize,
                Rectangle,
                spa::utils::Rectangle { width, height }
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoFramerate,
                Fraction,
                framerate
            ),
        ],
    };

    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner()
}

/// Build a `ParamBuffers` pod that matches the negotiated format. This is the
/// response mutter / xdg-desktop-portal-gnome waits for after we receive the
/// Format — without it the stream stays Paused forever and `process()` never
/// fires. The `dataType` mask must match whatever the producer fixated:
/// advertising DMA-BUF when it picked SHM (or vice-versa) fails allocation.
fn build_param_buffers(_width: u32, _height: u32, uses_dmabuf: bool) -> Vec<u8> {
    use pw::spa::pod::{ChoiceValue, Object, Property, PropertyFlags, Value};
    use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags};

    // SPA_DATA_* are 0-based indices; `(1 << n)` forms the dataType bitmask.
    let data_type_mask: i32 = if uses_dmabuf {
        1 << pw::spa::sys::SPA_DATA_DmaBuf
    } else {
        (1 << pw::spa::sys::SPA_DATA_MemPtr) | (1 << pw::spa::sys::SPA_DATA_MemFd)
    };

    // Intentionally omit BUFFERS_stride and BUFFERS_size. Tiled DMA-BUF formats
    // often have strides/sizes that don't match width*4 (alignment padding,
    // compressed layouts, etc.), and specifying tight minimums silently blocks
    // the producer from enqueuing buffers it's already allocated. Leaving
    // them unspecified tells the producer "any stride/size is fine".
    let obj = Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: pw::spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            Property {
                key: pw::spa::sys::SPA_PARAM_BUFFERS_buffers,
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Int(Choice::<i32>(
                    ChoiceFlags::empty(),
                    ChoiceEnum::<i32>::Range { default: 8, min: 2, max: 32 },
                ))),
            },
            Property {
                key: pw::spa::sys::SPA_PARAM_BUFFERS_blocks,
                flags: PropertyFlags::empty(),
                value: Value::Int(1),
            },
            Property {
                key: pw::spa::sys::SPA_PARAM_BUFFERS_dataType,
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Int(Choice::<i32>(
                    ChoiceFlags::empty(),
                    ChoiceEnum::<i32>::Flags {
                        default: data_type_mask,
                        flags: vec![data_type_mask],
                    },
                ))),
            },
        ],
    };

    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner()
}

fn pipewire_capture_loop(
    fd: OwnedFd,
    node_id: u32,
    tx: SyncSender<RawFrame>,
    gpu_tx: SyncSender<DmaBufFrame>,
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
        gpu_tx,
        target_width,
        target_height,
        target_fps: config.target_fps,
        frame_count: 0,
        last_capture_us: 0,
        start: Instant::now(),
        quit_mainloop: mainloop.downgrade(),
        dim_tx: Some(dim_tx),
        diag_process_calls: 0,
        diag_no_buffer: 0,
        diag_rate_limited: 0,
        diag_empty_datas: 0,
        diag_zero_chunk: 0,
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
        .add_buffer(|_stream, _data, _buf| {
            eprintln!("[capture] add_buffer (producer allocated a buffer)");
        })
        .remove_buffer(|_stream, _data, _buf| {
            eprintln!("[capture] remove_buffer");
        })
        .param_changed(|stream, data, id, param| {
            let Some(param) = param else {
                eprintln!("[capture] param_changed(id={}, param=None)", id);
                return;
            };
            eprintln!(
                "[capture] param_changed(id={} = {:?})",
                id,
                spa::param::ParamType::from_raw(id)
            );
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
            let framerate = data.format.framerate();
            let modifier = data.format.modifier();

            // Video flags tell us what the producer fixated to.
            //   bit 2 = SPA_VIDEO_FLAG_MODIFIER             → DMA-BUF with modifier
            //   bit 3 = SPA_VIDEO_FLAG_MODIFIER_FIXATION_REQUIRED → producer wants us to pick one
            const SPA_VIDEO_FLAG_MODIFIER: u32 = 1 << 2;
            const SPA_VIDEO_FLAG_MODIFIER_FIXATION_REQUIRED: u32 = 1 << 3;
            let vflags = data.format.flags().bits();
            let uses_dmabuf = (vflags & SPA_VIDEO_FLAG_MODIFIER) != 0;
            let needs_fixation = (vflags & SPA_VIDEO_FLAG_MODIFIER_FIXATION_REQUIRED) != 0;

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
                "[capture] Negotiated format: {:?} {}x{} -> {}x{} @ {}/{} (modifier=0x{:x}, dmabuf={}, fixate={})",
                fmt, w, h,
                data.target_width, data.target_height,
                framerate.num, framerate.denom,
                modifier, uses_dmabuf, needs_fixation,
            );

            // Build the response params. The producer is waiting on us:
            //   - If fixation is required, we must re-send a concrete `Format`
            //     with a single chosen modifier so the producer can allocate.
            //   - We also always push `ParamBuffers` so the producer knows our
            //     acceptable dataType (must match SHM vs DMA-BUF).
            let mut bytes_store: Vec<Vec<u8>> = Vec::new();
            if needs_fixation {
                // VideoInfoRaw::parse() only hands us the "default" modifier
                // from the producer's narrowed Choice list. That's usually
                // LINEAR (0), which mutter claims to support but often can't
                // actually export on NVIDIA / tiled-native GPUs — allocation
                // then fails. Parse the raw pod to see the full list and
                // prefer a non-LINEAR modifier (the GPU's native layout).
                let alternatives = extract_modifier_alternatives(param).unwrap_or_default();
                let chosen_modifier = pick_modifier(&alternatives, modifier);
                eprintln!(
                    "[capture] Fixating modifier: producer offered {:?} (default 0x{:x}) -> sending 0x{:x}",
                    alternatives
                        .iter()
                        .map(|m| format!("0x{:x}", m))
                        .collect::<Vec<_>>(),
                    modifier,
                    chosen_modifier,
                );
                bytes_store.push(build_fixated_format_pod(
                    w, h, framerate, fmt, chosen_modifier,
                ));
            }
            bytes_store.push(build_param_buffers(w, h, uses_dmabuf));
            // Opt in to SPA_META_Header — some producers (mutter/Niri variants)
            // require the consumer to request per-frame metadata before they
            // actually start enqueuing buffers. Without this, the pool fills
            // up but frames never fire process().
            bytes_store.push(build_param_meta_header());

            let pods: Vec<&Pod> = bytes_store
                .iter()
                .filter_map(|b| Pod::from_bytes(b))
                .collect();
            let mut params: Vec<&Pod> = pods;
            if let Err(e) = stream.update_params(&mut params) {
                eprintln!("[capture] update_params failed: {:?}", e);
            }
        })
        .process(|stream, data| {
            data.diag_process_calls = data.diag_process_calls.saturating_add(1);
            let log_diag = data.frame_count == 0 && data.diag_process_calls <= 5;

            let Some(mut buffer) = stream.dequeue_buffer() else {
                data.diag_no_buffer = data.diag_no_buffer.saturating_add(1);
                if log_diag {
                    eprintln!("[capture] process(): dequeue_buffer returned None");
                }
                return;
            };

            // Rate-limit: skip frames arriving faster than target FPS
            let now_us = data.start.elapsed().as_micros() as u64;
            let frame_interval_us = 1_000_000 / data.target_fps.max(1) as u64;
            if now_us.saturating_sub(data.last_capture_us) < frame_interval_us {
                data.diag_rate_limited = data.diag_rate_limited.saturating_add(1);
                drop(buffer);
                return;
            }

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                data.diag_empty_datas = data.diag_empty_datas.saturating_add(1);
                if log_diag {
                    eprintln!("[capture] process(): buffer.datas is empty");
                }
                return;
            }

            let d = &mut datas[0];
            let chunk_size = d.chunk().size() as usize;
            let stride = d.chunk().stride() as usize;
            let buf_type = d.type_();

            use pw::spa::buffer::DataType;
            if log_diag {
                eprintln!(
                    "[capture] process() diag: chunk_size={}, stride={}, buf_type={:?}, has_data={}",
                    chunk_size, stride, buf_type, d.data().is_some()
                );
            }

            // Stride is always required. chunk_size is only meaningful for
            // SHM buffers — DMA-BUF producers (mutter, Niri) commonly report
            // chunk.size=0 because the buffer size is implied by stride *
            // height. Requiring chunk_size > 0 here previously discarded every
            // DMA-BUF frame and froze the stream.
            if stride == 0 {
                data.diag_zero_chunk = data.diag_zero_chunk.saturating_add(1);
                if data.diag_zero_chunk == 1 || data.diag_zero_chunk % 600 == 0 {
                    eprintln!(
                        "[capture] process(): zero stride — count={}",
                        data.diag_zero_chunk
                    );
                }
                return;
            }
            if buf_type != DataType::DmaBuf && chunk_size == 0 {
                data.diag_zero_chunk = data.diag_zero_chunk.saturating_add(1);
                if data.diag_zero_chunk == 1 || data.diag_zero_chunk % 600 == 0 {
                    eprintln!(
                        "[capture] process(): SHM buffer with zero chunk_size — count={}",
                        data.diag_zero_chunk
                    );
                }
                return;
            }

            let src_w = data.format.size().width;
            let src_h = data.format.size().height;
            let fmt = data.format.format();

            data.frame_count += 1;
            if data.frame_count <= 3 || data.frame_count % 120 == 0 {
                eprintln!(
                    "[capture] Frame {} ({:?} {}x{}, stride={}, chunk={} bytes, buf_type={:?}, {:.1}s)",
                    data.frame_count, fmt, src_w, src_h, stride, chunk_size,
                    buf_type, data.start.elapsed().as_secs_f64()
                );
            }

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

            // Reject tiled DMA-BUFs (anything that isn't LINEAR). We only
            // advertise LINEAR + MOD_INVALID and read everything via the
            // mmap shim — tiled bytes through mmap are garbage. If the
            // producer fixated MOD_INVALID, the modifier flag is set but
            // the actual modifier is whatever the producer chose internally
            // (usually a vendor-specific tile layout). Surface a clear
            // error rather than silently encoding a black stream.
            if buf_type == DataType::DmaBuf {
                const SPA_VIDEO_FLAG_MODIFIER: u32 = 1 << 2;
                let has_modifier = (data.format.flags().bits() & SPA_VIDEO_FLAG_MODIFIER) != 0;
                let modifier = if has_modifier { data.format.modifier() } else { 0 };
                if modifier != 0 {
                    if data.frame_count == 1 {
                        eprintln!(
                            "[capture] FATAL: producer chose tiled DMA-BUF (modifier=0x{:x}). \
                             Cannot read tiled buffers via mmap. This is the NVIDIA + Wayland + \
                             gnome-portal limitation — mutter on NVIDIA can't allocate LINEAR \
                             DMA-BUF for screencopy. Workarounds: (a) run an X11 session, \
                             (b) switch to a different compositor (Niri+wlr-portal works \
                             differently — try `xdg-desktop-portal-wlr`).",
                            modifier
                        );
                    }
                    return;
                }
            }

            // Unified read path for SHM and LINEAR DMA-BUF.
            //
            // PipeWire's MAP_BUFFERS stream flag mmap's incoming DMA-BUFs for
            // us, so `d.data()` returns valid CPU-readable bytes regardless
            // of the underlying buffer type — provided the modifier is one we
            // can actually read (LINEAR). We only ever advertise SHM and
            // LINEAR DMA-BUF, so anything that lands here is safe to mmap.
            //
            // chunk.size is the authoritative payload size for SHM. For
            // DMA-BUF, producers commonly leave it at 0 since the size is
            // implied by stride * height — fall back to that.
            let chunk_offset = d.chunk().offset() as usize;
            let Some(raw_data) = d.data() else { return };
            let bytes_per_frame = if chunk_size > 0 {
                chunk_size
            } else {
                stride * (src_h as usize)
            };
            if raw_data.len() < chunk_offset + bytes_per_frame {
                if data.frame_count <= 3 {
                    eprintln!(
                        "[capture] mmap'd buffer too small: have={}, need={} (offset={}, bytes={})",
                        raw_data.len(), chunk_offset + bytes_per_frame, chunk_offset, bytes_per_frame
                    );
                }
                return;
            }
            let raw_data = &raw_data[chunk_offset..][..bytes_per_frame];

            let pixel_format = if is_bgra {
                super::capture::PixelFormat::BGRA
            } else {
                super::capture::PixelFormat::RGBA
            };

            let frame = RawFrame {
                data: raw_data.to_vec(),
                width: src_w,
                height: src_h,
                stride,
                pixel_format,
                timestamp_us: now_us,
            };

            match data.tx.try_send(frame) {
                Ok(()) => { data.last_capture_us = now_us; }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    eprintln!("[capture] Frame channel closed, stopping");
                    if let Some(ml) = data.quit_mainloop.upgrade() { ml.quit(); }
                }
            }
        })
        .register()
        .map_err(|e| format!("PW listener: {:?}", e))?;

    // SHM preferred; LINEAR-DMA-BUF as fallback for compositors that can't
    // produce SHM at all.
    //
    // The "real" zero-copy DMA-BUF path (DMA-BUF fd → EGLImage → CUDA array
    // → NVENC) was supposed to be the fast lane on NVIDIA. It's unsalvageable
    // on Linux today: xdg-desktop-portal-gnome hands NVIDIA mesa-allocated
    // tiled DMA-BUFs that import as zero pages (every frame black), and
    // xdg-desktop-portal-wlr on Niri negotiates LINEAR cleanly but its
    // screencopy backend never allocates buffers for our PipeWire client.
    //
    // The compromise: take whatever the compositor will give us, but only in
    // a CPU-readable layout. SHM goes straight to a Vec. LINEAR DMA-BUF is
    // mmap'd by PipeWire (MAP_BUFFERS flag) and we read the same way. We
    // never advertise tiled modifiers, so we can't get a buffer that needs
    // EGL to read. NVENC consumes BGRA and converts to NV12 internally on
    // the GPU, so per-frame CPU cost is one memcpy out of the PipeWire
    // buffer — a few ms at 1080p.
    let shm_bytes = build_format_pod(target_width, target_height, config.target_fps, false);
    let dmabuf_bytes = build_format_pod(target_width, target_height, config.target_fps, true);
    let mut params = [
        Pod::from_bytes(&shm_bytes).unwrap(),
        Pod::from_bytes(&dmabuf_bytes).unwrap(),
    ];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )
    .map_err(|e| format!("PW stream connect: {:?}", e))?;

    // Some producers keep the stream in Paused until the consumer explicitly
    // flips it active, even without PW_STREAM_FLAG_INACTIVE set. This is a
    // harmless no-op when the stream is already active.
    if let Err(e) = stream.set_active(true) {
        eprintln!("[capture] set_active(true) failed: {:?}", e);
    }

    eprintln!("[capture] PipeWire stream connected, running main loop");
    mainloop.run();
    eprintln!("[capture] PipeWire main loop exited");

    Ok(())
}

