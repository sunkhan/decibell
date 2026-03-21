const AVATAR_COLORS = [
  "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
  "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
];

export function stringToColor(str: string): string {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return AVATAR_COLORS[Math.abs(hash) % AVATAR_COLORS.length];
}
