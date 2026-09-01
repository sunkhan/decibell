//! Share-audio application filter commands.
//!
//! `list_stream_audio_apps` enumerates the applications that currently
//! have an audio output (playing or paused) so the Go Live dialog and the
//! live popover can render checkboxes; `set_stream_audio_filter` stores
//! the streamer's mode + ticks on AppState and pushes them into the
//! running capture. The filter itself lives in
//! `media::stream_audio_filter`; the per-platform enumeration lives with
//! each capture backend. No wire traffic — the filter is local.

use crate::media::stream_audio_filter::StreamAudioFilter;
use crate::state;

#[napi(object)]
pub struct ListStreamAudioAppsArgs {
    /// Chromium desktopCapturer source id of the picked capture source.
    /// Windows resolves `window:HWND:0` to its owning process so that app
    /// can be flagged `owns_window_source`; ignored elsewhere.
    pub source_id: Option<String>,
}

#[napi(object)]
pub struct StreamAudioAppValue {
    /// Stable identity (lowercase program name) — what the filter stores.
    pub id: String,
    /// Display label.
    pub name: String,
    /// Live process ids behind this row (informational).
    pub pids: Vec<u32>,
    /// Currently rendering audio, as opposed to merely holding a session.
    pub active: bool,
    /// This app owns the window the streamer picked as the video source.
    pub owns_window_source: bool,
}

#[napi(object)]
pub struct StreamAudioAppList {
    /// False where per-app stream audio isn't available (macOS, renderer
    /// encode path) — the renderer hides the picker.
    pub supported: bool,
    pub apps: Vec<StreamAudioAppValue>,
}

#[napi]
pub async fn list_stream_audio_apps(args: ListStreamAudioAppsArgs) -> napi::Result<StreamAudioAppList> {
    #[cfg(target_os = "linux")]
    {
        // pw-dump spawns a child process; keep it off the tokio workers. The
        // portal gives no window identity, so `source_id` has nothing to map.
        let _ = args;
        let apps = tokio::task::spawn_blocking(crate::media::capture_audio_pipewire::list_apps)
            .await
            .map_err(|e| napi::Error::from_reason(format!("app enumeration failed: {}", e)))?
            .map_err(napi::Error::from_reason)?;
        return Ok(StreamAudioAppList {
            supported: true,
            apps: apps
                .into_iter()
                .map(|a| StreamAudioAppValue {
                    id: a.id,
                    name: a.name,
                    pids: a.pids,
                    active: a.active,
                    owns_window_source: false,
                })
                .collect(),
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows: WASAPI session enumeration lands with the process-loopback
        // mixer. macOS has no native stream audio — unsupported.
        let _ = args;
        Ok(StreamAudioAppList { supported: false, apps: Vec::new() })
    }
}

#[napi(object)]
pub struct SetStreamAudioFilterArgs {
    /// `selected` | `all_except` | `all`.
    pub mode: String,
    /// Ticked app identities (any spelling; normalised here).
    pub apps: Vec<String>,
}

/// Store the streamer's app filter and apply it to the running share-audio
/// capture, if any. Always succeeds: without a live engine the filter is
/// simply remembered for the next `start_screen_share`.
#[napi]
pub async fn set_stream_audio_filter(args: SetStreamAudioFilterArgs) -> napi::Result<()> {
    let filter = StreamAudioFilter::from_args(Some(&args.mode), Some(&args.apps));
    let state_arc = state::shared();
    let mut s = state_arc.lock().await;
    log::info!(
        "[stream-audio] filter → {} ({} app{})",
        filter.mode.as_str(),
        filter.apps.len(),
        if filter.apps.len() == 1 { "" } else { "s" }
    );
    if let Some(engine) = &s.audio_stream_engine {
        engine.set_filter(filter.clone());
    }
    s.stream_audio_filter = filter;
    Ok(())
}
