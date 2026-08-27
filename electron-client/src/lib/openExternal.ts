// Hand an http(s) URL to the OS browser. The renderer never navigates
// itself (hardenNavigation blocks off-origin top-frame navigation),
// so every link click routes through this. Main re-validates the
// scheme; this early check just keeps obviously wrong hrefs local.
export function openExternal(url: string): void {
  if (!/^https?:\/\//i.test(url)) return;
  void window.decibell.shell.openExternal(url).catch((e) => {
    console.warn("[openExternal] failed:", e);
  });
}

/// Anchor click handler: left click opens externally instead of
/// navigating; modifier clicks do the same (no tabs to open here).
export function onLinkClick(e: React.MouseEvent<HTMLAnchorElement>): void {
  e.preventDefault();
  openExternal(e.currentTarget.href);
}

/// Middle click, which browsers route to auxclick rather than click.
export function onLinkAuxClick(e: React.MouseEvent<HTMLAnchorElement>): void {
  if (e.button !== 1) return;
  e.preventDefault();
  openExternal(e.currentTarget.href);
}
