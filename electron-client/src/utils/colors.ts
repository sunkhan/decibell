const AVATAR_COLORS = [
  "#4596ff", "#f0883e", "#a371f7", "#3fb950",
  "#d29922", "#f85149", "#e879f9", "#79c0ff",
];

const GRADIENT_PAIRS: [string, string][] = [
  ["#4596ff", "#2569cd"],
  ["#f0883e", "#da6d25"],
  ["#a371f7", "#8957e5"],
  ["#3fb950", "#238636"],
  ["#d29922", "#b37a15"],
  ["#f85149", "#da3633"],
  ["#e879f9", "#c840e0"],
  ["#79c0ff", "#4596ff"],
];

export function stringToColor(str: string): string {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return AVATAR_COLORS[Math.abs(hash) % AVATAR_COLORS.length];
}

export function stringToGradient(str: string): string {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  const [from, to] = GRADIENT_PAIRS[Math.abs(hash) % GRADIENT_PAIRS.length];
  return `linear-gradient(135deg, ${from}, ${to})`;
}
