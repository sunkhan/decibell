/// Presentation helpers shared by the server-settings tabs.

/// 0xRRGGBB → css color; 0 (no color) falls back to the muted token.
export function roleColor(color: number): string {
  if (!color) return "var(--color-text-muted)";
  return `#${color.toString(16).padStart(6, "0")}`;
}

export const ROLE_COLOR_PRESETS = [
  0, 0xe74c3c, 0xe67e22, 0xf1c40f, 0x2ecc71, 0x1abc9c, 0x3498db, 0x9b59b6,
  0xe91e63, 0x95a5a6,
];

export function formatJoined(epoch: number): string {
  if (!epoch) return "—";
  const d = new Date(epoch * 1000);
  return d.toLocaleDateString();
}
