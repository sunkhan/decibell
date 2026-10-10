/// Mouse back / forward buttons: Chromium walks the page's history on
/// their release unless the page cancels it — and that happens in the
/// renderer, so main's `app-command` guard never sees it. Decibell is one
/// state-driven page (the router only switches between login and the
/// app), so there is nothing to walk to; "back" used to land a signed-in
/// user on the login screen. Cancel the release. The buttons still reach
/// every listener (the keybind recorder, the focused hotkey fallback).
export function installMouseNavGuard(): void {
  window.addEventListener(
    "mouseup",
    (e) => {
      if (e.button === 3 || e.button === 4) e.preventDefault();
    },
    true,
  );
}
