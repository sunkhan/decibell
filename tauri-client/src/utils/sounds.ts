import { invoke } from "@tauri-apps/api/core";

type SoundName =
  | "mute"
  | "unmute"
  | "deafen"
  | "undeafen"
  | "user_join"
  | "user_leave"
  | "stream_start"
  | "stream_stop"
  | "connect"
  | "disconnect";

export function playSound(name: SoundName) {
  invoke("play_sound", { name }).catch(() => {});
}
