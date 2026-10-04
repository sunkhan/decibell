// Stream thumbnails: the poster image a streamer broadcasts to everyone in
// the voice channel (stream tiles, the profile popup). Keep these in step
// with the native capture paths (native/src/media/thumb_encode.rs) and the
// community server's MAX_STREAM_THUMB_BYTES (src/community/main.cpp).

/// Longest edge, px. Stream tiles in the voice view reach ~960 px wide.
export const THUMBNAIL_MAX_EDGE = 960;
/// The community server drops bigger thumbnails silently.
export const THUMBNAIL_MAX_BYTES = 256 * 1024;
/// JPEG qualities tried in order until one fits THUMBNAIL_MAX_BYTES.
export const THUMBNAIL_QUALITY_LADDER = [0.8, 0.65, 0.5] as const;
/// Steady-state cadence.
export const THUMBNAIL_INTERVAL_MS = 15_000;
/// The first thumbnail comes from the first frame, often black or
/// half-drawn while capture warms up; a second follows this soon after.
export const THUMBNAIL_FOLLOWUP_MS = 2_000;
