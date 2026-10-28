import { useState, useEffect, useCallback } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import { VideoCodec, type CaptureSource } from "../../types";
import { playSound } from "../../utils/sounds";
import {
  activeStreamCapture,
  startActiveStream,
  stopActiveStream,
} from "./streaming/StreamCapture";
import { endStreamFromCapture } from "./streaming/streamEnded";
import { isNativeEncodeActive } from "../../utils/encoderProbe";
import { announceCallStreamStart } from "../call/callActions";
import Switch from "../../components/Switch";
import StreamAudioAppPicker from "./StreamAudioAppPicker";
import { canPickStreamAudioApps } from "./streamAudioFilter";
import { useChatStore } from "../../stores/chatStore";
import { useUserName } from "../../stores/usersStore";
import { Icon } from "../servers/tabs/settingsUi";
import { Field, Group, Row } from "../servers/channelSettings/ui";
import SourceGrid from "./goLive/SourceGrid";
import QualityPresets from "./goLive/QualityPresets";
import AdvancedSettings from "./goLive/AdvancedSettings";
import { matchPreset, resolutionLabel } from "./goLive/presets";
import { useMediaQuery } from "../../hooks/useMediaQuery";

interface Props {
  /// Community voice channel to announce in. Both absent inside a P2P DM
  /// call — the stream is announced to the peer over central instead.
  serverId?: string;
  channelId?: string;
  onClose: () => void;
}

/// Platforms where we render our own tabbed source picker. Linux is
/// excluded — getDisplayMedia goes through xdg-desktop-portal there
/// and the portal dialog is the picker. macOS uses Electron's
/// `useSystemPicker: true` to get the native ScreenCaptureKit dialog.
const NEEDS_CUSTOM_PICKER =
  typeof window !== "undefined" && window.decibell?.platform === "win32";

export default function CaptureSourcePicker({
  serverId,
  channelId,
  onClose,
}: Props) {
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [visible, setVisible] = useState(false);
  const [closing, setClosing] = useState(false);
  const [picked, setPicked] = useState<CaptureSource | null>(null);
  const pickedSourceId = picked?.id ?? null;

  const streamSettings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);
  const quality = matchPreset(streamSettings);
  // Settings that match no preset open Advanced, where they were made.
  const [advancedOpen, setAdvancedOpen] = useState(() => quality === null);
  // Stream sound exists only on the native capture path; elsewhere the
  // switch would announce sound that never arrives.
  const soundAvailable = canPickStreamAudioApps();
  const shareAudio = soundAvailable && streamSettings.shareAudio;

  useEffect(() => {
    requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
  }, []);

  const handleClose = useCallback(() => {
    if (closing) return;
    setClosing(true);
    setVisible(false);
  }, [closing]);

  const handleTransitionEnd = useCallback(() => {
    if (!visible && closing) onClose();
  }, [visible, closing, onClose]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") handleClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [handleClose]);

  // PR8: Chromium's `getDisplayMedia` triggers the OS-native screen-share
  // dialog when StreamCapture.start() runs, so the picker no longer needs
  // its own source list. Settings + Go Live only.
  //
  // For "source" the real size comes from the capture (useNativeSize flag
  // below): the native pipeline encodes the source's own size within its
  // encoder's ceiling and reports it back; the WebCodecs path reads it off
  // the negotiated track. The 1440p stand-in here only feeds the WebCodecs
  // encoder's pre-flight bitrate check, before Chromium has negotiated.
  const resolveDimensions = (): { width: number; height: number } => {
    switch (streamSettings.resolution) {
      case "720p":
        return { width: 1280, height: 720 };
      case "source":
        return { width: 2560, height: 1440 };
      default:
        return { width: 1920, height: 1080 };
    }
  };

  const handleGoLive = async () => {
    if (NEEDS_CUSTOM_PICKER && !pickedSourceId) {
      setError("Pick a screen or window to share first.");
      return;
    }
    setStarting(true);
    setError(null);
    try {
      const dims = resolveDimensions();
      const codec =
        streamSettings.enforcedCodec === VideoCodec.UNKNOWN
          ? VideoCodec.H264_HW
          : streamSettings.enforcedCodec;

      // Renderer side first: prompt for capture source, peek the
      // first frame, configure the encoder. start() returns the
      // *actual* dimensions Chromium negotiated — those are what we
      // announce to the server, so the resolution badge and presence
      // payload reflect reality even when the user picked "Source"
      // and we couldn't predict it. If the user cancels the OS
      // dialog, getDisplayMedia rejects and we never bother native.
      const stream = await startActiveStream({
        codec,
        width: dims.width,
        height: dims.height,
        fps: streamSettings.fps,
        bitrateKbps: streamSettings.videoBitrateKbps,
        shareAudio,
        audioBitrateKbps: streamSettings.audioBitrateKbps,
        audioMode: streamSettings.audioMode,
        audioApps: streamSettings.audioApps,
        includeCursor: streamSettings.includeCursor,
        // Every 5 fps stream (Slides, or 5 fps under Advanced) is encoded
        // for detail; see start_screen_share's `detail`.
        detail: streamSettings.fps === 5,
        serverId,
        channelId,
        useNativeSize: streamSettings.resolution === "source",
        sourceId: pickedSourceId ?? undefined,
        // The capture ended under the stream (track ended, encoder or pump
        // failure; the failures toast themselves). Never stop with this
        // picker's ids: a stream taken along to another channel lives
        // there now, and stopping here left it LIVE. Only while this is
        // still the live session — a stale one must not end the next.
        onCaptureEnded: () => {
          if (activeStreamCapture() === stream) {
            endStreamFromCapture({ kind: "source", notify: false });
          }
        },
      });
      let actualDims: { width: number; height: number };
      try {
        actualDims = await stream.start();
      } catch (e) {
        await stopActiveStream();
        throw e;
      }
      // What the session really encodes: a native→WebCodecs fallback
      // downgrades HEVC/AV1 to H.264. Announcing the requested codec made
      // the server enforce HEVC on watchers of an H.264 stream. An
      // enforcement the stream no longer satisfies is dropped.
      const activeCodec = stream.activeCodec;
      const enforcedCodec =
        streamSettings.enforcedCodec && streamSettings.enforcedCodec === activeCodec
          ? streamSettings.enforcedCodec
          : 0;
      // The session can end while we await (encoder error, native failure
      // event, a stop from elsewhere). Don't resurrect it as "streaming".
      const superseded = () => activeStreamCapture() !== stream;
      if (superseded()) throw new Error("Stream ended while starting");

      // Native signaling: register the stream with the truthful
      // dimensions. On Linux/macOS this is where StartStreamReq
      // goes out + VideoEngine is created (the renderer's WebCodecs
      // encoder, already running via stream.start() above, will start
      // pumping send_video_frame in a few ms).
      //
      // On the native path (Windows always; Linux when a HW encoder was
      // probed) stream.start() ALREADY invoked start_screen_share to spin
      // up native capture + encode AND send the StartStreamReq — calling
      // it again bounces off the "Already sharing screen" guard. Only the
      // renderer-WebCodecs path (Linux without native, macOS) needs this
      // separate call to create the VideoEngine sender + announce.
      if (!isNativeEncodeActive()) {
        try {
          await invoke("start_screen_share", {
            serverId,
            channelId,
            fps: streamSettings.fps,
            width: actualDims.width,
            height: actualDims.height,
            videoBitrateKbps: streamSettings.videoBitrateKbps,
            shareAudio,
            audioBitrateKbps: streamSettings.audioBitrateKbps,
            audioMode: streamSettings.audioMode,
            audioApps: streamSettings.audioApps,
            initialCodec: activeCodec,
            enforcedCodec,
            // Explicit false: the renderer owns capture + encode on this
            // branch. Windows honours this now (WebCodecs fallback) —
            // omitting it defaults to the native pipeline there.
            nativeEncode: false,
            includeCursor: streamSettings.includeCursor,
          });
        } catch (e) {
          await stopActiveStream();
          throw e;
        }
      }

      if (superseded()) {
        // Its stop may have run before the start_screen_share above
        // created the WebCodecs-path VideoEngine — tear that down too.
        if (!isNativeEncodeActive()) {
          await invoke("stop_screen_share", { serverId, channelId }).catch(() => {});
        }
        throw new Error("Stream ended while starting");
      }
      useVoiceStore.getState().setIsStreaming(true);
      // P2P DM call: no community presence broadcast — tell the peer
      // directly (no-op outside a call).
      announceCallStreamStart({
        codec: activeCodec,
        width: actualDims.width,
        height: actualDims.height,
        fps: streamSettings.fps,
        hasAudio: shareAudio,
      });
      playSound("stream_start");
      handleClose();
    } catch (e) {
      setError(String(e));
      setStarting(false);
    }
  };

  // Two layouts. Where the window has room (withPanel, below), the dialog is
  // a main card (what to share, quality, Go Live) with an Options panel
  // beside it (sound, cursor, Advanced). Otherwise it's one card holding
  // everything; on Windows its source grid sits side by side with the
  // settings from `lg`, stacked below. Either way the header and footer are
  // pinned — the body scrolls, never the Go Live button.
  const split = NEEDS_CUSTOM_PICKER;
  const platform = window.decibell.platform;
  // "Smooth · 1080p · 60 fps · with sound", beside the Go Live button.
  const qualitySummary = [
    quality
      ? `${quality.label} · ${quality.spec}`
      : `Custom · ${resolutionLabel(streamSettings.resolution)} · ${streamSettings.fps} fps`,
    shareAudio ? "with sound" : null,
  ]
    .filter(Boolean)
    .join(" · ");

  // Room for the options panel beside the dialog: the main card, the 336 px
  // panel and their gap, inside the backdrop's padding. Narrower windows keep
  // everything in one card.
  const withPanel = useMediaQuery(split ? "(width >= 74rem)" : "(width >= 55rem)");

  const optionRows = (
    <>
      <Row
        icon="speaker"
        title="Share sound"
        description={
          !soundAvailable
            ? platform === "darwin"
              ? "Not available on macOS yet."
              : "Needs hardware video encoding, which isn't available here."
            : streamSettings.shareAudio
              ? "Your apps, never your voice chat."
              : "Viewers get video only."
        }
        control={
          <Switch
            checked={shareAudio}
            disabled={!soundAvailable || starting}
            label="Share sound"
            onToggle={() =>
              setStreamSettings({ shareAudio: !streamSettings.shareAudio })
            }
          />
        }
        below={
          shareAudio ? (
            <div className="mt-3.5">
              <StreamAudioAppPicker sourceId={picked?.id} />
            </div>
          ) : undefined
        }
      />
      <Row
        icon="pointer"
        title="Show my cursor"
        description="Viewers see where you point."
        control={
          <Switch
            checked={streamSettings.includeCursor}
            disabled={starting}
            label="Show my cursor"
            onToggle={() =>
              setStreamSettings({
                includeCursor: !streamSettings.includeCursor,
              })
            }
          />
        }
      />
    </>
  );

  const advanced = (
    <AdvancedSettings
      open={advancedOpen}
      onToggle={() => setAdvancedOpen((o) => !o)}
      showSoundBitrate={shareAudio}
      disabled={starting}
      flat={withPanel}
    />
  );

  const sourceArea = NEEDS_CUSTOM_PICKER ? (
    <SourceGrid picked={picked} onPick={setPicked} disabled={starting} />
  ) : (
    <div className="mx-5 mb-5 flex shrink-0 items-center gap-4 rounded-lg border border-border-divider bg-bg-light px-4 py-3">
      <SystemPickerArt />
      <div className="min-w-0">
        <p className="text-[14px] font-medium text-text-primary">
          You'll pick a screen or window next
        </p>
        <p className="mt-0.5 text-[12px] leading-[1.55] text-text-muted">
          After you press Go Live, {platform === "darwin" ? "macOS" : "your system"} asks what to share.
        </p>
      </div>
    </div>
  );

  // The main card's size: beside the panel it's narrower (Windows: the
  // grid over a row of quality cards); alone it's the one-card dialog,
  // side by side with its settings column on a wide Windows window.
  const cardSize = withPanel
    ? split
      ? "w-[760px] h-[min(720px,calc(100vh-3rem))]"
      : "w-[460px]"
    : split
      ? "w-full max-w-[560px] lg:h-[min(760px,calc(100vh-3rem))] lg:max-w-[1040px]"
      : "w-full max-w-[520px]";

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6 transition-colors duration-300"
      style={{
        backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)",
      }}
      onClick={(e) => e.target === e.currentTarget && handleClose()}
      onTransitionEnd={handleTransitionEnd}
    >
      {/* The two cards are one dialog. The row between them lets clicks
          through to the backdrop, so a click in the gap closes it too. */}
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="go-live-title"
        className="pointer-events-none flex w-full items-start justify-center gap-3 transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
      >
        <div
          className={`pointer-events-auto flex max-h-[calc(100vh-3rem)] flex-col overflow-hidden rounded-xl border border-border bg-bg-secondary shadow-modal ${cardSize}`}
        >
          <div className="flex shrink-0 items-center gap-3.5 px-5 pt-5 pb-4">
            <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-accent-soft text-accent">
              <Icon name="screenShare" size={19} />
            </span>
            <div className="min-w-0 flex-1">
              <h2
                id="go-live-title"
                className="font-display text-[16px] font-semibold text-text-bright"
              >
                Go live
              </h2>
              <Destination serverId={serverId} channelId={channelId} />
            </div>
            <button
              onClick={handleClose}
              aria-label="Close"
              className="flex h-8 w-8 shrink-0 items-center justify-center self-start rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            >
              <Icon name="x" size={16} />
            </button>
          </div>

          {withPanel ? (
            split ? (
              <div className="flex min-h-0 flex-1 flex-col border-t border-border-divider">
                {sourceArea}
                <div className="shrink-0 border-t border-border-divider px-5 pt-4 pb-5">
                  <Field label="Quality">
                    <QualityPresets row disabled={starting} />
                  </Field>
                </div>
              </div>
            ) : (
              <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
                {sourceArea}
                <div className="shrink-0 px-5 pb-5">
                  <Field label="Quality">
                    <QualityPresets disabled={starting} />
                  </Field>
                </div>
              </div>
            )
          ) : (
            <div
              className={`flex min-h-0 flex-1 flex-col overflow-y-auto ${
                split ? "lg:flex-row lg:overflow-hidden lg:border-t lg:border-border-divider" : ""
              }`}
            >
              {sourceArea}
              <div
                className={`flex shrink-0 flex-col gap-5 px-5 pb-5 ${
                  split ? "lg:w-[400px] lg:overflow-y-auto lg:border-l lg:border-border-divider lg:pt-5" : ""
                }`}
              >
                <Field label="Quality">
                  <QualityPresets disabled={starting} />
                </Field>
                <Group label="Options">{optionRows}</Group>
                {advanced}
              </div>
            </div>
          )}

          <div className="shrink-0 border-t border-border-divider px-5 py-3.5">
            {error && (
              <div
                role="alert"
                className="mb-3 flex items-start gap-2 rounded-md bg-error/10 px-3 py-2 text-[12px] text-error"
              >
                <Icon name="alert" size={14} className="mt-px" />
                <span className="line-clamp-3 break-words" title={error}>
                  {error}
                </span>
              </div>
            )}
            <div className="flex items-center gap-2">
              <div className="flex min-w-0 flex-1 items-center gap-2.5">
                {!NEEDS_CUSTOM_PICKER ? (
                  <p className="truncate text-[12px] text-text-muted">
                    {qualitySummary}
                  </p>
                ) : picked ? (
                  <>
                    {picked.thumbnail && (
                      <img
                        src={picked.thumbnail}
                        alt=""
                        draggable={false}
                        className="h-7 w-12 shrink-0 rounded-sm bg-bg-darkest object-contain"
                      />
                    )}
                    <span className="min-w-0">
                      <span className="block truncate text-[13px] font-medium text-text-primary" title={picked.name}>
                        {picked.name}
                      </span>
                      <span className="block truncate text-[12px] tabular-nums text-text-muted">
                        {qualitySummary}
                      </span>
                    </span>
                  </>
                ) : (
                  <p className="truncate text-[12px] text-text-muted">
                    Pick a screen or window to share.
                  </p>
                )}
              </div>
              <button
                onClick={handleClose}
                className="rounded-sm border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
              >
                Cancel
              </button>
              <button
                onClick={handleGoLive}
                disabled={starting || (NEEDS_CUSTOM_PICKER && !picked)}
                className="inline-flex items-center gap-2 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-hover disabled:opacity-50 disabled:hover:bg-accent"
              >
                {starting ? (
                  <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-on-accent/30 border-t-on-accent" />
                ) : (
                  <Icon name="broadcast" size={15} />
                )}
                {starting ? "Starting…" : "Go Live"}
              </button>
            </div>
          </div>
        </div>

        {withPanel && (
          <section
            aria-labelledby="go-live-options-title"
            className={`pointer-events-auto flex w-84 shrink-0 flex-col overflow-hidden rounded-xl border border-border bg-bg-secondary shadow-float transition-[opacity,transform] duration-300 ${
              // Never taller than the Windows card beside it.
              split ? "max-h-[min(720px,calc(100vh-3rem))]" : "max-h-[calc(100vh-3rem)]"
            }`}
            style={{
              opacity: visible ? 1 : 0,
              // Settles in beside the dialog a moment after it opens.
              transform: visible ? "none" : "translateX(-8px)",
              transitionDelay: visible ? "80ms" : "0ms",
            }}
          >
            <div className="shrink-0 px-4 pt-4 pb-3">
              <h3
                id="go-live-options-title"
                className="font-display text-[14px] font-semibold text-text-bright"
              >
                Options
              </h3>
              <p className="mt-0.5 text-[12px] text-text-muted">
                Optional. The defaults suit most streams.
              </p>
            </div>
            <div className="min-h-0 overflow-y-auto border-t border-border-divider">
              <div className="divide-y divide-border-divider">{optionRows}</div>
              <div className="border-t border-border-divider">{advanced}</div>
            </div>
          </section>
        )}
      </div>
    </div>,
    document.body,
  );
}

/// "Lounge · Night Shift" under the title, or the call's peer.
function Destination({ serverId, channelId }: { serverId?: string; channelId?: string }) {
  const serverName = useChatStore((s) =>
    serverId ? s.servers.find((x) => x.id === serverId)?.name : undefined,
  );
  const channelName = useChatStore((s) =>
    serverId && channelId
      ? s.channelsByServer[serverId]?.find((c) => c.id === channelId)?.name
      : undefined,
  );
  const callPeer = useVoiceStore((s) => s.callPeer);
  const peerName = useUserName(callPeer, "");
  const text = serverId
    ? [channelName, serverName].filter(Boolean).join(" · ")
    : peerName
      ? `In your call with ${peerName}`
      : "";
  if (!text) return null;
  return (
    <p className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[12px] text-text-muted">
      <Icon name={serverId ? "speaker" : "user"} size={13} />
      <span className="truncate">{text}</span>
    </p>
  );
}

/// A screen with a window in front of it: what the system dialog offers.
function SystemPickerArt() {
  return (
    <svg width="56" height="44" viewBox="0 0 56 44" fill="none" aria-hidden="true" className="shrink-0">
      <rect x="1" y="1" width="42" height="28" rx="4" className="fill-bg-lighter stroke-text-muted" strokeWidth="1.5" />
      <path d="M15 37h14M22 29v8" className="stroke-text-muted" strokeWidth="1.5" strokeLinecap="round" />
      <rect x="22" y="12" width="33" height="23" rx="3.5" className="fill-bg-light stroke-accent" strokeWidth="1.5" />
      <path d="M22 18h33" className="stroke-accent" strokeWidth="1.5" />
      <circle cx="26" cy="15" r="1" className="fill-accent" />
      <circle cx="29.5" cy="15" r="1" className="fill-accent" />
    </svg>
  );
}
