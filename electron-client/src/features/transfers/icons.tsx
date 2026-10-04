import type { AttachmentKind } from "../../types";

// Stroke glyphs for the Transfers panel (24-unit grid, currentColor).

function Svg({ size = 14, children }: { size?: number; children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  );
}

export const PauseIcon = () => (
  <Svg>
    <line x1="9" y1="5" x2="9" y2="19" />
    <line x1="15" y1="5" x2="15" y2="19" />
  </Svg>
);

export const PlayIcon = () => (
  <Svg>
    <polygon points="7 4 19 12 7 20 7 4" />
  </Svg>
);

export const CloseIcon = () => (
  <Svg>
    <line x1="18" y1="6" x2="6" y2="18" />
    <line x1="6" y1="6" x2="18" y2="18" />
  </Svg>
);

export const RetryIcon = () => (
  <Svg>
    <polyline points="1 4 1 10 7 10" />
    <path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" />
  </Svg>
);

export const FolderIcon = () => (
  <Svg>
    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
  </Svg>
);

export function KindIcon({ kind, size = 16 }: { kind: AttachmentKind; size?: number }) {
  if (kind === "image") {
    return (
      <Svg size={size}>
        <rect x="3" y="3" width="18" height="18" rx="2" />
        <circle cx="8.5" cy="8.5" r="1.5" />
        <polyline points="21 15 16 10 5 21" />
      </Svg>
    );
  }
  if (kind === "video") {
    return (
      <Svg size={size}>
        <rect x="2" y="5" width="15" height="14" rx="2" />
        <polygon points="23 7 17 12 23 17 23 7" />
      </Svg>
    );
  }
  if (kind === "audio") {
    return (
      <Svg size={size}>
        <path d="M9 18V5l12-2v13" />
        <circle cx="6" cy="18" r="3" />
        <circle cx="18" cy="16" r="3" />
      </Svg>
    );
  }
  return (
    <Svg size={size}>
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
    </Svg>
  );
}
