import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { invoke } from "./lib/ipc";
import { probeDecoders } from "./utils/decoderProbe";
import { probeEncoders } from "./utils/encoderProbe";
import { loadSettings } from "./features/settings/loadSettings";
import { flushSaveSettings } from "./features/settings/saveSettings";
import { endCall } from "./features/call/callActions";
import { useCodecSettingsStore } from "./stores/codecSettingsStore";
import { initRendererSentry } from "./lib/sentry";
import "./styles/globals.css";

// Initialize Sentry FIRST, before any other boot work. Any throw
// inside loadSettings, probeDecoders, the React mount itself, etc.,
// is then captured by the SDK. Boot order matters: Sentry init must
// be the first executable statement that runs.
initRendererSentry();

// saveSettings is debounced (250ms trailing) so slider drags collapse
// to one disk write. Flush any pending save on window unload so the
// user's most recent tick isn't lost in the gap between change and
// persist. beforeunload fires before the renderer process tears down.
window.addEventListener("beforeunload", () => {
  flushSaveSettings();
  // A DM call in progress: tell the peer (best effort — native shutdown()
  // sends the same HANGUP on app quit; this covers a bare window reload).
  void endCall(null);
});

// Hydrate persisted settings + auto-login (if credentials saved)
// before the React tree mounts. Fire-and-forget — if it fails the
// user lands on the login screen with in-store defaults.
loadSettings().catch((e) =>
  console.warn("[boot] loadSettings failed:", e),
);

// Probe WebCodecs decoder + encoder capabilities at boot and ship them
// to native so JoinVoiceRequest advertises the merged caps to peers.
// PR8: encoder probe runs renderer-side now (Chromium WebCodecs.VideoEncoder)
// instead of native FFmpeg. Both cached in localStorage; user can refresh
// via Settings → Codecs.
//
// Once both probes have shipped, hydrate codecSettingsStore from native.
// The store used to be loaded only when the Settings → Codecs tab
// mounted, but VoicePanel/UserProfilePopup gate stream-watching on its
// decodeCaps — so on a fresh launch every stream showed as locked
// ("hardware doesn't support this codec") until the user happened to
// open that tab. Sequencing matters: load() reads caps back from
// native, so it must run after both probes have shipped theirs.
Promise.allSettled([
  probeDecoders().then((decoderCaps) =>
    invoke("set_decoder_caps", { decoderCaps }),
  ),
  probeEncoders(),
]).then((results) => {
  for (const r of results) {
    if (r.status === "rejected") {
      console.warn("[caps] boot probe/ship failed:", r.reason);
    }
  }
  useCodecSettingsStore
    .getState()
    .load()
    .catch((e) => console.warn("[caps] codec settings load failed:", e));
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
