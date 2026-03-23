use super::capture::{CaptureSource, CaptureSourceType, RawFrame};

/// List available screens and windows.
/// On Linux with PipeWire, the actual source selection happens via the
/// XDG Desktop Portal picker dialog when capture starts. We return a
/// placeholder entry that triggers the portal picker.
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    let sources = vec![CaptureSource {
        id: "portal".to_string(),
        name: "Screen (Portal Picker)".to_string(),
        source_type: CaptureSourceType::Screen,
        width: 0,  // determined after portal selection
        height: 0,
    }];

    Ok(sources)
}

/// Start capturing from a PipeWire source.
/// Returns a channel that receives RawFrames.
/// Uses std::sync::mpsc (not tokio) because the video pipeline runs on a
/// dedicated OS thread, not in the tokio runtime.
///
/// The source selection is done via the XDG Desktop Portal D-Bus API,
/// which returns a PipeWire node ID. The pipewire crate then connects
/// to that node for frame capture.
pub async fn start_capture(
    _source_id: &str,
    config: &super::capture::CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(4);

    // PipeWire frame capture runs on a dedicated thread
    let _config = config.clone();
    std::thread::Builder::new()
        .name("decibell-capture".to_string())
        .spawn(move || {
            // TODO: Implementation steps:
            // 1. Use D-Bus (zbus) to call org.freedesktop.portal.ScreenCast
            //    - CreateSession, SelectSources (with portal picker), Start
            //    - Extract PipeWire node ID from the response
            // 2. Initialize PipeWire main loop
            // 3. Connect to the PipeWire node as a video consumer
            // 4. Extract BGRA frames and push RawFrame structs through tx
            let _ = (tx, _config);
        })
        .map_err(|e| format!("Failed to spawn capture thread: {}", e))?;

    Ok(rx)
}
