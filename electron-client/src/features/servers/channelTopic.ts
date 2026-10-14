/// The server's cap on a channel topic, in UTF-8 bytes.
export const TOPIC_MAX_BYTES = 1024;

const encoder = new TextEncoder();

export function topicBytes(topic: string): number {
  return encoder.encode(topic).length;
}

/// A topic is one line: line breaks and tabs become spaces (the server
/// drops control characters outright, which would glue the lines
/// together), and it's cut to the byte cap on a character boundary so
/// the server never truncates what the user saw.
export function normalizeTopicInput(input: string): string {
  const line = input.replace(/[\r\n\t]+/g, " ");
  if (topicBytes(line) <= TOPIC_MAX_BYTES) return line;
  let out = "";
  let bytes = 0;
  for (const ch of line) {
    const n = topicBytes(ch);
    if (bytes + n > TOPIC_MAX_BYTES) break;
    out += ch;
    bytes += n;
  }
  return out;
}
