import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";

export function useVoiceEvents() {
  const setSpeaking = useVoiceStore((s) => s.setSpeaking);
  const setMuted = useVoiceStore((s) => s.setMuted);
  const setDeafened = useVoiceStore((s) => s.setDeafened);
  const setLatency = useVoiceStore((s) => s.setLatency);
  const setError = useVoiceStore((s) => s.setError);
  const setParticipants = useVoiceStore((s) => s.setParticipants);
  const username = useAuthStore((s) => s.username);

  useEffect(() => {
    const unlisten: (() => void)[] = [];

    listen<{ serverId: string; channelId: string; participants: string[] }>(
      "voice_presence_updated",
      (event) => {
        setParticipants(
          event.payload.participants.map((u) => ({
            username: u,
            isMuted: false,
            isSpeaking: useVoiceStore.getState().speakingUsers.includes(u),
            audioLevel: 0,
          }))
        );
      }
    ).then((u) => unlisten.push(u));

    listen<{ username: string; speaking: boolean }>(
      "voice_user_speaking",
      (event) => {
        const speakingUsername =
          event.payload.username === "__local__"
            ? username ?? ""
            : event.payload.username;
        if (speakingUsername) {
          setSpeaking(speakingUsername, event.payload.speaking);
        }
      }
    ).then((u) => unlisten.push(u));

    listen<{ isMuted: boolean; isDeafened: boolean }>(
      "voice_state_changed",
      (event) => {
        setMuted(event.payload.isMuted);
        setDeafened(event.payload.isDeafened);
      }
    ).then((u) => unlisten.push(u));

    listen<{ latencyMs: number }>("voice_ping_updated", (event) => {
      setLatency(event.payload.latencyMs);
    }).then((u) => unlisten.push(u));

    listen<{ message: string }>("voice_error", (event) => {
      setError(event.payload.message);
    }).then((u) => unlisten.push(u));

    return () => {
      unlisten.forEach((fn) => fn());
    };
  }, [username, setSpeaking, setMuted, setDeafened, setLatency, setError, setParticipants]);
}
