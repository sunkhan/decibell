import * as path from "node:path";

// Paths the user has explicitly chosen via a native save dialog. The
// renderer's `fs.writeFile` may only write to a path in this set, so a
// compromised renderer can't write to arbitrary locations (autostart
// entries, the app's own unpacked binaries, shell profiles, …).
// Single-use: consumed on the matching write.
const approved = new Set<string>();

export function approveWritePath(p: string): void {
  approved.add(path.resolve(p));
  // Soft cap: a runaway of save-dialog confirmations without matching
  // writes shouldn't grow unbounded. 256 is far above any real backlog.
  if (approved.size > 256) {
    const first = approved.values().next().value;
    if (first !== undefined) approved.delete(first);
  }
}

export function consumeWritePath(p: string): boolean {
  return approved.delete(path.resolve(p));
}
