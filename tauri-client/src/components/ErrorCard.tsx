import type { ReactNode } from "react";

interface ErrorCardProps {
  icon?: ReactNode;
  children: ReactNode;
}

const defaultIcon = (
  <svg
    className="h-4 w-4 text-warning"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
    <line x1="12" y1="9" x2="12" y2="13" />
    <line x1="12" y1="17" x2="12.01" y2="17" />
  </svg>
);

export default function ErrorCard({ icon, children }: ErrorCardProps) {
  return (
    <div className="flex max-w-[520px] items-start gap-3 rounded-[10px] border border-warning/10 bg-warning/[0.06] px-4 py-3.5 animate-[fadeUp_0.4s_ease]">
      <div className="mt-px flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-warning/[0.12]">
        {icon ?? defaultIcon}
      </div>
      <p className="text-[13px] leading-relaxed text-text-secondary">
        {children}
      </p>
    </div>
  );
}
