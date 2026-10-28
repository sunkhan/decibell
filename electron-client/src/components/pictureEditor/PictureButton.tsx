// The way in to the picture editor: the current picture as a button (click
// to choose a file) and a drop target (drop a picture on the card around it).

import { useRef, useState, type ReactNode } from "react";
import { PICTURE_ACCEPT, pictureFromList } from "./picture";
import { CameraIcon, PencilIcon } from "./icons";

/// Drag-and-drop of a picture file onto any element. Only file drags light
/// it up; counting enters and leaves keeps children from flickering it.
export function usePictureDrop(onPick: (file: File) => void, enabled = true) {
  const [over, setOver] = useState(false);
  const depth = useRef(0);
  const isFiles = (e: React.DragEvent) => enabled && e.dataTransfer.types.includes("Files");
  return {
    over,
    handlers: {
      onDragEnter: (e: React.DragEvent) => {
        if (!isFiles(e)) return;
        e.preventDefault();
        depth.current += 1;
        setOver(true);
      },
      onDragOver: (e: React.DragEvent) => {
        if (!isFiles(e)) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "copy";
      },
      onDragLeave: (e: React.DragEvent) => {
        if (!isFiles(e)) return;
        depth.current = Math.max(0, depth.current - 1);
        if (depth.current === 0) setOver(false);
      },
      onDrop: (e: React.DragEvent) => {
        if (!isFiles(e)) return;
        e.preventDefault();
        depth.current = 0;
        setOver(false);
        const f = pictureFromList(e.dataTransfer.files);
        if (f) onPick(f);
      },
    },
  };
}

/// Shown over the card while a picture is dragged over it.
export function DropVeil({ label }: { label: string }) {
  return (
    <div className="pointer-events-none absolute inset-0 z-10 flex animate-[fadeUp_0.15s_ease] items-center justify-center rounded-[inherit] border-2 border-dashed border-accent bg-bg-light/90 text-[13px] font-semibold text-text-primary">
      {label}
    </div>
  );
}

/// Clicking the picture opens the file dialog. The hover scrim and the
/// pencil badge say so; nothing else about a picture suggests a button.
export function PictureButton({
  onPick,
  radius,
  label,
  target,
  badge = false,
  disabled = false,
  children,
  inputRef,
}: {
  onPick: (file: File) => void;
  /// The picture's corner (the scrim follows it).
  radius: string;
  label: string;
  /// `data-picture-target`: where a saved picture lands.
  target: string;
  badge?: boolean;
  disabled?: boolean;
  children: ReactNode;
  /// For a separate "Upload" button to open the same dialog.
  inputRef?: React.RefObject<HTMLInputElement>;
}) {
  const own = useRef<HTMLInputElement>(null);
  const input = inputRef ?? own;
  if (disabled) {
    return (
      <div data-picture-target={target} className="relative flex shrink-0" style={{ borderRadius: radius }}>
        {children}
      </div>
    );
  }
  return (
    <>
      <button
        type="button"
        data-picture-target={target}
        onClick={() => input.current?.click()}
        aria-label={label}
        title={label}
        className="group relative flex shrink-0 cursor-pointer outline-none focus-visible:shadow-ring"
        style={{ borderRadius: radius }}
      >
        {children}
        <span
          className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-1 bg-black/55 text-[11px] font-semibold text-white opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-visible:opacity-100"
          style={{ borderRadius: radius }}
        >
          <CameraIcon />
          Change
        </span>
        {badge && (
          <span className="absolute -bottom-1 -right-1 flex size-6 items-center justify-center rounded-full bg-accent text-on-accent ring-3 ring-bg-light transition-transform duration-150 group-hover:scale-110">
            <PencilIcon />
          </span>
        )}
      </button>
      <input
        ref={input}
        type="file"
        accept={PICTURE_ACCEPT}
        className="hidden"
        onChange={(e) => {
          const f = pictureFromList(e.target.files);
          // Reset so picking the same file again still fires onChange.
          e.target.value = "";
          if (f) onPick(f);
        }}
      />
    </>
  );
}
