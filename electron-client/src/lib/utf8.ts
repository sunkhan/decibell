const encoder = new TextEncoder();

export function utf8Bytes(text: string): number {
  return encoder.encode(text).length;
}

/// A one-line field as the community server will store it (its
/// clamp_utf8): line breaks and tabs become spaces (the server drops
/// control characters outright, which would glue the words together),
/// other control characters go, and the text is cut to `maxBytes` on a
/// character boundary. Applied while typing, so a save comes back exactly
/// as the user saw it.
export function clampLine(input: string, maxBytes: number): string {
  // eslint-disable-next-line no-control-regex
  const line = input.replace(/[\r\n\t]+/g, " ").replace(/[\u0000-\u001f\u007f]/g, "");
  if (utf8Bytes(line) <= maxBytes) return line;
  let out = "";
  let bytes = 0;
  for (const ch of line) {
    const n = utf8Bytes(ch);
    if (bytes + n > maxBytes) break;
    out += ch;
    bytes += n;
  }
  return out;
}
