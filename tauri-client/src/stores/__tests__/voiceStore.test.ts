import { describe, it, expect, beforeEach } from "vitest";
import { useVoiceStore } from "../voiceStore";

describe("voiceStore", () => {
  beforeEach(() => {
    useVoiceStore.setState({
      connectedServerId: null,
      connectedChannelId: null,
      participants: [],
      activeStreams: [],
      isMuted: false,
      isDeafened: false,
      speakingUsers: [],
      latencyMs: null,
      error: null,
    });
  });

  it("sets connected channel with server ID", () => {
    useVoiceStore.getState().setConnectedChannel("srv1", "vc1");
    const s = useVoiceStore.getState();
    expect(s.connectedServerId).toBe("srv1");
    expect(s.connectedChannelId).toBe("vc1");
  });

  it("sets participants", () => {
    useVoiceStore.getState().setParticipants([
      { username: "alice", isMuted: false, isSpeaking: false, audioLevel: 0 },
    ]);
    expect(useVoiceStore.getState().participants).toHaveLength(1);
  });

  it("sets speaking user", () => {
    useVoiceStore.getState().setSpeaking("alice", true);
    expect(useVoiceStore.getState().speakingUsers).toContain("alice");
    useVoiceStore.getState().setSpeaking("alice", false);
    expect(useVoiceStore.getState().speakingUsers).not.toContain("alice");
  });

  it("deafen implies mute", () => {
    useVoiceStore.getState().setDeafened(true);
    const s = useVoiceStore.getState();
    expect(s.isDeafened).toBe(true);
    expect(s.isMuted).toBe(true);
  });

  it("disconnect clears all state", () => {
    useVoiceStore.getState().setConnectedChannel("srv1", "vc1");
    useVoiceStore.getState().setSpeaking("alice", true);
    useVoiceStore.getState().setLatency(48);
    useVoiceStore.getState().disconnect();
    const s = useVoiceStore.getState();
    expect(s.connectedServerId).toBeNull();
    expect(s.connectedChannelId).toBeNull();
    expect(s.speakingUsers).toHaveLength(0);
    expect(s.latencyMs).toBeNull();
  });
});
