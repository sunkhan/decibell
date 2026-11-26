// Shared by the voice pages (voice-*, go-live*, call-*): the renderer's real MainLayout fed the
// GPUI voice fixture (crates/decibell-voice/src/fixture.rs, exported as JSON by
// `cargo run -p decibell-voice --features fixtures --bin decibell-voice-fixture -- pages/fixtures`):
// the shell's events and the scene's (voice rosters, streams, the MLS state, a ping, who speaks)
// through the renderer's own event hooks (_shell.tsx), then what the GPUI fixture does as the
// user (`apply()`): join Lounge, watch / focus / load streams, the posters, the view, the Go Live
// dialog, a call. Drawn as the whole window (Framed): the dialog, the incoming card and the
// mini-player place themselves against it, as in the app.
import { useEffect, useState } from "react";
import CaptureSourcePicker from "@app/features/voice/CaptureSourcePicker";
import { STREAM_PRESETS, presetSettings } from "@app/features/voice/goLive/presets";
import { probeEncoders } from "@app/utils/encoderProbe";
import { useVoiceStore } from "@app/stores/voiceStore";
import { useUiStore } from "@app/stores/uiStore";
import { useDmStore } from "@app/stores/dmStore";
import { useCallStore } from "@app/stores/callStore";
import { useCodecSettingsStore } from "@app/stores/codecSettingsStore";
import type { CodecCapability } from "@app/types";
import { Framed } from "./_frame";
import { ShellPage, replay, type ShellFixture } from "./_shell";
import { $, centre, click, frames, hover, respond, sleep } from "../runtime/harness";

export type VoiceFixture = ShellFixture & {
  voiceView: "voice" | "server" | "dm";
  watching: number[];
  focused: number | null;
  pip: number | null;
  thumbnails: Record<string, string>;
  speaking: number[];
  goLive: { preset: string; advanced: boolean; software: boolean } | null;
  call:
    | { status: "incoming"; peer: number; peerName: string }
    | { status: "active"; peer: number; peerName: string; path: "host" | "srflx"; verified: boolean; latency: number; speaking: number[] }
    | null;
};

/// The Go Live dialog is VoicePanel's component state (`showPicker`): the page opens the real
/// CaptureSourcePicker itself when the fixture asks for it.
let openPicker: ((open: boolean) => void) | null = null;

function GoLiveHost() {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    openPicker = setOpen;
    return () => {
      openPicker = null;
    };
  }, []);
  const v = useVoiceStore();
  if (!open || !v.connectedServerId || !v.connectedChannelId) return null;
  return <CaptureSourcePicker serverId={v.connectedServerId} channelId={v.connectedChannelId} onClose={() => setOpen(false)} />;
}

export function VoicePage() {
  return (
    <Framed>
      <ShellPage />
      <GoLiveHost />
    </Framed>
  );
}

export const size = { width: 1280, height: 800 };

/// The probe the GPUI fixture's Go Live screens show: NVENC for every codec, or SVT-AV1 only.
function encodeCaps(software: boolean): CodecCapability[] {
  return software
    ? [{ codec: 4, maxWidth: 3840, maxHeight: 2160, maxFps: 60, hardware: false }]
    : [
        { codec: 1, maxWidth: 3840, maxHeight: 2160, maxFps: 120, hardware: true },
        { codec: 3, maxWidth: 3840, maxHeight: 2160, maxFps: 120, hardware: true },
        { codec: 4, maxWidth: 3840, maxHeight: 2160, maxFps: 120, hardware: true },
      ] as CodecCapability[];
}

export async function replayVoice(fixture: VoiceFixture) {
  // The GPUI client streams through the native encoder only: the renderer's native path, so
  // "Share sound" is offered (a hardware probe answer turns it on; the caps shown are set below).
  respond("probeNativeEncoders", [
    { codec: 1, maxWidth: 3840, maxHeight: 2160, maxFps: 120, hardware: true, encoderName: "h264_nvenc" },
  ]);
  await probeEncoders(true).catch(() => []);

  await replay(fixture as ShellFixture);
  const voice = useVoiceStore.getState();
  // Scenes without voice events of their own (the calls) keep the shell fixture's Squad session:
  // the GPUI fixture's `shell::apply(Scene::Server)` delivers its engine state, a 38 ms ping and
  // perf2 talking after the events (the shell JSON's `voice_session_events`, which this scene's
  // JSON does not carry).
  if (fixture.nav.voice?.channel === "squad") {
    voice.setMuted(false);
    voice.setLatency(38);
    voice.setSpeaking(7, true);
  }
  for (const [uid, url] of Object.entries(fixture.thumbnails)) voice.setStreamThumbnail(Number(uid), url);
  for (const uid of fixture.speaking) voice.setSpeaking(uid, true);
  for (const uid of fixture.watching) voice.addWatching(uid);
  if (fixture.focused != null) voice.setFullscreenStream(fixture.focused);
  if (fixture.pip != null) voice.setPipStream(fixture.pip);
  if (fixture.voiceView === "voice") useUiStore.getState().setActiveView("voice");
  await frames(4);

  if (fixture.goLive) {
    const preset = STREAM_PRESETS.find((p) => p.id === fixture.goLive!.preset);
    if (preset) voice.setStreamSettings(presetSettings(preset));
    useCodecSettingsStore.setState({ encodeCaps: encodeCaps(fixture.goLive.software), loaded: true });
    openPicker?.(true);
    // The dialog fades in over two frames + 300 ms.
    await sleep(450);
    await frames(2);
    if (fixture.goLive.advanced && !document.querySelector('button[aria-expanded="true"]')) {
      const advanced = [...document.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")].find((b) =>
        b.textContent?.includes("Advanced"),
      );
      if (advanced) await click(...centre(advanced));
      // Rest the pointer where nothing reacts (the disclosure would keep its hover wash).
      await hover(1275, 795);
    }
  }

  if (fixture.call?.status === "incoming") {
    useCallStore.getState().setIncoming({
      callId: "fixture-call",
      fromUid: fixture.call.peer,
      from: fixture.call.peerName,
      pubKey: "AAAA",
      candidates: [],
      pubKeySig: null,
      keyId: 0,
    } as Parameters<ReturnType<typeof useCallStore.getState>["setIncoming"]>[0]);
    await sleep(250);
  } else if (fixture.call?.status === "active") {
    const call = fixture.call;
    // Accepting leaves community voice: the call is the session (one engine).
    voice.disconnect();
    const v = useVoiceStore.getState();
    v.setCallPeer(call.peer);
    v.setParticipants([
      { uid: fixture.me.uid, username: fixture.me.username, isMuted: false, isDeafened: false, isSpeaking: false, audioLevel: 0 },
      { uid: call.peer, username: call.peerName, isMuted: false, isDeafened: false, isSpeaking: false, audioLevel: 0 },
    ] as Parameters<typeof v.setParticipants>[0]);
    v.setLatency(call.latency);
    for (const uid of call.speaking) v.setSpeaking(uid, true);
    const store = useCallStore.getState();
    store.startOutgoing("fixture-call", call.peer);
    store.setActive(call.path, call.verified);
    // The duration reads 00:00 for the capture (CallDuration ticks from startedAt).
    useCallStore.setState({ startedAt: Date.now() + 60_000 });
    useDmStore.getState().setActiveDmUser(call.peer);
    useUiStore.getState().setActiveView("dm");
  }
  await frames(4);
  return { framed: true, scene: fixture.scene, events: fixture.events.length };
}

export { $ };
