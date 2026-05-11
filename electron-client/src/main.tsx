import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { invoke } from "./lib/ipc";
import { probeDecoders } from "./utils/decoderProbe";
import { probeEncoders } from "./utils/encoderProbe";
import { loadSettings } from "./features/settings/loadSettings";
import { flushSaveSettings } from "./features/settings/saveSettings";
import "./styles/globals.css";

// saveSettings is debounced (250ms trailing) so slider drags collapse
// to one disk write. Flush any pending save on window unload so the
// user's most recent tick isn't lost in the gap between change and
// persist. beforeunload fires before the renderer process tears down.
window.addEventListener("beforeunload", () => {
  flushSaveSettings();
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
probeDecoders().then((decoderCaps) => {
  invoke("set_decoder_caps", { decoderCaps }).catch((e) =>
    console.warn("[caps] failed to ship decoder caps to native:", e),
  );
});
probeEncoders().catch((e) =>
  console.warn("[caps] encoder probe failed:", e),
);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
