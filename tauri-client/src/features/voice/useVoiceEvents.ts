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
  const setChannelPresence = useVoiceStore((s) => s.setChannelPresence);
  const setUserState = useVoiceStore((s) => s.setUserState);
  const username = useAuthStore((s) => s.username);

  useEffect(() => {
    const unlisten: (() => void)[] = [];

    listen<{ serverId: string; channelId: string; participants: string[]; userStates: { username: string; isMuted: boolean; isDeafened: boolean }[] }>(
      "voice_presence_updated",
      (event) => {
        const { channelId, participants, userStates } = event.payload;
        // Always update per-channel presence map with user states
        setChannelPresence(channelId, participants, userStates);

        // Update connected channel participants
        const connectedId = useVoiceStore.getState().connectedChannelId;
        if (channelId === connectedId) {
          const stateMap = new Map(userStates?.map((s) => [s.username, s]) ?? []);
          setParticipants(
            participants.map((u) => ({
              username: u,
              isMuted: stateMap.get(u)?.isMuted ?? false,
              isDeafened: stateMap.get(u)?.isDeafened ?? false,
              isSpeaking: useVoiceStore.getState().speakingUsers.includes(u),
              audioLevel: 0,
            }))
          );
        }
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

    listen<{ username: string; isMuted: boolean; isDeafened: boolean }>(
      "voice_user_state_changed",
      (event) => {
        setUserState(event.payload.username, event.payload.isMuted, event.payload.isDeafened);
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
  }, [username, setSpeaking, setMuted, setDeafened, setLatency, setError, setParticipants, setChannelPresence, setUserState]);
}
