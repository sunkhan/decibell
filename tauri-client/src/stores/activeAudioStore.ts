import { create } from "zustand";

// Holds the currently-active audio attachment and the playback state
// mirrored from the persistent-layer <audio> element. Only one audio
// attachment plays at a time (clicking play on another replaces it).
//
// The chat-side AudioPlayer becomes a controller widget that reads
// state from here and sends commands via audioController; the actual
// <audio> element lives in PersistentAudioLayer at the app level so
// playback survives Virtuoso row unmounts when the user scrolls.

export interface ActiveAudio {
  attachmentId: number;
  serverId: string;
  channelId: string;
  // localhost media-server URL (decibell-attach-* served over HTTP).
  src: string;
  // Temp file path on disk — held so the persistent layer can unlink
  // it when active changes or the channel is left.
  tempPath: string;
  filename: string;
}

interface State {
  active: ActiveAudio | null;
  // Mirrored from the <audio> element so chat-side controls re-render.
  playing: boolean;
  time: number;
  duration: number;
  setActive: (a: ActiveAudio | null) => void;
  setPlaybackState: (s: Partial<Pick<State, "playing" | "time" | "duration">>) => void;
}

export const useActiveAudioStore = create<State>((set) => ({
  active: null,
  playing: false,
  time: 0,
  duration: 0,
  // Reset playback state when active changes — old time/duration
  // values would briefly show on the new attachment otherwise.
  setActive: (a) => set({ active: a, playing: false, time: 0, duration: 0 }),
  setPlaybackState: (s) => set(s),
}));
