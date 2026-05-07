import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { invoke } from "./lib/ipc";
import { probeDecoders } from "./utils/decoderProbe";
import { probeEncoders } from "./utils/encoderProbe";
import "./styles/globals.css";

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
