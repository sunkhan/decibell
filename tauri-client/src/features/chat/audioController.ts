// Module-level handle to the persistent-layer <audio> element. Bound
// by PersistentAudioLayer when its element mounts. The chat-side
// AudioPlayer's controls call into this directly rather than passing
// an imperative ref through stores or props — keeps the data layer
// (activeAudioStore) reactive and the control layer simple.

let element: HTMLAudioElement | null = null;

export function bindAudioElement(el: HTMLAudioElement | null): void {
  element = el;
}

export function audioToggle(): void {
  if (!element) return;
  if (element.paused) element.play().catch(() => {});
  else element.pause();
}

export function audioPause(): void {
  element?.pause();
}

export function audioSeek(t: number): void {
  if (!element) return;
  const max = element.duration || 0;
  element.currentTime = Math.max(0, Math.min(max, t));
}
