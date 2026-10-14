import { clampLine, utf8Bytes } from "../../lib/utf8";

/// The server's cap on a channel topic, in UTF-8 bytes.
export const TOPIC_MAX_BYTES = 1024;

export const topicBytes = utf8Bytes;

/// A topic is one line, cut to the byte cap on a character boundary so
/// the server never truncates what the user saw.
export function normalizeTopicInput(input: string): string {
  return clampLine(input, TOPIC_MAX_BYTES);
}
