// The viewer's glyphs: 24-unit strokes, the same weight as the chat's.

function Svg({ size = 18, children }: { size?: number; children: React.ReactNode }) {
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

export const ZoomOutIcon = () => (
  <Svg>
    <circle cx="11" cy="11" r="7" />
    <line x1="8" y1="11" x2="14" y2="11" />
    <line x1="20.5" y1="20.5" x2="16" y2="16" />
  </Svg>
);

export const ZoomInIcon = () => (
  <Svg>
    <circle cx="11" cy="11" r="7" />
    <line x1="8" y1="11" x2="14" y2="11" />
    <line x1="11" y1="8" x2="11" y2="14" />
    <line x1="20.5" y1="20.5" x2="16" y2="16" />
  </Svg>
);

export const RotateIcon = () => (
  <Svg>
    <path d="M20 11a8 8 0 1 0-2.4 5.7" />
    <polyline points="20 4 20 11 13 11" />
  </Svg>
);

export const CopyIcon = () => (
  <Svg>
    <rect x="9" y="9" width="12" height="12" rx="2" />
    <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
  </Svg>
);

export const DownloadIcon = () => (
  <Svg>
    <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
    <polyline points="7 10 12 15 17 10" />
    <line x1="12" y1="15" x2="12" y2="3" />
  </Svg>
);

export const ExternalIcon = () => (
  <Svg>
    <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
    <polyline points="15 3 21 3 21 9" />
    <line x1="10" y1="14" x2="21" y2="3" />
  </Svg>
);

export const LinkIcon = ({ size = 18 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7" />
    <path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7" />
  </Svg>
);

export const CloseIcon = () => (
  <Svg size={20}>
    <line x1="6" y1="6" x2="18" y2="18" />
    <line x1="18" y1="6" x2="6" y2="18" />
  </Svg>
);

export const ChevronIcon = ({ dir }: { dir: "left" | "right" }) => (
  <Svg size={22}>
    <polyline points={dir === "left" ? "15 5 8 12 15 19" : "9 5 16 12 9 19"} />
  </Svg>
);

export const FitIcon = () => (
  <Svg>
    <polyline points="4 9 4 4 9 4" />
    <polyline points="20 9 20 4 15 4" />
    <polyline points="4 15 4 20 9 20" />
    <polyline points="20 15 20 20 15 20" />
  </Svg>
);

export function Spinner() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden className="animate-spin">
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeOpacity="0.25" strokeWidth="3" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
    </svg>
  );
}
