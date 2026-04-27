import React from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import "./styles/globals.css";
import { probeDecoders } from "./utils/decoderProbe";

// Probe WebCodecs decoder capabilities at boot and ship them to Rust
// so the next JoinVoiceRequest can advertise the merged caps to peers.
// Cached in localStorage; user can refresh via Settings → Codecs.
probeDecoders().then((decoderCaps) => {
  invoke("set_decoder_caps", { decoderCaps }).catch((e) =>
    console.warn("[caps] failed to ship decoder caps to Rust:", e)
  );
});

// ── Disable browser-default context menu ─────────────────────────────────────
// Allow right-click on text inputs/textareas (paste, etc.) but block everywhere
// else to prevent the WebView's "Back / Forward / Reload" menu.
document.addEventListener("contextmenu", (e) => {
  const tag = (e.target as HTMLElement).tagName;
  if (tag !== "INPUT" && tag !== "TEXTAREA") {
    e.preventDefault();
  }
});

// ── Disable browser back/forward navigation ──────────────────────────────────
// Mouse back/forward buttons (buttons 3 & 4) trigger WebView history navigation
// which logs the user out since there's no prior page. Block them globally.
window.addEventListener("mouseup", (e) => {
  if (e.button === 3 || e.button === 4) {
    e.preventDefault();
    e.stopPropagation();
  }
}, true);

// Alt+Left / Alt+Right and Backspace also trigger navigation in some WebViews.
document.addEventListener("keydown", (e) => {
  if (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
    e.preventDefault();
  }
  // Backspace outside an input/textarea can trigger back navigation
  if (e.key === "Backspace") {
    const tag = (e.target as HTMLElement).tagName;
    const editable = (e.target as HTMLElement).isContentEditable;
    if (tag !== "INPUT" && tag !== "TEXTAREA" && !editable) {
      e.preventDefault();
    }
  }
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
