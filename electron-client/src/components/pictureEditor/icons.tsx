// The picture editor's glyphs: 24-unit strokes, the chat's weight.

function Svg({ size = 16, children }: { size?: number; children: React.ReactNode }) {
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

export const ResetIcon = () => (
  <Svg size={14}>
    <path d="M3 12a9 9 0 1 0 3-6.7" />
    <polyline points="3 4 3 9 8 9" />
  </Svg>
);

export const ImageIcon = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <rect x="3" y="4" width="18" height="16" rx="2.5" />
    <circle cx="9" cy="10" r="1.8" />
    <path d="M21 16l-5-5-9 9" />
  </Svg>
);

export const CameraIcon = ({ size = 18 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M4 8.5A1.5 1.5 0 0 1 5.5 7h2l1.5-2h6l1.5 2h2A1.5 1.5 0 0 1 20 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 17.5z" />
    <circle cx="12" cy="13" r="3.2" />
  </Svg>
);

export const PencilIcon = ({ size = 12 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M4 20h4L19 9a2.8 2.8 0 0 0-4-4L4 16z" />
  </Svg>
);

export const AlertIcon = () => (
  <Svg size={22}>
    <circle cx="12" cy="12" r="9" />
    <line x1="12" y1="7.5" x2="12" y2="12.5" />
    <line x1="12" y1="16.2" x2="12.01" y2="16.2" />
  </Svg>
);
