//! The panel's wording (`features/transfers/format.ts`, `DownloadRow.tsx`, `UploadRow.tsx`,
//! `TransfersButton.tsx`): status lines, the button's label and its progress ring. Pure functions,
//! unit-tested against the renderer's strings. "N min ago" and counts are the crate's regional
//! formatting (`crate::fmt`); sizes, speeds and ETAs stay as the renderer writes them.

use decibell_model::live::transfers::{
    DownloadState, DownloadView, UploadEntry, UploadState, format_eta, format_progress, format_size, format_speed,
};

/// `#channel`, else the community's name.
fn where_of(channel_name: &str, server_name: &str) -> String {
    if channel_name.is_empty() { server_name.to_string() } else { format!("#{channel_name}") }
}

fn join(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

/// A download row's status line, as `DownloadRow` builds it.
pub fn download_status(d: &DownloadView, now_ms: i64) -> String {
    match d.state {
        DownloadState::Active => join(&[
            format_progress(d.received_bytes, d.total_bytes),
            if d.speed_bps > 0. { format_speed(d.speed_bps) } else { String::new() },
            format_eta(d.eta_s),
        ]),
        DownloadState::Paused => format!("Paused · {}", format_progress(d.received_bytes, d.total_bytes)),
        DownloadState::Failed => format!("Failed — {}", d.error.as_deref().unwrap_or("unknown error")),
        DownloadState::Cancelled => "Cancelled".into(),
        DownloadState::Done if d.missing => "Deleted".into(),
        DownloadState::Done => join(&[
            format_size(d.total_bytes),
            where_of(&d.context.channel_name, &d.context.server_name),
            d.finished_at.map(|at| crate::fmt::ago(at, now_ms)).unwrap_or_default(),
        ]),
    }
}

/// An upload row's status line, as `UploadRow` builds it.
pub fn upload_status(u: &UploadEntry) -> String {
    let place = where_of(&u.channel_name, &u.server_name);
    match u.state {
        UploadState::Uploading => join(&[
            format_progress(u.transferred_bytes, u.total_bytes),
            if u.speed_bps > 0. { format_speed(u.speed_bps) } else { String::new() },
            if place.is_empty() { String::new() } else { format!("to {place}") },
        ]),
        UploadState::Paused => join(&[
            format!("Paused · {}", format_progress(u.transferred_bytes, u.total_bytes)),
            if place.is_empty() { String::new() } else { format!("to {place}") },
        ]),
        UploadState::Done => [
            if place.is_empty() { "Uploaded".to_string() } else { format!("Uploaded to {place}") },
            format_size(u.total_bytes),
        ]
        .join(" · "),
        UploadState::Failed => format!("Failed — {}", u.error.as_deref().unwrap_or("upload failed")),
        UploadState::Cancelled => "Cancelled".into(),
    }
}

/// The share of a transfer done, for its bar.
pub fn fraction(done: u64, total: u64) -> f32 {
    if total > 0 { (done as f64 / total as f64).clamp(0., 1.) as f32 } else { 0. }
}

/// The button's `aria-label` / `title` (without the shortcut).
pub fn button_label(active: usize, paused: usize) -> String {
    let running = active + paused;
    if running > 0 {
        format!("Transfers · {} {}", crate::fmt::count(running as i64), if active > 0 { "active" } else { "paused" })
    } else {
        "Transfers".into()
    }
}

/// `capLabel`: MB/s rounded to two decimals, written as JavaScript writes the number.
pub fn cap_label(bps: u64) -> String {
    let mbps = (bps as f64 / (1024. * 1024.) * 100.).round() / 100.;
    format!("{mbps} MB/s")
}

/// A point of the button's 24-unit box.
pub type Unit = (f32, f32);

/// The ring's arc: its start, its end, whether it is the larger one.
pub type RingArc = (Unit, Unit, bool);

/// The ring's progress arc in the button's 24-unit box (circle r = 10 at 12,12, starting at the
/// top and running clockwise, as the SVG rotated -90° with `stroke-dasharray`): the start, the
/// end and whether the arc is the larger one. `None` past nothing; a whole circle is two halves.
pub fn ring_arc(fraction: f32) -> Option<RingArc> {
    let f = fraction.clamp(0., 1.);
    if f <= 0. {
        return None;
    }
    let angle = f * std::f32::consts::TAU;
    let start = (12., 2.);
    let end = (12. + 10. * angle.sin(), 12. - 10. * angle.cos());
    Some((start, end, f > 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use decibell_model::live::transfers::{AttachmentKind, DownloadContext};

    fn download(state: DownloadState) -> DownloadView {
        DownloadView {
            id: "d".into(),
            server_id: "1".into(),
            attachment_id: 7,
            filename: "clip.mp4".into(),
            mime: "video/mp4".into(),
            kind: AttachmentKind::Video,
            total_bytes: 44 * 1024 * 1024,
            received_bytes: 18 * 1024 * 1024 + 400 * 1024,
            state,
            error: None,
            started_at: 0,
            finished_at: Some(1_000_000),
            context: DownloadContext { server_name: "Daily Gaming".into(), channel_name: "clips".into(), ..DownloadContext::default() },
            speed_bps: 2.5 * 1024. * 1024.,
            eta_s: 11,
            missing: false,
            encrypted: false,
        }
    }

    #[test]
    fn download_lines_read_like_the_renderer() {
        assert_eq!(download_status(&download(DownloadState::Active), 0), "18.4 of 44.0 MB · 2.5 MB/s · 11 s left");
        let mut still = download(DownloadState::Active);
        still.speed_bps = 0.;
        still.eta_s = -1;
        assert_eq!(download_status(&still, 0), "18.4 of 44.0 MB");
        assert_eq!(download_status(&download(DownloadState::Paused), 0), "Paused · 18.4 of 44.0 MB");
        let mut failed = download(DownloadState::Failed);
        assert_eq!(download_status(&failed, 0), "Failed — unknown error");
        failed.error = Some("HTTP 404".into());
        assert_eq!(download_status(&failed, 0), "Failed — HTTP 404");
        assert_eq!(download_status(&download(DownloadState::Cancelled), 0), "Cancelled");
        assert_eq!(download_status(&download(DownloadState::Done), 1_000_000 + 5 * 60_000), "44.0 MB · #clips · 5 min ago");
        let mut gone = download(DownloadState::Done);
        gone.missing = true;
        assert_eq!(download_status(&gone, 0), "Deleted");
        let mut no_channel = download(DownloadState::Done);
        no_channel.context.channel_name.clear();
        no_channel.finished_at = None;
        assert_eq!(download_status(&no_channel, 0), "44.0 MB · Daily Gaming");
    }

    #[test]
    fn button_and_cap_labels() {
        assert_eq!(button_label(0, 0), "Transfers");
        assert_eq!(button_label(2, 1), "Transfers · 3 active");
        assert_eq!(button_label(0, 2), "Transfers · 2 paused");
        assert_eq!(cap_label(2 * 1024 * 1024), "2 MB/s");
        assert_eq!(cap_label(1536 * 1024), "1.5 MB/s");
        assert_eq!(cap_label(100 * 1024), "0.1 MB/s");
    }

    #[test]
    fn the_ring_runs_clockwise_from_the_top() {
        assert_eq!(ring_arc(0.), None);
        let (start, end, large) = ring_arc(0.25).unwrap();
        assert_eq!(start, (12., 2.));
        assert!((end.0 - 22.).abs() < 1e-4 && (end.1 - 12.).abs() < 1e-4 && !large);
        let (_, end, large) = ring_arc(0.75).unwrap();
        assert!((end.0 - 2.).abs() < 1e-4 && (end.1 - 12.).abs() < 1e-4 && large);
    }
}
