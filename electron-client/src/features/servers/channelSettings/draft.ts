import type { ChannelInfo } from "../../../types";

export type RetentionField =
  | "retentionDaysText"
  | "retentionDaysImage"
  | "retentionDaysVideo"
  | "retentionDaysDocument"
  | "retentionDaysAudio";

export const RETENTION_FIELDS: RetentionField[] = [
  "retentionDaysText",
  "retentionDaysImage",
  "retentionDaysVideo",
  "retentionDaysDocument",
  "retentionDaysAudio",
];

/// Everything the Overview and Retention tabs edit, held as one draft
/// until the save bar commits it. Permission edits keep their own draft
/// (usePermissionDraft) since a different call and permission sends them.
export interface ChannelDraft {
  name: string;
  topic: string;
  slowmode: number;
  /// kbps; 0 = the client default.
  bitrate: number;
  retention: Record<RetentionField, number>;
}

export function draftOf(c: ChannelInfo): ChannelDraft {
  return {
    name: c.name,
    topic: c.topic ?? "",
    slowmode: c.slowmodeSeconds ?? 0,
    bitrate: c.voiceBitrateKbps,
    retention: {
      retentionDaysText: Math.max(0, c.retentionDaysText),
      retentionDaysImage: Math.max(0, c.retentionDaysImage),
      retentionDaysVideo: Math.max(0, c.retentionDaysVideo),
      retentionDaysDocument: Math.max(0, c.retentionDaysDocument),
      retentionDaysAudio: Math.max(0, c.retentionDaysAudio),
    },
  };
}

/// Which parts of `d` differ from the channel as stored. Only the fields
/// the channel's type shows count.
export function changesOf(c: ChannelInfo, d: ChannelDraft) {
  const s = draftOf(c);
  const text = c.type === "text";
  const name = d.name.trim().length > 0 && d.name.trim() !== s.name;
  const topic = text && d.topic.trim() !== s.topic;
  const slowmode = text && d.slowmode !== s.slowmode;
  const bitrate = c.type === "voice" && d.bitrate !== s.bitrate;
  const retention = text && RETENTION_FIELDS.some((f) => d.retention[f] !== s.retention[f]);
  return { name, topic, slowmode, bitrate, retention, any: name || topic || slowmode || bitrate || retention };
}
