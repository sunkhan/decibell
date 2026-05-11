import { useUiStore } from "../../stores/uiStore";
import { saveSettings } from "../settings/saveSettings";

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

export function audioSetVolume(v: number): void {
  const clamped = Math.max(0, Math.min(1, v));
  if (element) {
    // Element-bound path: write to the element; PersistentAudioLayer's
    // `volumechange` listener mirrors the new value into uiStore and
    // persists. Single writer keeps state consistent during drags.
    element.volume = clamped;
    if (clamped > 0 && element.muted) element.muted = false;
    if (clamped === 0) element.muted = true;
    return;
  }
  // No active audio yet — write straight to the store + persist so the
  // user can tune the level *before* pressing play. The next element
  // that mounts will be seeded with this value.
  const ui = useUiStore.getState();
  ui.setMediaAudioVolume(clamped);
  if (clamped === 0 && !ui.mediaAudioMuted) ui.setMediaAudioMuted(true);
  if (clamped > 0 && ui.mediaAudioMuted) ui.setMediaAudioMuted(false);
  saveSettings();
}

export function audioToggleMute(): void {
  if (element) {
    element.muted = !element.muted;
    return;
  }
  const ui = useUiStore.getState();
  ui.setMediaAudioMuted(!ui.mediaAudioMuted);
  saveSettings();
}
