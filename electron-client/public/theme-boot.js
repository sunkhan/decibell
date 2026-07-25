// Pre-mount theme application.
//
// The store of record for the selected theme is the native config
// blob, but reading it costs an async IPC round-trip that lands well
// after first paint. Without this, a user on one of the light themes
// sees a full-window flash of graphite dark on every cold start.
//
// So setTheme() mirrors its choice into localStorage and this script —
// loaded synchronously in <head>, before the stylesheet paints — reads
// the mirror back. loadSettings() still re-applies the authoritative
// value from disk a moment later; the two agree in every case except a
// config edited outside the app, where the mirror loses.
//
// This is a separate file rather than an inline <script> because
// index.html's CSP is `script-src 'self'` with no 'unsafe-inline'.
// Keep the storage key in sync with THEME_STORAGE_KEY in
// src/stores/uiStore.ts.
(function () {
  var THEMES = ["graphite", "graphite-light", "console", "console-light", "console-split"];
  var theme = "graphite";
  try {
    var stored = localStorage.getItem("decibell.theme");
    if (stored && THEMES.indexOf(stored) !== -1) theme = stored;
  } catch (e) {
    // Storage unavailable — fall through to the default.
  }
  document.documentElement.dataset.theme = theme;

  // Same story for the appearance scales: applying them after mount
  // would reflow the entire tree one frame in. Keys must match
  // TEXT_SIZE_STORAGE_KEY / ROW_SCALE_STORAGE_KEY in uiStore.
  //
  // Note this only ever writes the raw numbers — globals.css turns the
  // body size into a scale against whichever palette is active, so this
  // script needs no knowledge of either theme's metrics.
  function num(key, min, max) {
    try {
      var n = parseFloat(localStorage.getItem(key));
      if (n >= min && n <= max) return n;
    } catch (e) {
      // Storage unavailable — fall through to the default.
    }
    return 0;
  }
  var root = document.documentElement;
  // Left unset when nothing is stored, so globals.css's
  // --ui-text-size-default supplies the shipped default.
  var textSize = num("decibell.textSizePx", 11, 17);
  if (textSize > 0) root.style.setProperty("--ui-text-size-n", String(textSize));
  root.style.setProperty("--ui-row-scale", String(num("decibell.rowScale", 0.85, 1.3) || 1));
})();
