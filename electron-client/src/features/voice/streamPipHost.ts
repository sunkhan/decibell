/// Shared, persistent DOM host for the single live stream player.
///
/// StreamVideoPlayer is rendered ONCE (a React portal in StreamPipManager) into
/// this detached host node. The full StreamViewPanel and the floating
/// MiniStreamPlayer each reparent this SAME node into their own slot with
/// placeStreamPip(). Because the node (and the live decoder inside it) is MOVED
/// rather than remounted, switching between views — or popping out to the mini
/// player — is seamless: the video never stops, no keyframe wait, no black frame.
///
/// The host is `pointer-events: none` (inherited by the canvas): it's purely
/// visual, so it never intercepts clicks/drags meant for the player chrome that
/// sits around it (the mini's drag surface + close/expand buttons, the full
/// view's controls). Those live on the slot's own container, above/around this.
let host: HTMLDivElement | null = null;

export function getStreamPipHost(): HTMLDivElement {
  if (!host) {
    host = document.createElement("div");
    host.style.width = "100%";
    host.style.height = "100%";
    host.style.pointerEvents = "none";
  }
  return host;
}

/// Reparent the persistent stream host into `container` (a no-op if it already
/// lives there). Moving the node keeps the decoder alive across the transition.
export function placeStreamPip(container: HTMLElement): void {
  const node = getStreamPipHost();
  if (node.parentElement !== container) container.appendChild(node);
}

/// Retained for call-site compatibility; there is no cached animation state to
/// reset now that placement is a plain reparent.
export function resetStreamPipRect(): void {}

// Last on-screen rects of the two player surfaces, so each can animate a
// shrink/grow morph *from* the other's last position when the video moves
// between them (the previous surface is usually already unmounted by then).
let fullViewRect: DOMRect | null = null;
let miniRect: DOMRect | null = null;

export function recordFullViewRect(el: HTMLElement | null): void {
  if (el) fullViewRect = el.getBoundingClientRect();
}
export function getFullViewRect(): DOMRect | null {
  return fullViewRect;
}
export function recordMiniRect(el: HTMLElement | null): void {
  if (el) miniRect = el.getBoundingClientRect();
}
export function getMiniRect(): DOMRect | null {
  return miniRect;
}
